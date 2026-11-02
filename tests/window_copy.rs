//! Behavioral tests for explicit physical-copy replacement (phase 6B unit).
//!
//! Two installed copies of one package id cannot both be active, and the accepted session
//! refuses to pick one silently: activating the other copy is an explicit replacement of a
//! specific row. These cases drive that through the window's own generated callbacks on the
//! official Slint backend, and assert what the row list looked like before and after —
//! including that a stale answer is refused rather than reinterpreted, and that the window's
//! copy question can never overwrite the workflow's outstanding question.
//!
//! One `#[test]` per binary (the event-loop backend initialises once per process). The
//! observer only reads state; both timers are stopped before it returns.

use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use rimsort_rust::application::workflow::RefreshTarget;
use rimsort_rust::integrations::launch::Platform;
use rimsort_rust::integrations::settings::Settings;
use rimsort_rust::ui::window::{Startup, Workspace};
use slint::Model as _;
use tempfile::TempDir;

const OBSERVE: Duration = Duration::from_millis(25);
const WAIT: Duration = Duration::from_secs(10);

const DUP: &str = "dup.mod";
const OTHER: &str = "other.mod";

struct Fixture {
    temp: TempDir,
    config: PathBuf,
    settings: Settings,
}

impl Fixture {
    /// One package id installed twice — a local copy and a Workshop copy — plus a second,
    /// unrelated copy, and a loaded list that already activates the local one.
    fn new(confirm_warnings: bool) -> Self {
        let temp = TempDir::new().expect("a temporary install");
        let data = temp.path().join("game/Mods/Data");
        let local = temp.path().join("Local Mods");
        let workshop = temp.path().join("Workshop");
        let config = temp.path().join("config/ModsConfig.xml");
        for root in [&data, &local, &workshop] {
            fs::create_dir_all(root).expect("layout");
        }
        fs::create_dir_all(config.parent().expect("parent")).expect("config folder");
        fs::write(
            &config,
            format!("<ModsConfigData><activeMods><li>{DUP}</li></activeMods></ModsConfigData>"),
        )
        .expect("the config must be writable");
        for (root, name, package) in [
            (data.as_path(), "core", "base.game"),
            (local.as_path(), "dup", DUP),
            (workshop.as_path(), "dup", DUP),
            (local.as_path(), "other", OTHER),
        ] {
            let about = root.join(name).join("About");
            fs::create_dir_all(&about).expect("mod folder");
            fs::write(
                about.join("About.xml"),
                format!(
                    "<ModMetaData><packageId>{package}</packageId><name>{name}-{parent}</name>\
                     <supportedVersions><li>1.5</li></supportedVersions></ModMetaData>",
                    parent = if root.ends_with("Workshop") {
                        "ws"
                    } else {
                        "local"
                    }
                ),
            )
            .expect("metadata");
        }
        let settings = Settings {
            schema_version: 1,
            game_root: Some(temp.path().join("game")),
            content_root: Some(temp.path().join("game/Mods")),
            config_path: Some(config.clone()),
            local_mods_root: Some(temp.path().join("Local Mods")),
            workshop_root: Some(temp.path().join("Workshop")),
            target_version: Some(
                rimsort_rust::domain::types::GameVersion::parse("1.5.4171").expect("parses"),
            ),
            executable_override: None,
            confirm_warnings,
        };
        Self {
            temp,
            config,
            settings,
        }
    }

    fn startup(&self) -> Startup {
        Startup {
            target: RefreshTarget {
                settings: self.settings.clone(),
            },
            basis: Some(self.temp.path().to_path_buf()),
            platform: Platform::Linux,
            settings_file: None,
        }
    }

    fn config_text(&self) -> String {
        fs::read_to_string(&self.config).expect("the config must be readable")
    }

    /// A candidate whose loaded list carries a token no copy can answer, so validation has
    /// something to report and the workflow's warning question can be raised.
    fn with_unresolved(self) -> Self {
        fs::write(
            &self.config,
            "<ModsConfigData><activeMods><li>dup.mod</li><li>gone.mod</li>             </activeMods></ModsConfigData>",
        )
        .expect("the config must be rewritable");
        self
    }
}

/// Reads of the window's own state, all through the generated API.
struct View<'a> {
    ws: &'a Workspace,
}

impl<'a> View<'a> {
    fn new(ws: &'a Workspace) -> Self {
        Self { ws }
    }

    fn copy_open(&self) -> bool {
        let ws = self.ws;
        ws.with_window(|window| window.get_copy_open())
    }

