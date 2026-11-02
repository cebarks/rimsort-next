//! The generated workspace window and the adapter that answers its controls.
//!
//! The window types are generated once, here in the library, so a test binds the very
//! same callbacks, properties and model wiring the binary runs — the binary is only a shell
//! that opens the window, asks for its first read and runs the event loop.
//!
//! # How a result reaches the screen
//!
//! A worker never touches a window. [`Workspace::begin`] installs one coalesced notice on
//! the job host; a worker that posts something invokes it with no host lock held, and the
//! notice's only job is to schedule [`Workspace::deliver`] onto the Slint event loop with
//! [`slint::invoke_from_event_loop`]. Delivery therefore happens on the UI thread, through a
//! weak handle, and a window that is gone simply is not upgraded.
//!
//! [`deliver`] takes the pending notice *before* draining, which is what makes the
//! coalescing safe: a post that lands during a drain re-arms the notice and knocks again.
//! The result then passes the controller's own gates — the slot's active job and the session
//! state it was bound to — before anything is applied, and a refusal or a superseded result
//! changes nothing but the status line.
//!
//! # What the adapter is allowed to touch
//!
//! Every list change goes through the accepted [`Session`] verbs, and a load order computed by
//! a job goes through [`apply_sort_order`], which refuses anything the sorter could not have
//! produced. IO is dispatched through the workflow and job host; callbacks enforce operation
//! guards as well as the visible controls. Reset changes only the in-memory working list.

slint::include_modules!();
#[path = "profiles.rs"]
mod profiles;

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};

use crate::application::jobs::{
    Delivered, JobCompletion, JobHost, JobId, JobKind, JobOutcome, JobOwner, JobSource, JobStage,
    JobWork, ModListSnapshot, SortApplied, apply_sort_order,
};
use crate::application::session::Session;
use crate::application::workflow::{
    LaunchOutcome, OperationFailure, OperationId, PromptAnswer, PromptId, RefreshOutcome,
    RefreshReplacement, RefreshTarget, SaveOutcome, Workflow, WorkflowEffect, WorkflowEvent,
};
use crate::domain::catalog::Catalog;
use crate::domain::types::{GameVersion, ModId};
use crate::integrations::launch::Platform;
use crate::integrations::settings::{Settings, load_settings, save_settings, settings_path};
use crate::ui::bridge::{ComputeJobs, JobSlot, Projection, Routed, drain, install_wakeup};
use slint::{ComponentHandle as _, ModelRc, VecModel, Weak};

/// What the window starts from: one captured candidate, the directory relative launch paths
/// resolve against, and the platform the game runs on.
#[derive(Clone)]
pub struct Startup {
    pub target: RefreshTarget,
    /// The manager's working directory, captured once by the caller. `None` means it could
    /// not be read: relative launch paths then have no honest basis, so nothing is guessed
    /// and the operations that need it are refused.
    pub basis: Option<PathBuf>,
    pub platform: Platform,
    /// Where the settings service reads and writes. `None` asks the service for its own
    /// path; a test passes a fixture file instead of mutating process-global directories.
    pub settings_file: Option<PathBuf>,
}

/// What a window should start from, before anything is read.
#[derive(Clone, Default)]
pub struct Options {
    pub settings_file: Option<PathBuf>,
    pub basis: Option<PathBuf>,
    pub platform: Option<Platform>,
}

/// A painted frame: everything the window shows, as owned data.
///
/// The view is computed while the state lock is held and applied after it is released, so no
/// Slint callback is ever entered from inside the lock.
struct View {
    inactive: Vec<Row>,
    active: Vec<Row>,
    inspector: Inspector,
    validation: Vec<slint::SharedString>,
    reset_token: String,
    reset_enabled: bool,
    selected: String,
    dirty: bool,
    read_only: bool,
    busy_refresh: bool,
    busy_sort: bool,
    busy_save: bool,
    busy_launch: bool,
    prompt: Option<Prompt>,
    status: String,
    progress: String,
    note: String,
    draft: DraftView,
    copy: Option<CopyQuestion>,
}

/// What the window's selection refers to, in the session's own terms.
///
/// A row key cannot be kept as the selection: keys belong to the projection that issued them,
/// and a repaint has to be able to answer "is that still the row on screen". So the selection
/// is the exact occurrence instead - an underlying active-list position, or the physical copy
/// an available row was showing - together with the session generation it belongs to. Nothing
/// here is guessed from a package id, and a repeated occurrence stays the specific one it was.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Selection {
    Active {
        generation: u64,
        revision: u64,
        position: usize,
    },
    Inactive {
        generation: u64,
        mod_id: ModId,
    },
}

impl Selection {
    /// The session this occurrence was read out of; a newer installation means the position
    /// or copy no longer means what it did.
    /// The list state the tracked position was read from. An edit the window did not perform
    /// itself - a sort landing, a completion publishing a different order - moves positions it
    /// cannot account for, and then the selection has to go rather than follow the number.
    fn tracked(&self, session: &crate::application::session::Session) -> bool {
        match self {
            Self::Active {
                generation,
                revision,
                ..
            } => *generation == session.generation() && *revision == session.revision(),
            Self::Inactive { generation, .. } => *generation == session.generation(),
        }
    }

    fn set_revision(&mut self, revision: u64) {
        if let Self::Active { revision: slot, .. } = self {
            *slot = revision;
        }
    }

    /// Remaps the tracked position through a row that moved from one position to another.
    fn after_move(&mut self, from: usize, to: usize) {
        if let Self::Active { position, .. } = self {
            *position = if *position == from {
                to
            } else if from < to && *position > from && *position <= to {
                *position - 1
            } else if to < from && *position >= to && *position < from {
                *position + 1
            } else {
                *position
            };
        }
    }

    /// Remaps the tracked position through the removal of `gone`: the selected row itself
    /// leaves the list, rows behind it shift up, rows ahead are untouched.
    fn after_remove(&mut self, gone: usize) -> bool {
        match self {
            Self::Active { position, .. } if *position == gone => false,
            Self::Active { position, .. } => {
                if *position > gone {
                    *position -= 1;
                }
                true
            }
            Self::Inactive { .. } => true,
        }
    }
}

/// The form's fields as text, with empty meaning "not set" rather than a guess.
#[derive(Clone, Default)]
pub struct DraftView {
    pub game_root: String,
    pub content_root: String,
    pub config_path: String,
    pub local_mods_root: String,
    pub workshop_root: String,
    pub target_version: String,
    pub executable_override: String,
    pub confirm_warnings: bool,
}

/// The outstanding workflow question: the opaque prompt and operation identities, the
/// answers that question accepts, and the token the window hands back with an answer.
/// The question the window is showing, together with the identities it was asked under.
///
/// Only `token` and the text reach the window; the opaque prompt and operation identities stay
/// here, so an answer is recorded under the id the reducer issued and never one the UI made up.
#[derive(Clone, Debug)]
pub struct Prompt {
    /// The opaque label an answer has to match: a question that has been replaced stops
    /// answering, even when the same choice text arrives.
    pub token: String,
    pub title: String,
    pub detail: String,
    pub allow_save: bool,
    pub allow_discard: bool,
    pub allow_confirm: bool,
    id: PromptId,
}

/// The window's own copy question: which row, which copy it holds now, which copy the user
/// asked it to use, and the session state all of that was read from.
///
/// The nonce is private to this window and is not a workflow identity: it exists so an answer
/// can only ever be applied to the request it was given for.
#[derive(Clone, Debug)]
pub struct CopyQuestion {
    pub nonce: String,
    pub title: String,
    pub detail: String,
    generation: u64,
    revision: u64,
    position: usize,
    wanted: ModId,
}

/// Which request a question or a completion belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestKind {
    Save,
    Launch,
    Close,
    Refresh,
}

#[derive(Default)]
struct Inspector {
    title: String,
    source: String,
    package: String,
    version: String,
    description: String,
    diagnostics: String,
}

/// The loaded installation and everything the window derives from it.
///
/// A mutex rather than a `RefCell`, because the same state is reached from a worker's
/// coalesced notice as well as from UI callbacks.
struct App {
    session: Option<Session>,
    workflow: Option<Workflow>,
    /// The candidate that was read and the settings the loaded session belongs to. A form
    /// edit never redirects them; only an explicit refresh asks another candidate.
    target: RefreshTarget,
    /// The form as edited. Nothing here is installed, saved or used to redirect an
    /// operation that belongs to the loaded installation.
    draft: DraftView,
    note: String,
    basis: Option<PathBuf>,
    settings_file: Option<PathBuf>,
    platform: Platform,
    jobs: ComputeJobs,
    projection: Projection,
    bootstrap: Option<JobId>,
    /// The question the window is showing, with the identities it was asked under.
    prompt: Option<Prompt>,
    /// The copy question the window is asking, if any.
    copy: Option<CopyQuestion>,
    copy_serial: u64,
    reset: Option<(String, JobSource)>,
    reset_serial: u64,
    profiles: profiles::ProfileState,
    /// The job behind each outstanding operation, with the identity its result has to be fed
    /// back to. A fact's job is never released by a scan cancellation.
    save: Option<(JobId, OperationId)>,
    launch: Option<(JobId, OperationId)>,
    refresh: Option<(JobId, OperationId)>,
    /// Set only by the reducer's `Close` effect; it is what lets a native close through.
    close_effect: bool,
    /// A close request is outstanding: the workflow asked something first, so the `Close`
    /// that eventually answers it is what takes the window away.
    closing: bool,
    selected: Option<Selection>,
    /// The inputs the current projection was built from, so a frame that changes neither the
    /// session state nor a search box can reuse the keys it already handed out.
    projection_inputs: Option<(JobSource, String, String)>,
    inactive_search: String,
    active_search: String,
    status: String,
    progress: String,
    reported: bool,
}

