//! Behavioral tests for the generated workspace window and its adapter (Task 6 phase 6B).
//!
//! These run on the official Slint testing backend with its real event loop, and drive the
//! window only through its generated API: callbacks are invoked on the component, and results
//! are read back out of the models and properties the adapter paints. Nothing here calls a
//! domain method and calls that a UI test.
//!
//! The wait helpers **observe**. They never call `deliver`, apply a result or repaint: a
//! result reaching the window is proof of the production path — a worker posting, the host's
//! coalesced notice firing on the worker thread, `slint::invoke_from_event_loop` handing it
//! to the UI thread, and the adapter upgrading its weak handle. The watchdogs below exist
//! only so a delivery that never arrives fails the case instead of hanging it; they assert
//! nothing about how fast anything runs, and they report the window's own status line so an
//! operational failure says what happened rather than timing out blind.
//!
//! One `#[test]` function per binary, because the event-loop backend can be initialised only
//! once per process.

use std::fs;
use std::rc::Rc;
use std::time::Duration;

use rimsort_rust::application::workflow::RefreshTarget;
use rimsort_rust::integrations::launch::Platform;
use rimsort_rust::integrations::settings::Settings;
use rimsort_rust::ui::window::{Row, Startup, Workspace};
use slint::{Model as _, ModelRc};
use tempfile::TempDir;

/// How long a scenario lets the event loop run before it gives up. Cleanup only.
const WATCHDOG: Duration = Duration::from_secs(20);
/// How often the observer looks at the window's state. It changes nothing.
const OBSERVE: Duration = Duration::from_millis(25);

const DEPENDENT: &str = "dependent.mod";
const PROVIDER: &str = "provider.mod";
const CORE: &str = "base.game";

fn version() -> rimsort_rust::domain::types::GameVersion {
    rimsort_rust::domain::types::GameVersion::parse("1.5.4171").expect("the target must parse")
}

/// A scannable install: one official copy, a provider, and a copy that declares it loads
/// after the provider, with a config naming the two user copies in the wrong order.
struct Fixture {
    temp: TempDir,
    settings: Settings,
}

impl Fixture {
    fn new() -> Self {
        let temp = TempDir::new().expect("a temporary install");
        let game_root = temp.path().join("game");
        let content_root = game_root.join("Mods");
        let data_root = content_root.join("Data");
        let local_root = temp.path().join("Local Mods");
        let config = temp.path().join("config/ModsConfig.xml");
        for root in [&data_root, &local_root] {
            fs::create_dir_all(root).expect("the layout must be creatable");
        }
        fs::create_dir_all(config.parent().expect("the config has a parent"))
            .expect("the config folder must be creatable");
        fs::write(
            &config,
            format!(
                "<ModsConfigData><activeMods><li>{DEPENDENT}</li><li>{PROVIDER}</li>\
                 </activeMods></ModsConfigData>"
            ),
        )
        .expect("the config must be writable");
        for (root, name, package, after) in [
            (data_root.as_path(), "core", CORE, None),
            (local_root.as_path(), "provider", PROVIDER, None),
            (local_root.as_path(), "dependent", DEPENDENT, Some(PROVIDER)),
        ] {
            let about = root.join(name).join("About");
            fs::create_dir_all(&about).expect("the mod folder must be creatable");
            let load_after = after.map_or(String::new(), |package| {
                format!("<loadAfter><li>{package}</li></loadAfter>")
            });
            fs::write(
                about.join("About.xml"),
                format!(
                    "<ModMetaData><packageId>{package}</packageId><name>{name}</name>\
                     <supportedVersions><li>1.5</li></supportedVersions>{load_after}</ModMetaData>"
                ),
            )
            .expect("the metadata must be writable");
        }
        let settings = Settings {
            schema_version: 1,
            game_root: Some(game_root),
            content_root: Some(content_root),
            config_path: Some(config),
            local_mods_root: Some(local_root),
            workshop_root: None,
            target_version: Some(version()),
            executable_override: None,
            confirm_warnings: true,
        };
        Self { temp, settings }
    }

    fn startup(&self) -> Startup {
        Startup {
            target: RefreshTarget {
                settings: self.settings.clone(),
            },
            basis: Some(self.temp.path().to_path_buf()),
            platform: Platform::current().unwrap_or(Platform::Linux),
            settings_file: None,
        }
    }

    /// A candidate whose config file is simply not there.
    fn unreadable(&self) -> Startup {
        let mut broken = self.settings.clone();
        broken.config_path = Some(self.temp.path().join("nowhere/ModsConfig.xml"));
        Startup {
            target: RefreshTarget { settings: broken },
            basis: Some(self.temp.path().to_path_buf()),
            platform: Platform::current().unwrap_or(Platform::Linux),
            settings_file: None,
        }
    }

    fn config_bytes(&self) -> Vec<u8> {
        fs::read(
            self.settings
                .config_path
                .as_ref()
                .expect("the fixture has a config"),
        )
        .expect("the config must be readable")
    }

    fn missing_config(&self) -> std::path::PathBuf {
        self.temp.path().join("nowhere/ModsConfig.xml")
    }
}