    fn copy_text(&self) -> String {
        let ws = self.ws;
        ws.with_window(|window| format!("{} {}", window.get_copy_title(), window.get_copy_detail()))
    }

    fn copy_nonce(&self) -> String {
        let ws = self.ws;
        ws.with_window(|window| window.get_copy_nonce().to_string())
    }

    fn prompt_open(&self) -> bool {
        let ws = self.ws;
        ws.with_window(|window| window.get_prompt_open())
    }

    fn prompt_token(&self) -> String {
        let ws = self.ws;
        ws.with_window(|window| window.get_prompt_token().to_string())
    }

    fn dirty(&self) -> bool {
        let ws = self.ws;
        ws.with_window(|window| window.get_dirty())
    }

    fn status(&self) -> String {
        self.ws.status()
    }

    fn inactive_keys_by_detail(&self, wanted: &str) -> Vec<String> {
        let ws = self.ws;
        ws.with_window(|window| {
            let rows = window.get_inactive_rows();
            (0..rows.row_count())
                .filter_map(|index| rows.row_data(index))
                .filter(|row| row.detail.to_string().contains(wanted))
                .map(|row| row.key.to_string())
                .collect()
        })
    }
}

fn answer_copy(ws: &Workspace, nonce: &str, choice: &str) {
    let (nonce, choice) = (nonce.to_string(), choice.to_string());
    ws.with_window(|window| window.invoke_answer_copy(nonce.into(), choice.into()));
}

fn activate(ws: &Workspace, key: &str) {
    let key = key.to_string();
    ws.with_window(|window| window.invoke_activate_row(key.into()));
}

/// Runs the loop until `ready` holds. The observer only reads state — it never delivers,
/// applies or repaints — and both timers live inside the awaited loop and are stopped before
/// it returns, so nothing can queue a late quit for the next wait.
fn await_until(
    phase: &str,
    ws: &Workspace,
    ready: Rc<dyn Fn(&Workspace) -> bool>,
) -> Result<(), String> {
    let done = Rc::new(std::cell::Cell::new(false));
    if ready(ws) {
        return Ok(());
    }
    let ticking = Rc::clone(&ready);
    let observer = slint::Timer::default();
    let watchdog = slint::Timer::default();
    observer.start(slint::TimerMode::Repeated, OBSERVE, {
        let done = Rc::clone(&done);
        let ws = ws.clone();
        move || {
            if ticking(&ws) {
                done.set(true);
                let _ = slint::quit_event_loop();
            }
        }
    });
    watchdog.start(slint::TimerMode::SingleShot, WAIT, {
        let done = Rc::clone(&done);
        move || {
            if !done.get() {
                let _ = slint::quit_event_loop();
            }
        }
    });
    slint::run_event_loop().expect("the event loop must run");
    observer.stop();
    watchdog.stop();
    if done.get() || ready(ws) {
        return Ok(());
    }
    Err(format!(
        "{phase}: never reached; status {:?}, copy open {:?}, prompt open {:?}",
        ws.status(),
        View::new(ws).copy_open(),
        View::new(ws).prompt_open()
    ))
}

fn wait_copy_open(phase: &str, ws: &Workspace) -> String {
    await_until(
        phase,
        ws,
        Rc::new(|ws: &Workspace| View::new(ws).copy_open()),
    )
    .unwrap_or_else(|why| panic!("{why}"));
    View::new(ws).copy_nonce()
}

/// Keys are handed out per projection, so a test must read the current one each time rather
/// than caching a key across a repaint - exactly what a stale callback must not be able to do.
fn key_for(ws: &Workspace, detail: &str) -> String {
    View::new(ws)
        .inactive_keys_by_detail(detail)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no available row whose detail contains {detail:?}"))
}

fn open_and_install(fixture: &Fixture) -> Workspace {
    let ws = Workspace::open(&fixture.startup()).expect("the window must open");
    ws.show().expect("the window must be shown");
    ws.begin();
    await_until("bootstrap", &ws, Rc::new(|ws: &Workspace| ws.installed()))
        .unwrap_or_else(|why| panic!("{why}"));
    ws
}