impl App {
    fn new(startup: &Startup) -> Self {
        Self {
            session: None,
            workflow: None,
            target: startup.target.clone(),
            draft: draft_of(&startup.target.settings),
            note: String::new(),
            basis: startup.basis.clone(),
            settings_file: startup.settings_file.clone(),
            platform: startup.platform,
            jobs: ComputeJobs::new(),
            projection: Projection::default(),
            bootstrap: None,
            prompt: None,
            copy: None,
            copy_serial: 0,
            reset: None,
            reset_serial: 0,
            profiles: profiles::ProfileState::default(),
            save: None,
            launch: None,
            refresh: None,
            close_effect: false,
            closing: false,
            selected: None,
            projection_inputs: None,
            inactive_search: String::new(),
            active_search: String::new(),
            status: "reading the installation...".to_string(),
            progress: String::new(),
            reported: false,
        }
    }

    fn source(&self) -> Option<JobSource> {
        self.session.as_ref().map(JobSource::of)
    }

    /// Rebuilds both panes from the session as it stands, then reads the frame off it.
    fn view(&mut self) -> View {
        let (inactive_search, active_search) =
            (self.inactive_search.clone(), self.active_search.clone());
        if let Some(session) = self.session.as_ref() {
            let inputs = (
                JobSource::of(session),
                inactive_search.clone(),
                active_search.clone(),
            );
            if self.projection_inputs.as_ref() != Some(&inputs) {
                self.projection = Projection::build(session, &inactive_search, &active_search);
                self.projection_inputs = Some(inputs);
            }
        }

        self.retire_stale_selection();
        let session = self.session.as_ref();
        let catalog: Option<&Catalog> = session.map(|session| session.catalog());
        let inactive = self
            .projection
            .inactive()
            .iter()
            .map(|row| {
                let record = catalog.and_then(|catalog| catalog.get(&row.mod_id));
                Row {
                    key: row.key.to_string().into(),
                    ordinal: 0,
                    title: display_name(record, &row.mod_id).into(),
                    detail: record
                        .map_or_else(String::new, |record| {
                            format!("{} · {}", record.source, package_text(record))
                        })
                        .into(),
                    unresolved: false,
                    unusable: record.is_none_or(|record| !record.is_valid()),
                }
            })
            .collect();
        let active = self
            .projection
            .active()
            .iter()
            .map(|row| {
                let record = row
                    .mod_id
                    .as_ref()
                    .and_then(|mod_id| catalog.map(|catalog| catalog.get(mod_id)))
                    .flatten();
                Row {
                    key: row.key.to_string().into(),
                    ordinal: row.ordinal as i32,
                    title: match row.mod_id.as_ref() {
                        Some(mod_id) => display_name(record, mod_id),
                        None => row.token.clone(),
                    }
                    .into(),
                    detail: match row.mod_id.as_ref() {
                        Some(_) => record.map_or_else(String::new, |record| {
                            format!("{} · {}", record.source, package_text(record))
                        }),
                        None => "not installed — removable".to_string(),
                    }
                    .into(),
                    unresolved: row.mod_id.is_none(),
                    unusable: record.is_none_or(|record| !record.is_valid())
                        && row.mod_id.is_some(),
                }
            })
            .collect();
        let inspector = self.inspector_of(catalog);

        View {
            inactive,
            active,
            inspector,
            validation: session.map_or_else(Vec::new, |session| {
                session
                    .validate()
                    .into_iter()
                    .map(|issue| {
                        format!(
                            "{:?} · {}: {}",
                            issue.severity(),
                            issue.category,
                            issue.message
                        )
                        .into()
                    })
                    .collect()
            }),
            reset_token: self
                .reset
                .as_ref()
                .map(|(token, _)| token.clone())
                .unwrap_or_default(),
            reset_enabled: self.reset.is_none() && reset_allowed(self),
            selected: self
                .selection_key()
                .map(|key| key.to_string())
                .unwrap_or_default(),
            dirty: session.is_some_and(|session| session.is_dirty()),
            read_only: session.is_none(),
            busy_refresh: self.jobs.busy(JobSlot::Refresh),
            busy_sort: self.jobs.busy(JobSlot::Sort),
            busy_save: self.jobs.busy(JobSlot::Save),
            busy_launch: self.jobs.busy(JobSlot::Launch),
            prompt: self.prompt.clone(),
            // A frame reads the state; it does not consume it. Taking the strings meant a
            // second paint in the same action showed nothing, and a message could vanish
            // before anyone could read it.
            status: self.status.clone(),
            progress: self.progress.clone(),
            note: self.note.clone(),
            copy: self.copy.clone(),
            draft: self.draft.clone(),
        }
    }

    /// The key the current projection gives the selected occurrence, if it is on screen at
    /// all. A row hidden by a search, or removed, simply has no key - which is what stops a
    /// later command from aiming at a different row that happens to sit nearby.
    fn selection_key(&self) -> Option<crate::ui::bridge::RowKey> {
        let session = self.session.as_ref()?;
        let selection = self.selected.as_ref()?;
        if !selection.tracked(session) {
            return None;
        }
        match selection {
            Selection::Active { position, .. } => self
                .projection
                .active()
                .iter()
                .find(|row| &row.position == position)
                .map(|row| row.key),
            Selection::Inactive { mod_id, .. } => self
                .projection
                .inactive()
                .iter()
                .find(|row| &row.mod_id == mod_id)
                .map(|row| row.key),
        }
    }

    /// Drops a tracked position that the session's own list no longer matches, so an order the
    /// window did not cause cannot leave a selection pointing at a different row.
    fn retire_stale_selection(&mut self) {
        let tracked = match (self.session.as_ref(), self.selected.as_ref()) {
            (Some(session), Some(selection)) => selection.tracked(session),
            // Nothing loaded, or nothing picked: there is no position to have lost.
            _ => true,
        };
        if !tracked {
            self.selected = None;
        }
    }

    /// Records the selection a callback's key refers to, refusing a key this projection never
    /// issued rather than keeping an address that cannot be resolved.
    fn choose(&mut self, key: &str) -> bool {
        let Some(found) = locate(self, key) else {
            self.status = refused(crate::ui::bridge::Stale::Unknown, key);
            return false;
        };
        let (generation, revision) = match self.session.as_ref() {
            Some(session) => (session.generation(), session.revision()),
            None => {
                self.status = "nothing is loaded to select from".to_string();
                return false;
            }
        };
        let found_in_active = self
            .projection
            .active()
            .iter()
            .find(|row| row.key == found)
            .map(|row| row.position);
        self.selected = match found_in_active {
            Some(position) => Some(Selection::Active {
                generation,
                revision,
                position,
            }),
            None => self
                .projection
                .inactive()
                .iter()
                .find(|row| row.key == found)
                .map(|row| Selection::Inactive {
                    generation,
                    mod_id: row.mod_id.clone(),
                }),
        };
        true
    }

    /// What the inspector says about the selected row, read from the catalog rather than
    /// from any model.
    fn inspector_of(&self, catalog: Option<&Catalog>) -> Inspector {
        let no_selection = || Inspector {
            title: if self.session.is_some() {
                "Nothing selected".to_string()
            } else {
                "No installation loaded".to_string()
            },
            ..Inspector::default()
        };
        let Some(session) = self.session.as_ref() else {
            return no_selection();
        };
        // Only a selection this list can still account for is described: a row that a sort or
        // a completion moved out from under the window shows nothing rather than its neighbour.
        let Some(selected) = self
            .selected
            .as_ref()
            .filter(|selection| selection.tracked(session))
        else {
            return no_selection();
        };
        if let Selection::Active { position, .. } = selected {
            let Some(entry) = session.working_entries().get(*position) else {
                return no_selection();
            };
            let record = entry.mod_id().and_then(|mod_id| catalog?.get(mod_id));
            return Inspector {
                title: display_name(
                    record,
                    &entry.mod_id().cloned().unwrap_or_else(|| ModId::new("")),
                ),
                source: record.map_or_else(
                    || "not installed".to_string(),
                    |record| format!("installed from {}", record.source),
                ),
                package: record.map_or_else(String::new, package_text),
                version: record.map_or_else(String::new, versions_text),
                description: record
                    .and_then(|record| record.parsed.description.clone())
                    .unwrap_or_else(|| "no description declared".to_string()),
                diagnostics: diagnostics_text(record),
            };
        }
        let mod_id = match selected {
            Selection::Inactive { mod_id, .. } => Some(mod_id.clone()),
            Selection::Active { .. } => None,
        };
        let record = mod_id.as_ref().and_then(|mod_id| catalog?.get(mod_id));
        Inspector {
            title: mod_id.as_ref().map_or_else(
                || "Nothing selected".to_string(),
                |mod_id| display_name(record, mod_id),
            ),
            source: record.map_or_else(String::new, |record| {
                format!("installed from {}", record.source)
            }),
            package: record.map_or_else(String::new, package_text),
            version: record.map_or_else(String::new, versions_text),
            description: record
                .and_then(|record| record.parsed.description.clone())
                .unwrap_or_else(|| "no description declared".to_string()),
            diagnostics: diagnostics_text(record),
        }
    }
}