/// The generated API of the open window, reached only through the workspace's own access.
struct View<'a> {
    ws: &'a Workspace,
}

impl<'a> View<'a> {
    fn new(ws: &'a Workspace) -> Self {
        Self { ws }
    }

    fn row_titles(&self, rows: ModelRc<Row>) -> Vec<String> {
        (0..rows.row_count())
            .filter_map(|index| rows.row_data(index))
            .map(|row| row.title.to_string())
            .collect()
    }

    fn row_keys(&self, rows: ModelRc<Row>) -> Vec<String> {
        (0..rows.row_count())
            .filter_map(|index| rows.row_data(index))
            .map(|row| row.key.to_string())
            .collect()
    }

    fn active_titles(&self) -> Vec<String> {
        let ws = self.ws;
        ws.with_window(|window| self.row_titles(window.get_active_rows()))
    }

    fn inactive_titles(&self) -> Vec<String> {
        let ws = self.ws;
        ws.with_window(|window| self.row_titles(window.get_inactive_rows()))
    }

    fn active_keys(&self) -> Vec<String> {
        let ws = self.ws;
        ws.with_window(|window| self.row_keys(window.get_active_rows()))
    }

    fn inactive_keys(&self) -> Vec<String> {
        let ws = self.ws;
        ws.with_window(|window| self.row_keys(window.get_inactive_rows()))
    }

    fn read_only(&self) -> bool {
        let ws = self.ws;
        ws.with_window(|window| window.get_read_only())
    }

    fn dirty(&self) -> bool {
        let ws = self.ws;
        ws.with_window(|window| window.get_dirty())
    }

    fn status(&self) -> String {
        self.ws.status()
    }

    fn inspector_title(&self) -> String {
        let ws = self.ws;
        ws.with_window(|window| window.get_inspector_title().to_string())
    }

    fn activate(&self, key: &str) {
        let ws = self.ws;
        let key = key.to_string();
        ws.with_window(|window| window.invoke_activate_row(key.into()));
    }

    fn deactivate(&self, key: &str) {
        let ws = self.ws;
        let key = key.to_string();
        ws.with_window(|window| window.invoke_deactivate_row(key.into()));
    }

    fn move_row(&self, key: &str, delta: i32) {
        let ws = self.ws;
        let key = key.to_string();
        ws.with_window(|window| window.invoke_move_row(key.into(), delta));
    }

    fn sort(&self) {
        self.ws.with_window(|window| window.invoke_sort_list());
    }

    fn search_active(&self, text: &str) {
        let ws = self.ws;
        let text = text.to_string();
        ws.with_window(|window| window.invoke_active_searched(text.into()));
    }
}

/// Runs the loop until `ready` holds. The observer only reads state - it never delivers,
/// applies or repaints - and both timers live inside the awaited loop and are stopped before
/// it returns, so a successful wait leaves no watchdog to queue a later `quit_event_loop()`.
fn await_until(ws: &Workspace, ready: Rc<dyn Fn(&Workspace) -> bool>) -> Result<(), String> {
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
    watchdog.start(slint::TimerMode::SingleShot, WATCHDOG, {
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
        "the window never reached the awaited state; status {:?}, installed {:?}",
        ws.status(),
        ws.installed()
    ))
}