#[test]
fn the_window_asks_before_it_replaces_a_physical_copy() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let fixture = Fixture::new(false);
    let ws = open_and_install(&fixture);
    let view = View::new(&ws);
    let on_disk = fixture.config_text();
    assert_eq!(ws.active_tokens(), vec![DUP.to_string()]);
    assert!(!view.dirty(), "the loaded list is clean");

    // 1. Choosing the Workshop copy of a package whose Local copy is active is not an
    //    addition: the window has to ask, naming both copies and where they came from.
    let workshop = key_for(&ws, "workshop");
    activate(&ws, &workshop);
    let nonce = wait_copy_open("copy question", &ws);
    let text = view.copy_text();
    assert!(
        text.contains("dup-local") && text.contains("dup-ws") && text.contains("workshop"),
        "the question must show the row's copy, the asked-for copy and their sources: {text}"
    );
    assert_eq!(
        ws.active_tokens(),
        vec![DUP.to_string()],
        "asking changes nothing"
    );
    assert!(!view.dirty(), "and it is not an edit");
    assert!(
        !view.prompt_open(),
        "the workflow has no question outstanding here"
    );

    // 2. Keeping the current copy mutates nothing at all.
    answer_copy(&ws, &nonce, "cancel");
    await_until(
        "copy cancelled",
        &ws,
        Rc::new(|ws: &Workspace| !View::new(ws).copy_open()),
    )
    .expect("the question closes");
    assert_eq!(
        ws.active_tokens(),
        vec![DUP.to_string()],
        "cancelling replaced nothing"
    );
    assert!(!view.dirty());
    assert_eq!(fixture.config_text(), on_disk, "and nothing was written");

    // 3. Confirming replaces that one row, through the accepted session verb, and writes
    //    the token that encodes exactly the requested physical copy.
    activate(&ws, &key_for(&ws, "workshop"));
    let nonce = wait_copy_open("copy question again", &ws);
    answer_copy(&ws, &nonce, "replace");
    await_until(
        "copy replaced",
        &ws,
        Rc::new(|ws: &Workspace| ws.active_tokens() == vec![format!("{DUP}_steam")]),
    )
    .unwrap_or_else(|why| panic!("{why}"));
    assert!(view.dirty(), "the replacement is a pending edit");
    assert_eq!(
        fixture.config_text(),
        on_disk,
        "a replacement is still not a save"
    );

    // 4. An answer to a question the user has since moved past is refused, not reinterpreted
    //    onto whatever row now sits at the same position.
    activate(&ws, &key_for(&ws, "local"));
    let stale_nonce = wait_copy_open("second copy question", &ws);
    activate(&ws, &key_for(&ws, "other.mod"));
    let after_intervening_edit = ws.active_tokens();
    assert_eq!(
        after_intervening_edit,
        vec![format!("{DUP}_steam"), OTHER.to_string()],
        "the unrelated activation went through normally"
    );
    answer_copy(&ws, &stale_nonce, "replace");
    await_until(
        "stale question cleared",
        &ws,
        Rc::new(|ws: &Workspace| !View::new(ws).copy_open()),
    )
    .expect("a stale question is cleared rather than left blocking the window");
    assert_eq!(
        ws.active_tokens(),
        after_intervening_edit,
        "answering a stale copy question must not touch any row"
    );
    assert!(
        view.status().contains("no longer") || view.status().contains("replaced"),
        "and it says so: {:?}",
        view.status()
    );

    // 5. A fresh question can still be raised afterwards - clearing the stale one did not
    //    wedge the window.
    activate(&ws, &key_for(&ws, "local"));
    let fresh = wait_copy_open("fresh copy question", &ws);
    assert_ne!(fresh, stale_nonce, "a new question is a new identity");
    answer_copy(&ws, &fresh, "cancel");

    // 6. The workflow's outstanding question is never displaced by a copy request.
    let warned = Fixture::new(true).with_unresolved();
    let ws = open_and_install(&warned);
    let view = View::new(&ws);
    ws.with_window(|window| window.invoke_save());
    await_until(
        "warning question",
        &ws,
        Rc::new(|ws: &Workspace| View::new(ws).prompt_open()),
    )
    .expect("the workflow asks about its findings first");
    let prompt_token = view.prompt_token();
    activate(&ws, &key_for(&ws, "workshop"));
    assert!(
        !view.copy_open(),
        "a copy request cannot stack over the workflow's question: {:?}",
        view.status()
    );
    assert!(
        view.prompt_open() && view.prompt_token() == prompt_token,
        "the workflow's own question is untouched, same identity"
    );
    ws.with_window(|w| w.invoke_answer_prompt(prompt_token.clone().into(), "confirm".into()));
    await_until(
        "workflow question answered",
        &ws,
        Rc::new(|ws: &Workspace| !View::new(ws).prompt_open() && !View::new(ws).dirty()),
    )
    .expect("the workflow question can still be answered normally");
    std::mem::drop(ws);
}