/// The form text for a settings record. An absent optional is shown as empty, and empty
/// comes back as absent: nothing is guessed in either direction.
fn draft_of(settings: &Settings) -> DraftView {
    let text = |path: &Option<PathBuf>| {
        path.as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    DraftView {
        game_root: text(&settings.game_root),
        content_root: text(&settings.content_root),
        config_path: text(&settings.config_path),
        local_mods_root: text(&settings.local_mods_root),
        workshop_root: text(&settings.workshop_root),
        target_version: settings
            .target_version
            .as_ref()
            .map(|v| v.display().to_string())
            .unwrap_or_default(),
        executable_override: text(&settings.executable_override),
        confirm_warnings: settings.confirm_warnings,
    }
}

/// The form's text turned back into a settings record, or the reason it cannot be one yet.
///
/// Empty means absent: an optional path is never guessed, and an unreadable release is
/// refused rather than replaced.
fn draft_settings(draft: &DraftView) -> Result<Settings, String> {
    let path = |text: &str| {
        let trimmed = text.trim();
        (!trimmed.is_empty()).then(|| PathBuf::from(trimmed))
    };
    let target_version = if draft.target_version.trim().is_empty() {
        return Err(
            "no target version is set; name the release the list is validated against".to_string(),
        );
    } else {
        match GameVersion::parse(draft.target_version.trim()) {
            Ok(version) => version,
            Err(error) => return Err(format!("the target version is not usable: {error}")),
        }
    };
    Ok(Settings {
        schema_version: Settings::default().schema_version,
        game_root: path(&draft.game_root),
        content_root: path(&draft.content_root),
        config_path: path(&draft.config_path),
        local_mods_root: path(&draft.local_mods_root),
        workshop_root: path(&draft.workshop_root),
        target_version: Some(target_version),
        executable_override: path(&draft.executable_override),
        confirm_warnings: draft.confirm_warnings,
    })
}

fn edit_field(state: &Arc<State>, field: &str, value: &str) {
    let mut app = state.app.lock().expect("state");
    let draft = &mut app.draft;
    match field {
        "game-root" => draft.game_root = value.to_string(),
        "content-root" => draft.content_root = value.to_string(),
        "config-path" => draft.config_path = value.to_string(),
        "local-mods-root" => draft.local_mods_root = value.to_string(),
        "workshop-root" => draft.workshop_root = value.to_string(),
        "target-version" => draft.target_version = value.to_string(),
        "executable-override" => draft.executable_override = value.to_string(),
        other => {
            app.note = format!("the form has no {other} field to change");
            let view = app.view();
            drop(app);
            paint(state, &view);
            return;
        }
    }
    app.note.clear();
    let view = app.view();
    drop(app);
    paint(state, &view);
}

/// Persists the form through the accepted service. Nothing is written unless the whole form
/// is usable, and a failure leaves the previous file standing.
fn save_settings_request(state: &Arc<State>) {
    let mut app = state.app.lock().expect("state");
    let settings = match draft_settings(&app.draft) {
        Ok(settings) => settings,
        Err(reason) => {
            app.note = reason;
            let view = app.view();
            drop(app);
            paint(state, &view);
            return;
        }
    };
    let outcome = match app.settings_file.as_ref() {
        Some(path) => save_settings(path, &settings).map_err(|error| error.to_string()),
        None => Err("the settings location is unavailable".to_string()),
    };
    app.note = match outcome {
        Ok(()) => "settings saved".to_string(),
        Err(error) => format!("the settings could not be saved: {error}"),
    };
    let view = app.view();
    drop(app);
    paint(state, &view);
}

/// Asks the form's candidate for a read. An invalid form is refused here, so the loaded
/// installation keeps answering for its own captured target.
fn refresh_from_form(state: &Arc<State>) {
    let mut app = state.app.lock().expect("state");
    let settings = match draft_settings(&app.draft) {
        Ok(settings) => settings,
        Err(reason) => {
            app.note = reason;
            let view = app.view();
            drop(app);
            paint(state, &view);
            return;
        }
    };
    let target = RefreshTarget { settings };
    let view = if app.session.is_some() {
        // A loaded installation re-reads through the reducer, which owns the operation this
        // read belongs to and rejects a replacement that does not answer to it.
        drop(app);
        request_with(state, RequestKind::Refresh, Some(target));
        return;
    } else {
        if app.basis.is_none() {
            app.note = "the manager's working directory could not be read, so nothing was read"
                .to_string();
            let view = app.view();
            drop(app);
            paint(state, &view);
            return;
        }
        let job = app.jobs.dispatch_compute(
            &state.host,
            JobSlot::Refresh,
            JobOwner::Startup,
            None,
            JobWork::Refresh(target),
        );
        app.bootstrap = Some(job);
        app.status = "reading the configured paths...".to_string();
        app.note.clear();
        app.view()
    };
    paint(state, &view);
}

fn display_name(record: Option<&crate::domain::types::ModMetadata>, mod_id: &ModId) -> String {
    record
        .and_then(|record| record.parsed.name.clone())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| mod_id.to_string())
}

fn package_text(record: &crate::domain::types::ModMetadata) -> String {
    record
        .parsed
        .package_id
        .as_ref()
        .map(|package_id| package_id.as_str().to_string())
        .unwrap_or_else(|| "no package id".to_string())
}