/// The production path must install without any help: the window is shown, the loop runs,
/// and only a worker's notice through the event loop can change what is visible.
#[test]
fn the_window_bootstraps_and_answers_its_generated_controls() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let fixture = Fixture::new();

    // 0. The window's component is alive the moment it is created: nothing has to be shown
    //    or delivered for the handle the UI owns to exist.
    let window = Workspace::open(&fixture.startup()).expect("the window must open");
    assert!(
        window.window_alive(),
        "the workspace must keep its own window component alive after `open` returns"
    );
    window.show().expect("the window must be shown");
    assert!(
        window.window_alive(),
        "showing the window keeps it alive, as the event loop needs a visible window"
    );

    // 1. Bootstrap: an explicit job under the captured candidate, installed only through the
    //    worker -> notice -> event-loop path.
    window.begin();
    let bootstrap = window
        .bootstrap_job()
        .expect("begin must name its bootstrap job");
    assert!(
        !window.installed(),
        "nothing is installed before the read arrives"
    );
    assert!(
        View::new(&window).read_only(),
        "before anything is read the window is a read-only shell, not an editable guess"
    );

    await_until(
        &window,
        Rc::new(|ws: &Workspace| ws.installed() || ws.status_reported()),
    )
    .unwrap_or_else(|why| panic!("the bootstrap result must reach the window: {why}"));
    assert!(
        window.installed(),
        "the bootstrap read must install, and not fail: {}",
        window.status()
    );
    let view = View::new(&window);
    assert_eq!(
        window.active_tokens(),
        vec![DEPENDENT.to_string(), PROVIDER.to_string()],
        "the installed list is the candidate's own file, in its own order"
    );
    assert!(
        !view.read_only(),
        "an installation that was read is editable"
    );
    assert_eq!(
        view.active_titles(),
        vec!["dependent".to_string(), "provider".to_string()],
        "the active pane shows the loaded occurrences in load order"
    );
    assert_eq!(
        view.inactive_titles(),
        vec!["core".to_string()],
        "and the available pane shows the copies that are not active"
    );
    let stale_key = view.active_keys()[0].clone();
    let on_disk = fixture.config_bytes();
    let _ = bootstrap;

    // 2. A generated callback edits the session and the painted model follows it.
    let available = view.inactive_keys();
    view.activate(&available[0]);
    assert_eq!(
        window.active_tokens(),
        vec![
            DEPENDENT.to_string(),
            PROVIDER.to_string(),
            CORE.to_string()
        ],
        "activating through the callback adds the physical copy the row named, at the end"
    );
    assert!(view.dirty(), "and the pending edit is visible");
    assert_eq!(
        view.active_titles(),
        vec![
            "dependent".to_string(),
            "provider".to_string(),
            "core".to_string()
        ],
        "the pane was repainted from the same edit"
    );

    // 3. A key from the superseded projection is refused, not reinterpreted.
    view.move_row(&stale_key, 1);
    assert_eq!(
        window.active_tokens(),
        vec![
            DEPENDENT.to_string(),
            PROVIDER.to_string(),
            CORE.to_string()
        ],
        "an old projection's key must not move whatever row now sits at that index: {}",
        view.status()
    );

    // 4. Searching through a generated callback narrows a pane without renumbering the
    //    underlying list: every copy is active now, so the available pane is empty and the
    //    active pane is the one a term filters.
    assert!(
        view.inactive_titles().is_empty(),
        "the copy was just activated, so nothing is left available"
    );
    view.search_active("provider");
    assert_eq!(
        view.active_titles(),
        vec!["provider".to_string()],
        "the term keeps the one row it matches: {:?}",
        view.active_titles()
    );
    assert_eq!(
        window.active_tokens(),
        vec![
            DEPENDENT.to_string(),
            PROVIDER.to_string(),
            CORE.to_string()
        ],
        "and the session's own list is exactly what it was before the filter"
    );
    view.search_active("nomatch");
    assert!(
        view.active_titles().is_empty(),
        "a term that matches nothing leaves an empty pane, without inventing rows"
    );
    view.search_active("");
    assert_eq!(
        view.active_titles().len(),
        3,
        "clearing the term brings the same three occurrences back"
    );

    // 5. Sorting is a job; the order arrives through delivery, never a UI-thread sort.
    view.sort();
    await_until(
        &window,
        Rc::new(|ws| {
            ws.active_tokens()
                == vec![
                    CORE.to_string(),
                    PROVIDER.to_string(),
                    DEPENDENT.to_string(),
                ]
        }),
    )
    .unwrap_or_else(|why| {
        panic!(
            "the sort result must be delivered through the event loop: {why}; tokens now {:?}",
            window.active_tokens()
        )
    });
    assert_eq!(
        view.active_titles(),
        vec![
            "core".to_string(),
            "provider".to_string(),
            "dependent".to_string()
        ],
        "the painted order is the order the sorter produced"
    );

    // 6. Moving and removing act on the occurrence a key names.
    let ordered = view.active_keys();
    view.move_row(&ordered[2], -1);
    assert_eq!(
        window.active_tokens(),
        vec![
            CORE.to_string(),
            DEPENDENT.to_string(),
            PROVIDER.to_string()
        ],
        "moving the third row up one place reorders exactly those occurrences: {}",
        view.status()
    );
    let after_move = view.active_keys();
    view.deactivate(&after_move[1]);
    assert_eq!(
        window.active_tokens(),
        vec![CORE.to_string(), PROVIDER.to_string()],
        "and the row that key named is the row that went away"
    );

    // 7. Nothing was written anywhere: edits stay pending, the file keeps its loaded bytes.
    assert_eq!(
        fixture.config_bytes(),
        on_disk,
        "an edited window never writes the game's configuration by itself"
    );
    assert!(
        view.dirty(),
        "and the pending state is still what the user sees"
    );
    assert_eq!(
        view.inspector_title(),
        "Nothing selected",
        "with no row selected the inspector says so rather than keeping an old row's text"
    );

    // 8. A candidate whose list cannot be read stays an error state, and creates nothing.
    let broken = Fixture::new();
    let second = Workspace::open(&broken.unreadable()).expect("a window must still open");
    assert!(
        second.window_alive(),
        "a window over a bad candidate is still alive"
    );
    second.show().expect("the window must be shown");
    second.begin();
    await_until(
        &second,
        Rc::new(|ws: &Workspace| ws.installed() || ws.status_reported()),
    )
    .expect("an operational failure must be reported, not swallowed");
    assert!(
        !second.installed(),
        "a candidate whose list cannot be read is never installed"
    );
    assert!(
        View::new(&second).read_only(),
        "and the window stays read-only instead of guessing: {}",
        View::new(&second).status()
    );
    assert!(
        !broken.missing_config().exists(),
        "and no configuration file was created to make the read succeed"
    );
}
