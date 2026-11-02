//! Checkpoint/list adapter. File IO owns a worker receipt until the UI consumes it.
use super::*;
use crate::integrations::{
    lists::{self, ModListDocument},
    profiles::ProfileLibrary,
};

#[derive(Default)]
pub(super) struct ProfileState {
    pub seed: Option<Vec<String>>,
    pub busy: bool,
    pub delete: Option<(String, u64)>,
    serial: u64,
    library: Option<ProfileLibrary>,
    selected: Option<u64>,
    revision: usize,
    note: String,
    receipt: Arc<Mutex<Option<Receipt>>>,
}
struct Receipt {
    library: Option<ProfileLibrary>,
    result: Result<Outcome, String>,
}
enum Outcome {
    Saved(Option<u64>),
    Imported {
        document: ModListDocument,
        append: bool,
        source: JobSource,
        path: PathBuf,
    },
    Exported,
}
enum Task {
    Open,
    Create(String, Vec<String>),
    Save(u64, Vec<String>),
    Rename(u64, String),
    Duplicate(u64, String),
    Delete(u64),
    Import(PathBuf, bool, JobSource),
    Export(PathBuf, PathBuf, ModListDocument, bool),
}

pub(super) fn register(state: &Arc<State>, window: &ProfilesWindow) {
    let s = state.clone();
    window.on_select_profile(move |id| {
        edit(&s, |app| {
            if app.profiles.busy || app.profiles.delete.is_some() {
                return false;
            }
            if let Ok(id) = id.parse::<u64>()
                && app
                    .profiles
                    .library
                    .as_ref()
                    .is_some_and(|l| l.profiles().iter().any(|p| p.id == id))
            {
                app.profiles.selected = Some(id);
                app.profiles.revision = 0;
            }
            true
        });
        paint_profiles(&s);
    });
    let s = state.clone();
    window.on_select_revision(move |index| {
        edit(&s, |app| {
            if !app.profiles.busy
                && index >= 0
                && selected(app).is_some_and(|p| (index as usize) < p.revisions.len())
            {
                app.profiles.revision = index as usize;
            }
            true
        });
        paint_profiles(&s);
    });
    let s = state.clone();
    window.on_action(move |action, name| action_request(&s, &action, &name));
    let s = state.clone();
    window.on_list_action(move |action, path| list_request(&s, &action, &path));
    let s = state.clone();
    window.on_answer_delete(move |token, confirmed| {
        let id = {
            let mut app = s.app.lock().expect("state");
            if app
                .profiles
                .delete
                .as_ref()
                .is_none_or(|(t, _)| t.as_str() != token.as_str())
            {
                return;
            }
            let (_, id) = app.profiles.delete.take().expect("checked question");
            id
        };
        if confirmed {
            start(&s, Task::Delete(id));
        } else {
            paint_profiles(&s);
        }
    });
    let s = state.clone();
    window.on_close_window(move || {
        if !s.app.lock().expect("state").profiles.busy
            && let Some(window) = s.profiles_window.upgrade()
        {
            let _ = window.hide();
        }
    });
    let s = state.clone();
    window.window().on_close_requested(move || {
        if s.app.lock().expect("state").profiles.busy {
            slint::CloseRequestResponse::KeepWindowShown
        } else {
            slint::CloseRequestResponse::HideWindow
        }
    });
}
fn selected(app: &App) -> Option<&crate::integrations::profiles::Profile> {
    app.profiles
        .library
        .as_ref()?
        .profiles()
        .iter()
        .find(|p| Some(p.id) == app.profiles.selected)
}
fn permitted(app: &App) -> bool {
    app.reset.is_none() && reset_allowed(app) && !app.profiles.busy
}
pub(super) fn open(state: &Arc<State>) {
    if let Some(window) = state.profiles_window.upgrade() {
        let _ = window.show();
    }
    let needs_open = state.app.lock().expect("state").profiles.library.is_none();
    if needs_open {
        start(state, Task::Open);
    }
    paint_profiles(state);
}
fn action_request(state: &Arc<State>, action: &str, name: &str) {
    let task = {
        let mut app = state.app.lock().expect("state");
        if !permitted(&app) {
            app.profiles.note =
                "Answer the pending question or wait for the operation first".into();
            drop(app);
            paint_profiles(state);
            return;
        }
        let tokens = app
            .session
            .as_ref()
            .map(Session::working_tokens)
            .unwrap_or_default();
        let id = app.profiles.selected.unwrap_or(0);
        match action {
            "create" => Some(Task::Create(name.into(), tokens)),
            "save" => Some(Task::Save(id, tokens)),
            "rename" => Some(Task::Rename(id, name.into())),
            "duplicate" => Some(Task::Duplicate(id, name.into())),
            "reload" => Some(Task::Open),
            "delete" => {
                app.profiles.serial += 1;
                app.profiles.delete = Some((format!("delete-{}", app.profiles.serial), id));
                None
            }
            "load" => {
                let candidate = selected(&app).and_then(|p| {
                    p.revisions.get(app.profiles.revision).map(|r| {
                        (
                            r.tokens.clone(),
                            format!("profile {} revision {}", p.name, app.profiles.revision + 1),
                        )
                    })
                });
                drop(app);
                if let Some((tokens, source)) = candidate {
                    replace(state, tokens, source);
                }
                return;
            }
            _ => {
                app.profiles.note = "Unknown checkpoint action".into();
                None
            }
        }
    };
    if let Some(task) = task {
        start(state, task);
    }
    paint_profiles(state);
}
fn list_request(state: &Arc<State>, action: &str, path: &str) {
    let task = {
        let mut app = state.app.lock().expect("state");
        if !permitted(&app) {
            app.profiles.note =
                "Answer the pending question or wait for the operation first".into();
            drop(app);
            paint_profiles(state);
            return;
        }
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            app.profiles.note = "Choose an absolute file path".into();
            drop(app);
            paint_profiles(state);
            return;
        }
        let Some(session) = app.session.as_ref() else {
            return;
        };
        match action {
            "append" | "replace" => Task::Import(path, action == "append", JobSource::of(session)),
            "export-json" | "export-xml" => {
                let mut document = match lists::parse_list(&session.loaded_config().raw) {
                    Ok(doc) => doc,
                    Err(e) => {
                        app.profiles.note = e.to_string();
                        drop(app);
                        paint_profiles(state);
                        return;
                    }
                };
                document.tokens = session.working_tokens();
                Task::Export(
                    path,
                    session.config_path().into(),
                    document,
                    action == "export-json",
                )
            }
            _ => return,
        }
    };
    start(state, task);
}
fn replace(state: &Arc<State>, tokens: Vec<String>, source: String) {
    let confirm = {
        let app = state.app.lock().expect("state");
        if !permitted(&app) {
            return;
        }
        app.draft.confirm_warnings
    };
    reduce(
        state,
        vec![WorkflowEvent::ReplaceRequested {
            tokens,
            source,
            confirm_warnings: confirm,
        }],
    );
}
fn start(state: &Arc<State>, task: Task) {
    let input = {
        let mut app = state.app.lock().expect("state");
        if !permitted(&app) {
            app.profiles.note =
                "Operation unavailable while another question or operation is pending".into();
            None
        } else if let (Some(root), Some(seed)) = (
            app.settings_file
                .as_ref()
                .and_then(|p| p.parent())
                .map(PathBuf::from),
            app.profiles.seed.clone(),
        ) {
            app.profiles.busy = true;
            Some((
                root.join("profiles.json"),
                seed,
                app.profiles.library.take(),
                app.profiles.receipt.clone(),
            ))
        } else {
            app.profiles.note =
                "Load a game configuration and configure an application data directory first"
                    .into();
            None
        }
    };
    paint_profiles(state);
    let Some((path, seed, mut library, receipt)) = input else {
        return;
    };
    let state = state.clone();
    std::thread::spawn(move || {
        let result = (|| {
            if library.is_none() || matches!(task, Task::Open) {
                library = Some(ProfileLibrary::open(&path, &seed).map_err(|e| e.to_string())?);
            }
            let l = library.as_mut().expect("opened library");
            let result = match task {
                Task::Open => Ok(Outcome::Saved(None)),
                Task::Create(name, tokens) => {
                    l.create(&name, &tokens).map(|id| Outcome::Saved(Some(id)))
                }
                Task::Save(id, tokens) => l
                    .save_revision(id, &tokens, "Explicit profile save")
                    .map(|()| Outcome::Saved(None)),
                Task::Rename(id, name) => l.rename(id, &name).map(|()| Outcome::Saved(None)),
                Task::Duplicate(id, name) => {
                    l.duplicate(id, &name).map(|id| Outcome::Saved(Some(id)))
                }
                Task::Delete(id) => l.delete(id).map(|()| Outcome::Saved(None)),
                Task::Import(path, append, source) => {
                    return lists::read_list(&path)
                        .map(|document| Outcome::Imported {
                            document,
                            append,
                            source,
                            path,
                        })
                        .map_err(|e| e.to_string());
                }
                Task::Export(path, config, doc, json) => {
                    return lists::export_file(&path, &config, &doc, json)
                        .map(|()| Outcome::Exported)
                        .map_err(|e| e.to_string());
                }
            };
            result.map_err(|e| e.to_string())
        })();
        *receipt.lock().expect("receipt") = Some(Receipt { library, result });
        // The receipt lives in State, independently of the secondary window's visibility.
        let _ = slint::invoke_from_event_loop(move || state_wake(&state));
    });
}
pub(super) fn deliver(state: &Arc<State>) {
    let receipt = {
        let app = state.app.lock().expect("state");
        app.profiles.receipt.lock().expect("receipt").take()
    };
    let Some(receipt) = receipt else {
        return;
    };
    let mut imported = None;
    {
        let mut app = state.app.lock().expect("state");
        app.profiles.busy = false;
        app.profiles.library = receipt.library;
        match receipt.result {
            Ok(Outcome::Saved(id)) => {
                if let Some(id) = id {
                    app.profiles.selected = Some(id);
                    app.profiles.revision = 0;
                }
                if selected(&app).is_none() {
                    app.profiles.selected = app
                        .profiles
                        .library
                        .as_ref()
                        .and_then(|l| l.profiles().first())
                        .map(|p| p.id);
                    app.profiles.revision = 0;
                }
                app.profiles.note = "Checkpoint library persisted; game config unchanged".into();
            }
            Ok(Outcome::Exported) => {
                app.profiles.note = "List exported atomically; game config unchanged".into()
            }
            Ok(Outcome::Imported {
                document,
                append,
                source,
                path,
            }) => {
                if app.source() == Some(source) && permitted(&app) {
                    imported = Some((document, append, path));
                } else {
                    app.profiles.note =
                        "List changed while reading; import not applied. Retry explicitly.".into();
                }
            }
            Err(e) => {
                app.profiles.note = e;
            }
        }
    }
    if let Some((document, append, path)) = imported {
        let mismatch = {
            let app = state.app.lock().expect("state");
            let current = app
                .session
                .as_ref()
                .and_then(|s| s.loaded_config().version.as_ref());
            document
                .game_version
                .as_ref()
                .is_some_and(|v| Some(v) != current)
        };
        let source = format!(
            "import {}{}",
            path.display(),
            if mismatch {
                " (game version mismatch)"
            } else {
                ""
            }
        );
        if append {
            edit(state, |app| {
                if !permitted(app) {
                    return false;
                }
                let session = app.session.as_mut().expect("loaded session");
                let tokens = lists::append_unique(&session.working_tokens(), &document.tokens);
                let _ = session.replace_tokens(&tokens);
                let _ = app.jobs.cancel(JobSlot::Sort);
                app.selected = None;
                app.status = source.clone();
                true
            });
        } else {
            replace(state, document.tokens, source.clone());
        }
        state.app.lock().expect("state").profiles.note = source;
    }
    paint_profiles(state);
}
fn paint_profiles(state: &Arc<State>) {
    let Some(window) = state.profiles_window.upgrade() else {
        return;
    };
    let app = state.app.lock().expect("state");
    let rows = app
        .profiles
        .library
        .as_ref()
        .map(|l| {
            l.profiles()
                .iter()
                .map(|p| ProfileRow {
                    id: p.id.to_string().into(),
                    name: p.name.clone().into(),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let history = selected(&app)
        .map(|p| {
            p.revisions
                .iter()
                .map(|r| {
                    format!(
                        "{} — {} ({} tokens)",
                        i64::try_from(r.timestamp)
                            .ok()
                            .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0))
                            .map(|date| date.format("%Y-%m-%d %H:%M:%S UTC").to_string())
                            .unwrap_or_else(|| format!("UTC epoch {}", r.timestamp)),
                        r.reason,
                        r.tokens.len()
                    )
                    .into()
                })
                .collect::<Vec<slint::SharedString>>()
        })
        .unwrap_or_default();
    window.set_profiles(ModelRc::new(VecModel::from(rows)));
    window.set_history(ModelRc::new(VecModel::from(history)));
    window.set_selected_id(
        app.profiles
            .selected
            .map(|id| id.to_string())
            .unwrap_or_default()
            .into(),
    );
    window.set_selected_revision(app.profiles.revision as i32);
    window.set_busy(app.profiles.busy);
    window.set_note(app.profiles.note.clone().into());
    window.set_delete_token(
        app.profiles
            .delete
            .as_ref()
            .map(|(t, _)| t.clone())
            .unwrap_or_default()
            .into(),
    );
}