fn versions_text(record: &crate::domain::types::ModMetadata) -> String {
    if record.parsed.supported_versions.is_empty() {
        return "no supported versions declared".to_string();
    }
    record
        .parsed
        .supported_versions
        .iter()
        .map(|version| version.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn diagnostics_text(record: Option<&crate::domain::types::ModMetadata>) -> String {
    let Some(record) = record else {
        return String::new();
    };
    if record.parsed.diagnostics.is_empty() {
        return "no metadata problems reported".to_string();
    }
    record
        .parsed
        .diagnostics
        .iter()
        .map(|diagnostic| format!("{:?}: {}", diagnostic.kind, diagnostic.element))
        .collect::<Vec<_>>()
        .join("\n")
}

struct State {
    app: Mutex<App>,
    host: JobHost,
    /// A weak handle only: a worker's notice reaches the window through it, and a window
    /// that has gone away is simply not upgraded.
    window: Weak<MainWindow>,
    profiles_window: Weak<ProfilesWindow>,
    woken: OnceLock<()>,
}

/// One open workspace window.
///
/// The `MainWindow` field is what keeps the component alive: a `slint::Weak` cannot, and a
/// window that was dropped the moment it was created has nothing for a worker's notice to
/// upgrade to. `Clone` on a component handle is another strong reference to the same
/// component, so a callback or a test that clones a Workspace does not extend or shorten
/// the window's life by itself.
#[derive(Clone)]
pub struct Workspace {
    /// The owning handle. A `slint::Weak` cannot keep a component alive, and in Slint 1.18
    /// the generated handle is not `Clone`, so the window is shared through an `Rc` that
    /// every `Workspace` clone holds. Workers never see it: they hold only a weak reference
    /// to the state, and the state holds only a weak reference to the window.
    window: Rc<MainWindow>,
    profiles_window: Rc<ProfilesWindow>,
    state: Arc<State>,
    weak_state: std::sync::Weak<State>,
}

impl Workspace {
    /// Creates the window and registers its controls.
    pub fn open(startup: &Startup) -> Result<Self, slint::PlatformError> {
        let window = MainWindow::new()?;
        let profiles_window = ProfilesWindow::new()?;
        let state = Arc::new(State {
            app: Mutex::new(App::new(startup)),
            host: JobHost::new(),
            window: window.as_weak(),
            profiles_window: profiles_window.as_weak(),
            woken: OnceLock::new(),
        });
        let workspace = Self {
            window: Rc::new(window),
            profiles_window: Rc::new(profiles_window),
            weak_state: Arc::downgrade(&state),
            state,
        };
        workspace.register(&workspace.window);
        workspace.register_close();
        profiles::register(&workspace.state, &workspace.profiles_window);
        // Armed once, here, so every producer of a result reaches the same notice: the first
        // read, a re-read from the form, a save, a launch.
        workspace.arm_wakeup();
        let view = workspace
            .state
            .app
            .lock()
            .expect("the state lock is never held across a callback")
            .view();
        paint(&workspace.state, &view);
        Ok(workspace)
    }

    /// Runs a closure against the generated window, for a caller that reads or drives its
    /// properties, models and callbacks.
    pub fn with_window<T>(&self, run: impl FnOnce(&MainWindow) -> T) -> T {
        run(&self.window)
    }

    /// Runs a closure against the application's checkpoint window.
    pub fn with_profiles<T>(&self, run: impl FnOnce(&ProfilesWindow) -> T) -> T {
        run(&self.profiles_window)
    }

    /// Makes the window visible. The event loop only runs while a window is, so this is
    /// part of starting an application rather than a detail a caller may skip.
    pub fn show(&self) -> Result<(), slint::PlatformError> {
        self.window.show()
    }

    fn register_close(&self) {
        let state = Arc::clone(&self.state);
        self.window.window().on_close_requested(move || {
            if close_allowed(&state) {
                slint::CloseRequestResponse::HideWindow
            } else {
                slint::CloseRequestResponse::KeepWindowShown
            }
        });
    }

    /// Whether the window's component is still alive on this thread.
    #[must_use]
    pub fn window_alive(&self) -> bool {
        self.state.window.upgrade().is_some()
    }

    /// The status line as the window currently shows it.
    #[must_use]
    pub fn status(&self) -> String {
        self.window.get_status().to_string()
    }

    /// Asks the captured candidate for its first read, under an identity the window names.
    /// Opens from the settings service. A missing file is the legitimate first-run default;
    /// a real path or parse failure is reported and left exactly as it is, never quietly
    /// replaced by defaults, and no read is attempted until the user configures something.
    pub fn configure(options: &Options) -> Result<Self, slint::PlatformError> {
        let settings_file = match &options.settings_file {
            Some(path) => Some(path.clone()),
            None => settings_path().ok(),
        };
        let (form, note) = match settings_file.as_ref() {
            None => (
                Settings::default(),
                "the settings location is unavailable, so nothing was read: fill in the paths                  below and choose Save settings to persist them"
                    .to_string(),
            ),
            Some(path) => match load_settings(path) {
                Ok(loaded) => (loaded, String::new()),
                Err(error) => (
                    Settings::default(),
                    format!(
                        "the settings file at {} could not be read: {error}; it was left                          untouched and nothing was read",
                        path.display()
                    ),
                ),
            },
        };
        let configured = form.config_path.is_some();
        let workspace = Self::open(&Startup {
            target: RefreshTarget { settings: form },
            basis: options.basis.clone(),
            platform: options
                .platform
                .or_else(Platform::current)
                .unwrap_or(Platform::Linux),
            settings_file,
        })?;
        {
            let mut app = workspace.state.app.lock().expect("state");
            app.note = note;
            match (configured, options.basis.is_some()) {
                (true, true) => app.status = "reading the installation...".to_string(),
                // Nothing is guessed and nothing is created: say which of the two things is
                // actually missing, rather than sending the user to fill in a form that is
                // already filled in.
                (true, false) => {
                    app.status = "the manager's working directory could not be read".to_string();
                    app.note = "relative paths have no basis to resolve against, so nothing was \
                                read and saving or launching is refused"
                        .to_string();
                }
                (false, _) => {
                    app.status = "no installation is configured yet".to_string();
                    if app.note.is_empty() {
                        app.note = "fill in the paths and target version below, then choose \
                                    Refresh from form"
                            .to_string();
                    }
                }
            }
            let view = app.view();
            drop(app);
            paint(&workspace.state, &view);
        }
        if configured && options.basis.is_some() {
            workspace.begin();
        }
        Ok(workspace)
    }

    /// The draft as the form holds it, as text. Distinct from what is installed.
    #[must_use]
    pub fn draft(&self) -> DraftView {
        self.state.app.lock().expect("state").draft.clone()
    }

    /// The settings line under the form.
    #[must_use]
    pub fn settings_note(&self) -> String {
        self.window.get_settings_note().to_string()
    }

    pub fn begin(&self) {
        let target = self.state.app.lock().expect("state").target.clone();
        let job = self.state.app.lock().expect("state").jobs.dispatch_compute(
            &self.state.host,
            JobSlot::Refresh,
            JobOwner::Startup,
            None,
            JobWork::Refresh(target),
        );
        let mut app = self.state.app.lock().expect("state");
        app.bootstrap = Some(job);
        let view = app.view();
        drop(app);
        paint(&self.state, &view);
    }

    /// Takes the host's notice and applies whatever has arrived. UI thread only.
    pub fn deliver(&self) {
        deliver_state(&self.state);
    }

    /// Whether the window has reported a terminal outcome of its own.
    #[must_use]
    pub fn status_reported(&self) -> bool {
        self.state.app.lock().expect("state").reported
    }

    /// The bootstrap job the window is waiting for, if one was dispatched.
    #[must_use]
    pub fn bootstrap_job(&self) -> Option<JobId> {
        self.state.app.lock().expect("state").bootstrap
    }

    /// Whether an installation has been loaded into the window.
    #[must_use]
    pub fn installed(&self) -> bool {
        self.state.app.lock().expect("state").session.is_some()
    }

    /// The active list as the window holds it, token by token.
    #[must_use]
    pub fn active_tokens(&self) -> Vec<String> {
        let app = self.state.app.lock().expect("state");
        app.session.as_ref().map_or_else(Vec::new, |session| {
            session
                .working_entries()
                .iter()
                .map(|entry| entry.original_token().to_string())
                .collect()
        })
    }

    /// The generation and revision the window currently holds, if any.
    #[must_use]
    pub fn source(&self) -> Option<JobSource> {
        self.state.app.lock().expect("state").source()
    }

    /// One worker's result, judged by the controller's gates and then acted on.
    /// Installs the coalesced notice: one knock per burst, scheduled onto the event loop.
    fn arm_wakeup(&self) {
        if self.state.woken.get().is_some() {
            return;
        }
        let host = &self.state.host;
        // Weak on purpose: the notice lives inside the host, which lives inside the state,
        // so a strong capture here would be a cycle that keeps every job, result and window
        // reference in this process forever.
        let state = std::sync::Weak::clone(&self.weak_state);
        let installed = install_wakeup(host, move || {
            let state = std::sync::Weak::clone(&state);
            let _ = slint::invoke_from_event_loop(move || {
                // Upgraded on the UI thread, where the window owner is alive. A state that
                // is gone means the window is gone, and a late notice is then simply dropped.
                let Some(state) = state.upgrade() else {
                    return;
                };
                state_wake(&state);
            });
        });
        let _ = self.state.woken.set(());
        debug_assert!(
            installed || self.state.woken.get().is_some(),
            "the notice is installed exactly once"
        );
    }

    fn register(&self, window: &MainWindow) {
        let state = Arc::clone(&self.state);
        window.on_open_profiles(move || profiles::open(&state));
        let state = Arc::clone(&self.state);
        window.on_inactive_searched(move |text| {
            let state = Arc::clone(&state);
            edit(&state, move |app| {
                app.inactive_search = text.to_string();
                true
            });
        });
        let state = Arc::clone(&self.state);
        window.on_active_searched(move |text| {
            let state = Arc::clone(&state);
            edit(&state, move |app| {
                app.active_search = text.to_string();
                true
            });
        });
        let state = Arc::clone(&self.state);
        window.on_save(move || request(&state, RequestKind::Save));
        let state = Arc::clone(&self.state);
        window.on_reset(move || reset_request(&state));
        let state = Arc::clone(&self.state);
        window.on_answer_reset(move |token, choice| reset_answer(&state, &token, &choice));
        let state = Arc::clone(&self.state);
        window.on_launch(move || request(&state, RequestKind::Launch));
        let state = Arc::clone(&self.state);
        window.on_close_window(move || {
            if close_allowed(&state) {
                hide(&state);
            }
        });
        let state = Arc::clone(&self.state);
        window.on_answer_copy(move |nonce, choice| {
            answer_copy(&state, &nonce, &choice);
        });
        let state = Arc::clone(&self.state);
        window.on_edit_field(move |field, value| {
            edit_field(&state, &field, &value);
        });
        let state = Arc::clone(&self.state);
        window.on_toggle_warnings(move |value| {
            let mut app = state.app.lock().expect("state");
            app.draft.confirm_warnings = value;
            app.note = "the warning preference applies to the next operation".to_string();
            let view = app.view();
            drop(app);
            paint(&state, &view);
        });
        let state = Arc::clone(&self.state);
        window.on_save_settings(move || save_settings_request(&state));
        let state = Arc::clone(&self.state);
        window.on_refresh_from_form(move || refresh_from_form(&state));
        let state = Arc::clone(&self.state);
        window.on_answer_prompt(move |token, choice| {
            answer(&state, &token, &choice);
        });
        let state = Arc::clone(&self.state);
        window.on_select_row(move |key| {
            let state = Arc::clone(&state);
            edit(&state, |app| app.choose(&key));
        });
        let state = Arc::clone(&self.state);
        window.on_inspect_row(move |key| {
            let state = Arc::clone(&state);
            edit(&state, |app| app.choose(&key));
        });
        let state = Arc::clone(&self.state);
        window.on_activate_row(move |key| {
            let state = Arc::clone(&state);
            edit(&state, move |app| activate(app, &key));
        });
        let state = Arc::clone(&self.state);
        window.on_deactivate_row(move |key| {
            let state = Arc::clone(&state);
            edit(&state, move |app| deactivate(app, &key));
        });
        let state = Arc::clone(&self.state);
        window.on_select_relative(move |pane, delta| {
            let state = Arc::clone(&state);
            edit(&state, move |app| select_relative(app, &pane, delta));
        });
        let state = Arc::clone(&self.state);
        window.on_move_selected(move |delta| {
            let state = Arc::clone(&state);
            edit(&state, move |app| move_selected(app, delta));
        });
        let state = Arc::clone(&self.state);
        window.on_activate_selected(move || {
            let state = Arc::clone(&state);
            edit(&state, activate_selected);
        });
        let state = Arc::clone(&self.state);
        window.on_delete_selected(move || {
            let state = Arc::clone(&state);
            edit(&state, delete_selected);
        });
        let state = Arc::clone(&self.state);
        window.on_keyboard_save(move || keyboard_save(&state));
        let state = Arc::clone(&self.state);
        window.on_move_row(move |key, delta| {
            let state = Arc::clone(&state);
            edit(&state, move |app| move_row(app, &key, delta));
        });
        let state = Arc::clone(&self.state);
        window.on_refresh(move || {
            let state = Arc::clone(&state);
            refresh(&state);
        });
        let state = Arc::clone(&self.state);
        window.on_cancel_refresh(move || {
            let state = Arc::clone(&state);
            cancel(&state, JobSlot::Refresh, "scan");
        });
        let state = Arc::clone(&self.state);
        window.on_sort_list(move || {
            let state = Arc::clone(&state);
            sort(&state);
        });
        let state = Arc::clone(&self.state);
        window.on_cancel_sort(move || {
            let state = Arc::clone(&state);
            cancel(&state, JobSlot::Sort, "sort");
        });
    }
}

/// One worker's result, judged by the controller's gates and then acted on.
fn apply_delivered(app: &mut App, delivered: Delivered) -> Vec<WorkflowEvent> {
    let completion = match delivered {
        Delivered::Current(completion) => completion,
        Delivered::Discarded { kind, .. } => {
            app.status = format!(
                "a {} result was drawn from a list that has changed and was dropped",
                kind.word()
            );
            return Vec::new();
        }
    };
    let reported = app.reported;
    let is_bootstrap = !reported && app.bootstrap == Some(completion.job);
    let sort = match app.jobs.route(&completion, app.source()) {
        Routed::Superseded => {
            app.status = format!(
                "a {} result arrived that this window no longer waits for and was dropped",
                completion.kind.word()
            );
            return Vec::new();
        }
        Routed::Accepted(kind) => kind,
    };
    // A write or a spawn is a fact: its result goes back to the operation that asked for it,
    // whatever else the list has done since, and the reducer decides what it means.
    if let Some((_job, operation)) = app.save.filter(|(job, _)| *job == completion.job) {
        app.save = None;
        let outcome = match completion.outcome {
            JobOutcome::Saved(outcome) => outcome,
            JobOutcome::Cancelled => SaveOutcome::Failed(OperationFailure::new(
                "the save job was stopped before it wrote anything",
            )),
            other => SaveOutcome::Failed(OperationFailure::new(format!(
                "the save job reported {other:?}"
            ))),
        };
        return vec![WorkflowEvent::SaveFinished { operation, outcome }];
    }
    if let Some((_job, operation)) = app.launch.filter(|(job, _)| *job == completion.job) {
        app.launch = None;
        let outcome = match completion.outcome {
            JobOutcome::Launched(outcome) => outcome,
            JobOutcome::Cancelled => LaunchOutcome::Failed(OperationFailure::new(
                "the launch job was stopped before it started anything",
            )),
            other => LaunchOutcome::Failed(OperationFailure::new(format!(
                "the launch job reported {other:?}"
            ))),
        };
        return vec![WorkflowEvent::LaunchFinished { operation, outcome }];
    }
    if let Some((_job, operation)) = app.refresh.filter(|(job, _)| *job == completion.job) {
        app.refresh = None;
        let outcome = match completion.outcome {
            JobOutcome::Refreshed(outcome) => outcome,
            JobOutcome::Cancelled => RefreshOutcome::Failed(OperationFailure::new(
                "the re-read was stopped before it finished",
            )),
            other => RefreshOutcome::Failed(OperationFailure::new(format!(
                "the re-read reported {other:?}"
            ))),
        };
        return vec![WorkflowEvent::RefreshFinished { operation, outcome }];
    }
    if sort == JobKind::Refresh {
        app.reported = true;
        match completion.outcome {
            JobOutcome::Refreshed(RefreshOutcome::Reloaded(replacement)) => {
                install(app, *replacement, is_bootstrap);
            }
            JobOutcome::Refreshed(RefreshOutcome::Failed(failure)) => {
                app.status = format!("the installation could not be read: {}", failure.message);
            }
            JobOutcome::Cancelled => {
                app.status = "the scan was cancelled and nothing was installed".to_string();
            }
            other => app.status = format!("the read reported an unexpected result: {other:?}"),
        }
        return Vec::new();
    }
    if sort == JobKind::Sort {
        app.status = apply_sort_from(app, &completion);
    } else {
        app.status = format!("an unexpected {} result was ignored", sort.word());
    }
    Vec::new()
}

fn state_wake(state: &Arc<State>) {
    let Some(_window) = state.window.upgrade() else {
        // The owner is gone with the window: seal the host so a worker that finishes
        // afterwards is dropped and counted rather than retained.
        state.host.shutdown();
        return;
    };
    deliver_state(state);
}

/// Takes the host's notice and applies whatever has arrived. UI thread only.
fn deliver_state(state: &Arc<State>) {
    profiles::deliver(state);
    let (events, arrived) = {
        let mut app = state
            .app
            .lock()
            .expect("the state lock is never held across a callback");
        let current = app.source();
        let delivered = drain(&state.host, current);
        let mut changed = false;
        let mut events = Vec::new();
        for delivered in delivered {
            events.extend(apply_delivered(&mut app, delivered));
        }
        while let Some(progress) = state.host.poll_progress() {
            if app.jobs.shows_progress(&progress) {
                app.progress = format!(
                    "{}: {} ({} steps)",
                    progress.kind.word(),
                    stage_word(progress.stage),
                    progress.steps
                );
                changed = true;
            }
        }
        if !changed {
            app.progress.clear();
        }
        let arrived = app.view();
        (events, arrived)
    };
    paint(state, &arrived);
    if !events.is_empty() {
        // The reducer is asked from outside the state lock, so an effect that hides the
        // window or dispatches another job is never reached from inside it.
        reduce(state, events);
    }
    if state.app.lock().expect("state").close_effect {
        hide(state);
    }
}

/// Runs one edit against the session, then repaints what it changed.
fn edit(state: &Arc<State>, change: impl FnOnce(&mut App) -> bool) {
    let mut app = state
        .app
        .lock()
        .expect("the state lock is never held across a callback");
    if app.reset.is_some() {
        app.status = "answer the Reset question first".to_string();
        let view = app.view();
        drop(app);
        paint(state, &view);
        return;
    }
    if !change(&mut app) {
        let view = app.view();
        drop(app);
        paint(state, &view);
        return;
    }
    let view = app.view();
    drop(app);
    paint(state, &view);
}

fn reset_allowed(app: &App) -> bool {
    app.session.is_some()
        && !app.profiles.busy
        && app.profiles.delete.is_none()
        && app.prompt.is_none()
        && app.copy.is_none()
        && app.save.is_none()
        && app.launch.is_none()
        && app.refresh.is_none()
        && !app.closing
        && !app.jobs.busy(JobSlot::Save)
        && !app.jobs.busy(JobSlot::Launch)
        && !app.jobs.busy(JobSlot::Refresh)
}

fn reset_request(state: &Arc<State>) {
    let mut app = state.app.lock().expect("state");
    if app.reset.is_some() || !reset_allowed(&app) {
        app.status = "Reset is unavailable while an operation or question is pending".to_string();
    } else if app.session.as_ref().is_some_and(Session::is_dirty) {
        app.reset_serial += 1;
        app.reset = app
            .source()
            .map(|source| (format!("reset-{}", app.reset_serial), source));
    } else {
        reset_working(&mut app);
    }
    let view = app.view();
    drop(app);
    paint(state, &view);
}

fn reset_answer(state: &Arc<State>, token: &str, choice: &str) {
    let mut app = state.app.lock().expect("state");
    if let Some((pending, source)) = app.reset.as_ref()
        && pending == token
        && matches!(choice, "confirm" | "cancel")
    {
        let current = app.source().as_ref() == Some(source) && reset_allowed(&app);
        app.reset = None;
        if choice == "confirm" && current {
            reset_working(&mut app);
        } else {
            app.status = "Reset cancelled; working list retained".to_string();
        }
    }
    let view = app.view();
    drop(app);
    paint(state, &view);
}

fn reset_working(app: &mut App) {
    let _ = app.jobs.cancel(JobSlot::Sort);
    if let Some(session) = app.session.as_mut() {
        session.reset();
    }
    app.selected = None;
    app.status = "working list reset to the loaded game baseline; no files written".to_string();
}

/// Finds the row a window's label names, in either pane.
fn locate(app: &App, key: &str) -> Option<crate::ui::bridge::RowKey> {
    app.projection
        .inactive()
        .iter()
        .map(|row| row.key)
        .chain(app.projection.active().iter().map(|row| row.key))
        .find(|found| found.to_string() == key)
}

fn activate(app: &mut App, key: &str) -> bool {
    let Some(found) = locate(app, key) else {
        app.status = refused(crate::ui::bridge::Stale::Unknown, key);
        return false;
    };
    let App {
        session,
        projection,
        status,
        ..
    } = app;
    let Some(session) = session.as_mut() else {
        *status = "nothing is loaded to activate into".to_string();
        return false;
    };
    let mod_id = match projection.resolve_inactive(session, found) {
        Ok(mod_id) => mod_id,
        Err(stale) => {
            *status = refused(stale, key);
            return false;
        }
    };
    match session.activate(&mod_id) {
        Ok(()) => true,
        Err(crate::application::session::EditError::AlreadyActive {
            active_position,
            active_mod_id,
            wanted_mod_id,
            ..
        }) => {
            // The accepted session refuses to pick between two physical copies. That refusal
            // is turned into the owner-approved explicit replacement question for exactly the
            // row it named - never into a silent swap, and never into a question that
            // overwrites the workflow's own outstanding one.
            let App {
                prompt,
                copy,
                copy_serial,
                status,
                ..
            } = app;
            if prompt.is_some() {
                *status = "the workflow's question has to be answered first".to_string();
                return false;
            }
            if copy.is_some() {
                *status = "a copy question is already open".to_string();
                return false;
            }
            let describe = |mod_id: &ModId| {
                session
                    .catalog()
                    .get(mod_id)
                    .map(|record| {
                        format!("{} ({})", display_name(Some(record), mod_id), record.source)
                    })
                    .unwrap_or_else(|| format!("{mod_id} (unusable record)"))
            };
            let (current, wanted) = (describe(&active_mod_id), describe(&wanted_mod_id));
            *copy_serial += 1;
            *copy = Some(CopyQuestion {
                nonce: format!("copy-{copy_serial}"),
                title: format!("Row {} already uses another copy", active_position + 1),
                detail: format!(
                    "This row uses {current}. Use {wanted} instead? The other copy stays                      installed and unchanged."
                ),
                generation: session.generation(),
                revision: session.revision(),
                position: active_position,
                wanted: wanted_mod_id,
            });
            *status = "the window is asking which copy that row should use".to_string();
            false
        }
        Err(error) => {
            *status = format!("cannot activate that copy: {error}");
            false
        }
    }
}

/// Answers the window's copy question - and only the question carrying this nonce, raised
/// against this session state.
fn answer_copy(state: &Arc<State>, nonce: &str, choice: &str) {
    let mut app = state
        .app
        .lock()
        .expect("the state lock is never held across a callback");
    {
        let App {
            copy,
            status,
            session,
            ..
        } = &mut *app;
        let question = copy
            .as_ref()
            .filter(|question| question.nonce == nonce)
            .cloned();
        match question {
            // An answer for a question this one replaced is dropped, not reinterpreted
            // against whatever row now happens to sit at the old position.
            None => {
                *status = "that copy question has already been answered or replaced".to_string();
            }
            Some(question) => {
                let Some(session) = session.as_mut() else {
                    *copy = None;
                    *status = "nothing is loaded to replace a copy in".to_string();
                    return;
                };
                if choice != "replace" {
                    // Keeping the copy a row already uses changes nothing: no edit, no
                    // revision, no write.
                    *copy = None;
                    *status = "keeping the copy that row already uses".to_string();
                } else if session.generation() != question.generation
                    || session.revision() != question.revision
                {
                    // The row this question was asked about is no longer the list's row at
                    // that position: refuse, and clear the stale question so the window is
                    // not left waiting on an answer that can no longer mean anything.
                    *copy = None;
                    *status = "that copy question no longer matches this list, so nothing was \
                                replaced"
                        .to_string();
                } else {
                    // The accepted session's own gate does the rest: the token it writes has
                    // to resolve back to exactly this physical copy, or nothing changes.
                    let outcome = session.replace_copy(question.position, &question.wanted);
                    *copy = None;
                    *status = match outcome {
                        Ok(()) => "the row now uses the copy you chose".to_string(),
                        Err(error) => format!("that copy cannot be used here: {error}"),
                    };
                }
            }
        }
    }
    let view = app.view();
    drop(app);
    paint(state, &view);
}

fn deactivate(app: &mut App, key: &str) -> bool {
    let Some(found) = locate(app, key) else {
        app.status = refused(crate::ui::bridge::Stale::Unknown, key);
        return false;
    };
    let App {
        session,
        projection,
        status,
        ..
    } = app;
    let Some(session) = session.as_mut() else {
        *status = "nothing is loaded to remove from".to_string();
        return false;
    };
    let removed = match projection.resolve_active(session, found) {
        Ok(position) => match session.deactivate(position) {
            Ok(()) => Some(position),
            Err(error) => {
                *status = format!("cannot remove that row: {error}");
                None
            }
        },
        Err(stale) => {
            *status = refused(stale, key);
            None
        }
    };
    match removed {
        // The removed row leaves the list, so it cannot stay selected; a row behind it moves
        // up to keep being the same occurrence.
        Some(position) => {
            let revision = session.revision();
            let keeps = app.selected.as_mut().is_some_and(|selection| {
                selection.after_remove(position) && {
                    selection.set_revision(revision);
                    true
                }
            });
            if !keeps {
                app.selected = None;
            }
            true
        }
        None => false,
    }
}

fn move_row(app: &mut App, key: &str, delta: i32) -> bool {
    let Some(found) = locate(app, key) else {
        app.status = refused(crate::ui::bridge::Stale::Unknown, key);
        return false;
    };
    let App {
        session,
        projection,
        status,
        ..
    } = app;
    let Some(session) = session.as_mut() else {
        *status = "nothing is loaded to reorder".to_string();
        return false;
    };
    let Ok(position) = projection.resolve_active(session, found) else {
        *status = refused(crate::ui::bridge::Stale::Unknown, key);
        return false;
    };
    let width = session.working_entries().len() as i32;
    let target = position as i32 + delta;
    if target < 0 || target >= width {
        *status = "that row is already at the end of the list".to_string();
        return false;
    }
    let moved = match session.move_entry(position, target as usize) {
        Ok(()) => Some((position, target as usize, session.revision())),
        Err(error) => {
            *status = format!("cannot move that row: {error}");
            None
        }
    };
    match moved {
        // The selection follows the occurrence that moved, by position arithmetic on the
        // session's own list - never by matching a package id, which two rows can share.
        Some((from, to, revision)) => {
            if let Some(selection) = app.selected.as_mut() {
                selection.after_move(from, to);
                selection.set_revision(revision);
            }
            true
        }
        None => false,
    }
}

/// Asks the workflow for one of its four operations. The warning preference is read now, so
/// it governs this operation only, exactly as the reducer expects.
fn request(state: &Arc<State>, kind: RequestKind) {
    request_with(state, kind, None);
}

/// Asks the reducer for one operation. The warning preference is the one held **now**, so it
/// governs this operation while an operation already running keeps the value it captured; a
/// re-read uses the installed candidate unless the form explicitly named another.
fn request_with(state: &Arc<State>, kind: RequestKind, refresh: Option<RefreshTarget>) {
    let blocked = {
        let app = state.app.lock().expect("state");
        app.reset.is_some() || app.profiles.busy || app.profiles.delete.is_some()
    };
    if blocked {
        return;
    }
    let (confirm, target) = {
        let app = state.app.lock().expect("state");
        (
            app.draft.confirm_warnings,
            refresh.unwrap_or_else(|| app.target.clone()),
        )
    };
    let event = match kind {
        RequestKind::Save => WorkflowEvent::SaveRequested {
            confirm_warnings: confirm,
        },
        RequestKind::Launch => WorkflowEvent::LaunchRequested {
            confirm_warnings: confirm,
        },
        RequestKind::Close => WorkflowEvent::CloseRequested {
            confirm_warnings: confirm,
        },
        RequestKind::Refresh => WorkflowEvent::RefreshRequested {
            target,
            confirm_warnings: confirm,
        },
    };
    reduce(state, vec![event]);
}

/// Answers the question the window is showing, and only that question. The token the window
/// hands back is matched against the stored prompt, so a choice from a dialog the reducer has
/// already replaced is ignored instead of answering the newer one.
fn answer(state: &Arc<State>, token: &str, choice: &str) {
    let pending = {
        let mut app = state.app.lock().expect("state");
        match app
            .prompt
            .as_ref()
            .filter(|prompt| prompt.token == token)
            .cloned()
        {
            Some(prompt) => Some(prompt),
            None => {
                app.status = "that question has already been answered or replaced".to_string();
                None
            }
        }
    };
    let Some(prompt) = pending else {
        let view = state.app.lock().expect("state").view();
        paint(state, &view);
        return;
    };
    let Some(answer) = (match choice {
        "save" => Some(PromptAnswer::Save),
        "discard" => Some(PromptAnswer::Discard),
        "cancel" => Some(PromptAnswer::Cancel),
        "confirm" => Some(PromptAnswer::Confirm),
        "dismiss" => Some(PromptAnswer::Dismiss),
        _ => None,
    }) else {
        let mut app = state.app.lock().expect("state");
        app.status = "that is not one of the answers this question offers".to_string();
        let view = app.view();
        drop(app);
        paint(state, &view);
        return;
    };
    reduce(
        state,
        vec![WorkflowEvent::PromptAnswered {
            prompt: prompt.id,
            answer,
        }],
    );
}

/// Whether a close request may take the window away.
fn close_allowed(state: &Arc<State>) -> bool {
    let blocked = {
        let app = state.app.lock().expect("state");
        app.reset.is_some() || app.profiles.busy || app.profiles.delete.is_some()
    };
    if blocked {
        return false;
    }
    {
        let mut app = state.app.lock().expect("state");
        if app.save.is_some()
            || app.launch.is_some()
            || app.jobs.busy(JobSlot::Save)
            || app.jobs.busy(JobSlot::Launch)
        {
            app.status = "a write or a launch is finishing; the window stays open".to_string();
            let view = app.view();
            drop(app);
            paint(state, &view);
            return false;
        }
        if app.session.is_none() {
            // Nothing is loaded: stop any computation still running, seal the host so a
            // late result is dropped, and let the window go. No config is created.
            let _ = app.jobs.cancel(JobSlot::Refresh);
            let _ = app.jobs.cancel(JobSlot::Sort);
            state.host.shutdown();
            return true;
        }
    }
    let confirm = state
        .app
        .lock()
        .expect("state")
        .target
        .settings
        .confirm_warnings;
    {
        let mut app = state.app.lock().expect("state");
        app.close_effect = false;
    }
    reduce(
        state,
        vec![WorkflowEvent::CloseRequested {
            confirm_warnings: confirm,
        }],
    );
    state.app.lock().expect("state").close_effect
}

fn hide(state: &Arc<State>) {
    if let Some(window) = state.profiles_window.upgrade() {
        let _ = window.hide();
    }
    if let Some(window) = state.window.upgrade() {
        let _ = window.hide();
    }
}

/// Feeds events to the accepted reducer and carries out the effects it decides, in order.
fn reduce(state: &Arc<State>, events: Vec<WorkflowEvent>) {
    {
        let mut app = state
            .app
            .lock()
            .expect("the state lock is never held across a callback");
        let App {
            session,
            workflow,
            status,
            prompt,
            ..
        } = &mut *app;
        let (Some(session), Some(workflow)) = (session.as_mut(), workflow.as_mut()) else {
            *status = "nothing is loaded yet, so there is nothing to operate on".to_string();
            let view = app.view();
            drop(app);
            paint(state, &view);
            return;
        };
        let mut collected = Vec::new();
        for event in events {
            collected.extend(workflow.reduce(session, event));
        }
        if prompt
            .as_ref()
            .is_some_and(|prompt| !workflow.owns_prompt(prompt.id))
        {
            *prompt = None;
        }
        app.closing = workflow.has_pending_close()
            || collected
                .iter()
                .any(|effect| matches!(effect, WorkflowEffect::Close));
        apply_effects(state, &mut app, collected);
    }
    let (view, close_out) = {
        let mut app = state.app.lock().expect("state");
        let close_out = app.close_effect && app.closing;
        if close_out {
            app.close_effect = false;
            app.closing = false;
        }
        (app.view(), close_out)
    };
    paint(state, &view);
    if close_out {
        // A close that the reducer earned while the user was answering a question: the
        // window goes away now, and nothing else.
        hide(state);
    }
}

/// Applies one batch of effects. A `Save` or `Launch` is a fact-producing job and goes through
/// the fact slot, which refuses to displace a job whose result is still expected.
fn apply_effects(state: &State, app: &mut App, effects: Vec<WorkflowEffect>) {
    for effect in effects {
        match effect {
            WorkflowEffect::PromptUnsavedReplace { prompt, .. } => {
                app.prompt = Some(Prompt::ask(
                    prompt,
                    "Replace unapplied game changes?",
                    "Apply to game and continue, continue without applying, or cancel. No checkpoint is changed.",
                    true,
                    true,
                    false,
                ));
            }
            WorkflowEffect::InstallReplacement { tokens, source } => {
                if let Some(session) = app.session.as_mut() {
                    let _ = session.replace_tokens(&tokens);
                    let _ = app.jobs.cancel(JobSlot::Sort);
                    app.selected = None;
                    app.status = format!("Loaded {source}; game config unchanged by loading");
                }
            }
            WorkflowEffect::PromptUnsavedLaunch {
                prompt,
                operation: _,
            } => {
                app.prompt = Some(Prompt::ask(
                    prompt,
                    "Launch needs the pending edits saved",
                    "Launching saves the list first, or does not launch.",
                    true,
                    false,
                    false,
                ));
            }
            WorkflowEffect::PromptUnsavedClose {
                prompt,
                operation: _,
            } => {
                app.prompt = Some(Prompt::ask(
                    prompt,
                    "Closing with unsaved changes",
                    "Save them, discard them, or keep the window open.",
                    true,
                    true,
                    false,
                ));
            }
            WorkflowEffect::PromptUnsavedRefresh {
                prompt,
                operation: _,
            } => {
                app.prompt = Some(Prompt::ask(
                    prompt,
                    "Re-reading with unsaved changes",
                    "The re-read would replace the pending edits.",
                    true,
                    true,
                    false,
                ));
            }
            WorkflowEffect::PromptWarnings {
                prompt,
                operation: _,
                issues,
            } => {
                let detail = issues
                    .iter()
                    .map(|issue| issue.message.clone())
                    .collect::<Vec<_>>()
                    .join("\n");
                app.prompt = Some(Prompt::ask(
                    prompt,
                    "The list has findings",
                    &detail,
                    false,
                    false,
                    true,
                ));
            }
            WorkflowEffect::Save {
                operation,
                path,
                baseline,
                prepared,
            } => {
                let Some(bound) = app.source() else {
                    app.status = "a write was asked for with no session loaded".to_string();
                    continue;
                };
                match app.jobs.dispatch_fact(
                    &state.host,
                    JobSlot::Save,
                    operation,
                    bound,
                    JobWork::Save(crate::application::jobs::SaveRequest {
                        path,
                        baseline,
                        prepared,
                    }),
                ) {
                    Ok(job) => app.save = Some((job, operation)),
                    Err(_) => app.status = "a write is already running".to_string(),
                }
            }
            WorkflowEffect::Launch { operation, command } => {
                let Some(bound) = app.source() else {
                    app.status = "a launch was asked for with no session loaded".to_string();
                    continue;
                };
                match app.jobs.dispatch_fact(
                    &state.host,
                    JobSlot::Launch,
                    operation,
                    bound,
                    JobWork::Launch(command),
                ) {
                    Ok(job) => app.launch = Some((job, operation)),
                    Err(_) => app.status = "a launch is already running".to_string(),
                }
            }
            WorkflowEffect::Refresh { operation, target } => {
                let bound = app.source();
                let job = app.jobs.dispatch_compute(
                    &state.host,
                    JobSlot::Refresh,
                    JobOwner::Workflow(operation),
                    bound,
                    JobWork::Refresh(target),
                );
                app.refresh = Some((job, operation));
                app.status = "re-reading the installation...".to_string();
            }
            WorkflowEffect::InstallRefresh { replacement, .. } => {
                install(app, *replacement, false);
                app.refresh = None;
            }
            WorkflowEffect::ReportFailure { operation, message } => {
                app.status = format!("{}: {message}", operation.word());
                app.reported = true;
                // A refused request is not the running job's completion. Its receipt
                // remains owned until apply_delivered consumes the matching result.
            }
            WorkflowEffect::Close => {
                app.close_effect = true;
                app.prompt = None;
            }
        }
    }
}

impl Prompt {
    #[allow(clippy::too_many_arguments)]
    fn ask(
        id: PromptId,
        title: &str,
        detail: &str,
        allow_save: bool,
        allow_discard: bool,
        allow_confirm: bool,
    ) -> Self {
        Self {
            token: format!("{id:?}"),
            title: title.to_string(),
            detail: detail.to_string(),
            allow_save,
            allow_discard,
            allow_confirm,
            id,
        }
    }
}

fn refresh(state: &Arc<State>) {
    if state.app.lock().expect("state").session.is_some() {
        request(state, RequestKind::Refresh);
        return;
    }
    let target = state.app.lock().expect("state").target.clone();
    let bound = state.app.lock().expect("state").source();
    let job = state.app.lock().expect("state").jobs.dispatch_compute(
        &state.host,
        JobSlot::Refresh,
        JobOwner::Startup,
        bound,
        JobWork::Refresh(target),
    );
    let mut app = state.app.lock().expect("state");
    if app.session.is_none() {
        app.bootstrap = Some(job);
    }
    app.status = "re-reading the installation...".to_string();
    let view = app.view();
    drop(app);
    paint(state, &view);
}

fn sort(state: &Arc<State>) {
    if state.app.lock().expect("state").reset.is_some() {
        return;
    }
    let snapshot = {
        let app = state.app.lock().expect("state");
        app.session.as_ref().map(ModListSnapshot::from_session)
    };
    let Some(snapshot) = snapshot else {
        let mut app = state.app.lock().expect("state");
        app.status = "nothing is loaded to sort".to_string();
        let view = app.view();
        drop(app);
        paint(state, &view);
        return;
    };
    let bound = state.app.lock().expect("state").source();
    let mut app = state.app.lock().expect("state");
    if app.jobs.busy(JobSlot::Sort) {
        app.status = "a sort is already running".to_string();
    } else {
        app.jobs.dispatch_compute(
            &state.host,
            JobSlot::Sort,
            JobOwner::Startup,
            bound,
            JobWork::Sort(snapshot),
        );
        app.status = "sorting...".to_string();
    }
    let view = app.view();
    drop(app);
    paint(state, &view);
}

fn cancel(state: &Arc<State>, slot: JobSlot, word: &str) {
    let cancelled = state.app.lock().expect("state").jobs.cancel(slot);
    let mut app = state.app.lock().expect("state");
    app.status = if cancelled.is_some() {
        format!("cancelling the {word}; its result will not be applied")
    } else {
        format!("nothing to cancel for the {word}")
    };
    if slot == JobSlot::Refresh && app.bootstrap == cancelled {
        app.bootstrap = None;
    }
    let view = app.view();
    drop(app);
    paint(state, &view);
}

/// Builds the session and the workflow that goes with it, together, from the payload that
/// was read for the captured candidate — never from the form as edited.
fn install(app: &mut App, replacement: RefreshReplacement, bootstrap: bool) {
    let RefreshReplacement {
        origin,
        config,
        catalog,
        target_version,
        diagnostics,
    } = replacement;
    let Some(path) = origin.config_path() else {
        app.status = "the read candidate named no configuration file".to_string();
        return;
    };
    let Some(basis) = app.basis.as_ref() else {
        // Relative launch paths have no honest basis to resolve against, so an
        // installation is never half-loaded with an invented one.
        app.status = "the installation was read, but the manager's working directory could not                       be read, so saving and launching are refused"
            .to_string();
        return;
    };
    if app.profiles.seed.is_none() {
        app.profiles.seed = Some(config.active_tokens.clone());
    }
    let session = Session::new(catalog, config, path.to_path_buf(), target_version);
    let workflow = Workflow::new(&origin.settings, app.platform, basis);
    app.target = RefreshTarget {
        settings: origin.settings,
    };
    app.session = Some(session);
    app.workflow = Some(workflow);
    app.projection = Projection::default();
    let problems = if diagnostics.is_empty() {
        String::new()
    } else {
        format!(", {} reported while reading", diagnostics.len())
    };
    app.status = if bootstrap {
        format!("installation read{problems}")
    } else {
        format!("installation re-read{problems}")
    };
}

/// Publishes a delivered load order through the accepted gate.
fn apply_sort_from(app: &mut App, completion: &JobCompletion) -> String {
    let Some(session) = app.session.as_mut() else {
        return "a sort arrived with nothing loaded".to_string();
    };
    match apply_sort_order(session, completion) {
        SortApplied::Published => "sorted".to_string(),
        SortApplied::Unchanged => "already in load order".to_string(),
        // A refusal has two distinct reasons, and they read differently to the user: the
        // sorter refused the list, or the order was computed for a list that has moved on.
        SortApplied::Refused => match &completion.outcome {
            JobOutcome::Validated(report) => match &report.order {
                Ok(_) => "the sort result no longer matches this list".to_string(),
                Err(error) => format!("this list cannot be sorted: {error}"),
            },
            _ => "the sort result could not be published".to_string(),
        },
    }
}

/// The guard every keyboard chord shares. A question in front of the user, or nothing loaded,
/// means no key may act: the chords are the same controls the buttons offer, never a way past
/// them or past a busy fact.
fn chord_guard(app: &mut App, what: &str) -> bool {
    if app.prompt.is_some() || app.copy.is_some() {
        app.status = format!("a question has to be answered before {what}");
        return false;
    }
    if app.session.is_none() {
        app.status = "nothing is loaded to work on".to_string();
        return false;
    }
    true
}

/// Plain Up/Down: moves the selection through the visible rows of the pane that asked, without
/// touching the list. With nothing selected yet, the first press picks the end it is heading to.
fn select_relative(app: &mut App, pane: &str, delta: i32) -> bool {
    if !chord_guard(app, "moving the selection") {
        return false;
    }
    let (Some(session), Some(generation)) = (
        app.session.as_ref(),
        app.session.as_ref().map(|s| s.generation()),
    ) else {
        return false;
    };
    let revision = session.revision();
    match pane {
        "active" => {
            let rows = app.projection.active();
            if rows.is_empty() {
                return false;
            }
            let here = app
                .selection_key()
                .and_then(|key| rows.iter().position(|row| row.key == key));
            let next = match here {
                Some(index) => {
                    let moved = index as i32 + delta;
                    if moved < 0 || moved as usize >= rows.len() {
                        return false;
                    }
                    moved as usize
                }
                None if delta < 0 => rows.len() - 1,
                None => 0,
            };
            app.selected = Some(Selection::Active {
                generation,
                revision,
                position: rows[next].position,
            });
            true
        }
        _ => {
            let rows = app.projection.inactive();
            if rows.is_empty() {
                return false;
            }
            let here = app
                .selection_key()
                .and_then(|key| rows.iter().position(|row| row.key == key));
            let next = match here {
                Some(index) => {
                    let moved = index as i32 + delta;
                    if moved < 0 || moved as usize >= rows.len() {
                        return false;
                    }
                    moved as usize
                }
                None if delta < 0 => rows.len() - 1,
                None => 0,
            };
            app.selected = Some(Selection::Inactive {
                generation,
                mod_id: rows[next].mod_id.clone(),
            });
            true
        }
    }
}

/// The selected active occurrence, moved one place. The move itself is the row command the
/// buttons already use, so the position, the remap and the refusals are the same.
fn move_selected(app: &mut App, delta: i32) -> bool {
    if !chord_guard(app, "moving a row") {
        return false;
    }
    if !matches!(app.selected, Some(Selection::Active { .. })) {
        app.status = "only an active row can be moved".to_string();
        return false;
    }
    let Some(key) = app.selection_key().map(|key| key.to_string()) else {
        app.status = "the selected row is not on screen to move".to_string();
        return false;
    };
    move_row(app, &key, delta)
}

/// Delete on the active pane removes the selected entry at its own position, which is how a
/// placeholder nothing can answer for is removed too.
fn delete_selected(app: &mut App) -> bool {
    if !chord_guard(app, "removing a row") {
        return false;
    }
    if !matches!(app.selected, Some(Selection::Active { .. })) {
        app.status = "only an active row can be removed".to_string();
        return false;
    }
    let Some(key) = app.selection_key().map(|key| key.to_string()) else {
        app.status = "the selected row is not on screen to remove".to_string();
        return false;
    };
    deactivate(app, &key)
}

/// Enter on the available pane activates the selected copy through the same route as the Add
/// button, including the copy question a duplicate has to raise.
fn activate_selected(app: &mut App) -> bool {
    if !chord_guard(app, "activating a copy") {
        return false;
    }
    if !matches!(app.selected, Some(Selection::Inactive { .. })) {
        app.status = "no available copy is selected to activate".to_string();
        return false;
    }
    let Some(key) = app.selection_key().map(|key| key.to_string()) else {
        app.status = "the selected copy is not on screen to activate".to_string();
        return false;
    };
    activate(app, &key)
}

/// Ctrl/Cmd+S: the workflow save, guarded exactly like the Save button, never the settings form.
fn keyboard_save(state: &Arc<State>) {
    let allowed = {
        let mut app = state
            .app
            .lock()
            .expect("the state lock is never held across a callback");
        let refusal = if app.prompt.is_some() || app.copy.is_some() {
            Some("a question has to be answered before saving")
        } else if app.session.is_none() {
            Some("nothing is loaded to save")
        } else if app.jobs.busy(JobSlot::Save) {
            Some("a save is already running")
        } else {
            None
        };
        let allowed = refusal.is_none();
        if let Some(refusal) = refusal {
            app.status = refusal.to_string();
        }
        let view = app.view();
        drop(app);
        if !allowed {
            paint(state, &view);
        }
        allowed
    };
    if allowed {
        request(state, RequestKind::Save);
    }
}

fn refused(stale: crate::ui::bridge::Stale, key: &str) -> String {
    match stale {
        crate::ui::bridge::Stale::Unknown => {
            format!("row {key} is not in the list this window drew")
        }
        crate::ui::bridge::Stale::Rebound => {
            format!("row {key} was drawn from a list that has since changed")
        }
    }
}

fn stage_word(stage: crate::application::jobs::JobStage) -> &'static str {
    match stage {
        JobStage::ReadingInstall => "reading folders",
        JobStage::ReadingConfig => "reading the list",
        JobStage::BuildingCatalog => "indexing copies",
        JobStage::Validating => "checking declarations",
        JobStage::Ordering => "ordering",
        JobStage::Writing => "writing",
        JobStage::Spawning => "starting",
    }
}

/// Applies one frame to the window, with no state lock held.
fn paint(state: &Arc<State>, view: &View) {
    let Some(window) = state.window.upgrade() else {
        return;
    };
    window.set_inactive_rows(ModelRc::from(Rc::new(VecModel::from(
        view.inactive.clone(),
    ))));
    window.set_active_rows(ModelRc::from(Rc::new(VecModel::from(view.active.clone()))));
    window.set_validation_findings(ModelRc::from(Rc::new(VecModel::from(
        view.validation.clone(),
    ))));
    window.set_reset_open(!view.reset_token.is_empty());
    window.set_reset_token(view.reset_token.as_str().into());
    window.set_reset_enabled(view.reset_enabled);
    window.set_selected_key(view.selected.as_str().into());
    window.set_inspector_title(view.inspector.title.as_str().into());
    window.set_inspector_source(view.inspector.source.as_str().into());
    window.set_inspector_package(view.inspector.package.as_str().into());
    window.set_inspector_version(view.inspector.version.as_str().into());
    window.set_inspector_description(view.inspector.description.as_str().into());
    window.set_inspector_diagnostics(view.inspector.diagnostics.as_str().into());
    window.set_dirty(view.dirty);
    window.set_read_only(view.read_only);
    window.set_busy_refresh(view.busy_refresh);
    window.set_busy_sort(view.busy_sort);
    window.set_busy_save(view.busy_save);
    window.set_busy_launch(view.busy_launch);
    match view.prompt.as_ref() {
        Some(prompt) => {
            window.set_prompt_open(true);
            window.set_prompt_title(prompt.title.as_str().into());
            window.set_prompt_detail(prompt.detail.as_str().into());
            window.set_prompt_token(prompt.token.as_str().into());
            window.set_prompt_allow_save(prompt.allow_save);
            window.set_prompt_allow_discard(prompt.allow_discard);
            window.set_prompt_allow_confirm(prompt.allow_confirm);
        }
        None => {
            window.set_prompt_open(false);
            window.set_prompt_token("".into());
            window.set_prompt_detail("".into());
            window.set_prompt_allow_save(false);
            window.set_prompt_allow_discard(false);
            window.set_prompt_allow_confirm(false);
        }
    }
    window.set_status(view.status.as_str().into());
    window.set_settings_note(view.note.as_str().into());
    window.set_set_game_root(view.draft.game_root.as_str().into());
    window.set_set_content_root(view.draft.content_root.as_str().into());
    window.set_set_config_path(view.draft.config_path.as_str().into());
    window.set_set_local_mods_root(view.draft.local_mods_root.as_str().into());
    window.set_set_workshop_root(view.draft.workshop_root.as_str().into());
    window.set_set_target_version(view.draft.target_version.as_str().into());
    window.set_set_executable_override(view.draft.executable_override.as_str().into());
    window.set_set_confirm_warnings(view.draft.confirm_warnings);
    match view.copy.as_ref() {
        Some(question) => {
            window.set_copy_open(true);
            window.set_copy_title(question.title.as_str().into());
            window.set_copy_detail(question.detail.as_str().into());
            window.set_copy_nonce(question.nonce.as_str().into());
        }
        None => {
            window.set_copy_open(false);
            window.set_copy_title("".into());
            window.set_copy_detail("".into());
            window.set_copy_nonce("".into());
        }
    }
    window.set_progress(view.progress.as_str().into());
}
