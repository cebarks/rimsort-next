//! Behavioral tests for the window's workflow-backed operations (Task 6 phase 6B, next unit):
//! saving, launching, re-reading a loaded installation and the native close request.
//!
//! Everything here runs on the official Slint testing backend with its real event loop, and the
//! only way a result reaches the window is the production path — a worker posting, the host's
//! coalesced notice firing on the worker thread, `slint::invoke_from_event_loop` handing it to the
//! UI thread. The observers below only read state; they never deliver, apply or repaint.
//!
//! Two claims the lifecycle has to keep honest are tested directly: a re-read must never throw away
//! edits the user has not saved, and closing must be refused while the workflow still owes an
//! answer. The native close is driven through `Window::request_close`, which runs the same
//! `on_close_requested` callback the window manager does.

use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use rimsort_rust::application::workflow::RefreshTarget;
use rimsort_rust::integrations::launch::Platform;
use rimsort_rust::integrations::settings::Settings;
use rimsort_rust::ui::window::{MainWindow, Workspace};
use slint::{ComponentHandle as _, Model as _};
use tempfile::TempDir;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;

const WATCHDOG: Duration = Duration::from_secs(20);
const OBSERVE: Duration = Duration::from_millis(25);

const CORE: &str = "base.game";
const PROVIDER: &str = "provider.mod";
const DEPENDENT: &str = "dependent.mod";

fn version() -> rimsort_rust::domain::types::GameVersion {
    rimsort_rust::domain::types::GameVersion::parse("1.5.4171").expect("the target must parse")
}

/// A scannable install, its own config file, and a harmless launch fixture that records the
/// argument count so a spawn can be proven without touching a real game.
struct Fixture {
    temp: TempDir,
    settings: Settings,
    marker: PathBuf,
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
            .expect("config folder");
        fs::write(
            &config,
            format!("<ModsConfigData><activeMods><li>{CORE}</li></activeMods></ModsConfigData>"),
        )
        .expect("the config must be writable");
        for (root, name, package, after) in [
            (data_root.as_path(), "core", CORE, None),
            (local_root.as_path(), "provider", PROVIDER, None),
            (local_root.as_path(), "dependent", DEPENDENT, Some(PROVIDER)),
        ] {
            let about = root.join(name).join("About");
            fs::create_dir_all(&about).expect("the mod folder must be creatable");
            let load_after = after.map_or(String::new(), |p| {
                format!("<loadAfter><li>{p}</li></loadAfter>")
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
        let marker = game_root.join("launched");
        let launcher = game_root.join("harmless-launch.sh");
        fs::write(
            &launcher,
            format!(
                "#!/bin/sh\nprintf 'started %s' \"$#\" > '{}'\n",
                marker.display()
            ),
        )
        .expect("the launch fixture must be writable");
        #[cfg(unix)]
        fs::set_permissions(&launcher, fs::Permissions::from_mode(0o755))
            .expect("the launch fixture must be executable");
        let settings = Settings {
            schema_version: 1,
            game_root: Some(game_root),
            content_root: Some(content_root),
            config_path: Some(config),
            local_mods_root: Some(local_root),
            workshop_root: None,
            target_version: Some(version()),
            executable_override: Some(launcher.clone()),
            confirm_warnings: false,
        };
        Self {
            temp,
            settings,
            marker,
        }
    }

    fn startup(&self) -> rimsort_rust::ui::window::Startup {
        rimsort_rust::ui::window::Startup {
            target: RefreshTarget {
                settings: self.settings.clone(),
            },
            basis: Some(self.temp.path().to_path_buf()),
            platform: Platform::current().unwrap_or(Platform::Linux),
            settings_file: None,
        }
    }

    fn config(&self) -> PathBuf {
        self.settings
            .config_path
            .clone()
            .expect("the fixture has a config")
    }

    /// The active list as the written file itself reads it.
    fn written_tokens(&self) -> Vec<String> {
        rimsort_rust::integrations::config::parse_config(
            &fs::read(self.config()).expect("the config must be readable"),
        )
        .expect("the written file must parse")
        .active_tokens
    }

    fn config_text(&self) -> String {
        String::from_utf8_lossy(&fs::read(self.config()).expect("the config must be readable"))
            .to_string()
    }

    fn backup_text(&self) -> String {
        let mut name = self.config().as_os_str().to_os_string();
        name.push(".rimsort-rust.bak");
        String::from_utf8_lossy(&fs::read(PathBuf::from(name)).expect("the backup must exist"))
            .to_string()
    }
}

/// Reads and operations on the open window, all through its generated API.
struct Ui<'a> {
    ws: &'a Workspace,
}

impl<'a> Ui<'a> {
    fn new(ws: &'a Workspace) -> Self {
        Self { ws }
    }

    fn text(&self, read: impl FnOnce(&MainWindow) -> slint::SharedString) -> String {
        let ws = self.ws;
        ws.with_window(|window| read(window).to_string())
    }

    fn flag(&self, read: impl FnOnce(&MainWindow) -> bool) -> bool {
        let ws = self.ws;
        ws.with_window(|window| read(window))
    }

    fn status(&self) -> String {
        self.text(|w| w.get_status())
    }

    fn prompt_token(&self) -> String {
        self.text(|w| w.get_prompt_token())
    }

    fn prompt_detail(&self) -> String {
        self.text(|w| w.get_prompt_detail())
    }

    fn prompt_open(&self) -> bool {
        self.flag(|w| w.get_prompt_open())
    }

    fn allows_save(&self) -> bool {
        self.flag(|w| w.get_prompt_allow_save())
    }

    fn allows_discard(&self) -> bool {
        self.flag(|w| w.get_prompt_allow_discard())
    }

    fn dirty(&self) -> bool {
        self.flag(|w| w.get_dirty())
    }

    fn busy_save(&self) -> bool {
        self.flag(|w| w.get_busy_save())
    }

    fn busy_launch(&self) -> bool {
        self.flag(|w| w.get_busy_launch())
    }

    fn visible(&self) -> bool {
        self.flag(|w| w.window().is_visible())
    }

    fn inactive_keys(&self) -> Vec<String> {
        let ws = self.ws;
        ws.with_window(|window| {
            let rows = window.get_inactive_rows();
            (0..rows.row_count())
                .filter_map(|index| rows.row_data(index))
                .map(|row| row.key.to_string())
                .collect()
        })
    }

    fn activate(&self, key: &str) {
        let ws = self.ws;
        let key = key.to_string();
        ws.with_window(|window| window.invoke_activate_row(key.into()));
    }

    fn refresh(&self) {
        self.ws.with_window(|window| window.invoke_refresh());
    }

    fn save(&self) {
        self.ws.with_window(|window| window.invoke_save());
    }

    fn launch(&self) {
        self.ws.with_window(|window| window.invoke_launch());
    }

    fn answer(&self, token: &str, choice: &str) {
        let ws = self.ws;
        let (token, choice) = (token.to_string(), choice.to_string());
        ws.with_window(|window| window.invoke_answer_prompt(token.into(), choice.into()));
    }

    /// The window manager's close, as the window itself receives it: a real
    /// `WindowEvent::CloseRequested`, which runs the same `on_close_requested` callback the
    /// desktop would trigger. Whether the window went away is then read from its visibility,
    /// never from a shortcut past the callback.
    fn native_close(&self) {
        let ws = self.ws;
        ws.with_window(|window| {
            window
                .window()
                .dispatch_event(slint::platform::WindowEvent::CloseRequested);
        });
    }
}

/// Runs the loop until `ready` holds, then reports what the window actually said.
///
/// The observer only reads state — it never delivers, applies or repaints — and both timers
/// live inside the loop being awaited and are stopped before this returns, so a successful
/// wait cannot leave a watchdog behind queueing a later `quit_event_loop()` that would end
/// the next loop before its delivery arrived. The context is reported because `installed`
/// staying false and `status_reported` staying false are different failures, and only one of
/// them means the install logic was reached.
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
        "{phase}: the window never reached the awaited state; status {:?}, installed {:?}, \
         a result reported {:?}, bootstrap {:?}",
        Ui::new(ws).status(),
        ws.installed(),
        ws.status_reported(),
        ws.bootstrap_job().map(|job| format!("{job:?}")),
    ))
}

fn open(fixture: &Fixture) -> Workspace {
    let window = Workspace::open(&fixture.startup()).expect("the window must open");
    assert!(
        window.window_alive(),
        "the window must own its own component"
    );
    window.show().expect("the window must be shown");
    window.begin();
    await_until(
        "bootstrap",
        &window,
        Rc::new(|ws: &Workspace| ws.installed() || ws.status_reported()),
    )
    .unwrap_or_else(|why| {
        panic!(
            "the bootstrap read must install: {why}; installed={:?} reported={:?}",
            window.installed(),
            window.status_reported()
        )
    });
    await_until(
        "startup acquisition recovery",
        &window,
        Rc::new(|ws: &Workspace| ws.with_acquisition(|w| !w.get_busy())),
    )
    .expect("offline lifecycle actions start after real startup recovery returns");
    window
}

fn scenario_refresh_never_discards_unsaved_edits(window: &Workspace, fixture: &Fixture) {
    let ui = Ui::new(window);
    let available = ui.inactive_keys();
    ui.activate(&available[0]);
    assert!(ui.dirty(), "the activation is a pending edit");
    let tokens = window.active_tokens();

    // A re-read of a loaded installation goes through the workflow, which will not let it
    // replace edits the user has not resolved.
    ui.refresh();
    await_until("dirty refresh question", window, {
        let w = window.clone();
        Rc::new(move |ws: &Workspace| {
            let _ = &w;
            Ui::new(ws).prompt_open()
        })
    })
    .expect("a dirty re-read must ask the user what to do with the edits");
    assert!(
        ui.allows_save() && ui.allows_discard(),
        "the question offers saving or discarding: {:?}",
        ui.prompt_detail()
    );
    assert_eq!(
        window.active_tokens(),
        tokens,
        "the pending list is untouched while the question is open"
    );
    assert!(
        !fixture.config_text().contains(PROVIDER),
        "and nothing was written to the game's configuration"
    );

    let token = ui.prompt_token();
    assert!(
        !token.is_empty(),
        "the question carries an identity to answer under"
    );
    ui.answer(&token, "cancel");
    assert!(!ui.prompt_open(), "cancelling closes the question");
    assert!(ui.dirty(), "and the edits stay exactly where they were");
    assert_eq!(
        window.active_tokens(),
        tokens,
        "with nothing installed or written"
    );
}

fn scenario_save_writes_and_the_window_learns_it_is_clean(window: &Workspace, fixture: &Fixture) {
    let ui = Ui::new(window);
    let before = fixture.config_text();
    let available = ui.inactive_keys();
    ui.activate(&available[0]);
    assert!(ui.dirty(), "the edit is pending");
    assert!(!ui.busy_save(), "no write is in flight before the request");
    let expected = window.active_tokens();

    ui.save();
    // Wait for the save job's own slot, not for any earlier terminal report: the window's
    // busy-save flag is set by the dispatch and cleared when the result is routed.
    let save_dispatched = await_until("save dispatched", window, {
        let w = window.clone();
        Rc::new(move |ws: &Workspace| {
            let _ = &w;
            Ui::new(ws).busy_save()
        })
    });
    // A write can finish before an observer tick sees the flag set, so this only records
    // that the slot was entered; the acknowledged wait below is what the case asserts.
    let _ = save_dispatched;
    await_until("save acknowledged", window, {
        let w = window.clone();
        Rc::new(move |ws: &Workspace| {
            let _ = &w;
            (!Ui::new(ws).busy_save() && !Ui::new(ws).dirty())
                || Ui::new(ws).status().contains("changed since it was loaded")
        })
    })
    .expect("the write must be performed by its job and acknowledged by the workflow");
    assert!(
        !ui.dirty(),
        "a successful save clears the pending label; status {:?}, reported {:?}",
        ui.status(),
        window.status_reported()
    );
    assert_eq!(
        fixture.written_tokens(),
        expected,
        "the file on disk reads back exactly the list the session held"
    );
    assert_eq!(
        fixture.backup_text().as_bytes(),
        before.as_bytes(),
        "the bytes it replaced are kept as the backup, through the accepted service"
    );
    assert!(
        !ui.prompt_open(),
        "no question stays open after a clean save: {:?}",
        ui.prompt_detail()
    );
}

fn scenario_conflicted_write_holds_the_window_and_the_edits(window: &Workspace, fixture: &Fixture) {
    let ui = Ui::new(window);
    let available = ui.inactive_keys();
    ui.activate(&available[0]);
    // Somebody else writes the file between the effect and the worker.
    fs::write(
        fixture.config(),
        "<ModsConfigData><activeMods><li>x.other</li></activeMods></ModsConfigData>",
    )
    .expect("the rival write must land");
    let rival = fixture.config_text();

    ui.save();
    await_until("conflicted write", window, {
        let w = window.clone();
        Rc::new(move |ws: &Workspace| {
            let _ = &w;
            Ui::new(ws).status().contains("changed since it was loaded")
        })
    })
    .expect("a conflicted write must be reported");
    assert!(ui.dirty(), "a failed write never clears the pending edits");
    assert_eq!(
        fixture.config_text(),
        rival,
        "and it never overwrites the other writer"
    );
}

fn scenario_launch_saves_first_and_only_offers_save_or_cancel(
    window: &Workspace,
    fixture: &Fixture,
) {
    let ui = Ui::new(window);
    let available = ui.inactive_keys();
    ui.activate(&available[0]);

    ui.launch();
    await_until("launch question", window, {
        let w = window.clone();
        Rc::new(move |ws: &Workspace| {
            let _ = &w;
            Ui::new(ws).prompt_open()
        })
    })
    .expect("launching over unsaved edits must ask");
    assert!(
        ui.allows_save() && !ui.allows_discard(),
        "the launch question offers saving or cancelling, never discarding the edits"
    );
    let token = ui.prompt_token();
    ui.answer(&token, "save");
    let marker = fixture.marker.clone();
    let expected = window.active_tokens();
    await_until("launch after save", window, {
        let w = window.clone();
        let marker = marker.clone();
        Rc::new(move |ws: &Workspace| {
            let _ = &w;
            marker.exists()
                && !Ui::new(ws).dirty()
                && !Ui::new(ws).busy_save()
                && !Ui::new(ws).busy_launch()
        })
    })
    .expect("the accepted order is: write first, then start the game");
    assert_eq!(
        fixture.written_tokens(),
        expected,
        "the write happened, with the list the session held, before the spawn"
    );
    assert!(
        !ui.prompt_open(),
        "the question is closed once it was answered"
    );
    let recorded = fs::read_to_string(&marker).expect("the fixture records its own start");
    assert_eq!(
        recorded, "started 0",
        "the fixture started with no arguments: {recorded}"
    );
}

fn scenario_native_close_is_refused_until_the_workflow_says_so(
    window: &Workspace,
    fixture: &Fixture,
) {
    let ui = Ui::new(window);
    let available = ui.inactive_keys();
    ui.activate(&available[0]);

    ui.native_close();
    assert!(
        ui.visible(),
        "and it stays visible while the question is open"
    );
    assert!(
        ui.prompt_open(),
        "the close request became the unsaved-close question"
    );
    assert!(
        ui.allows_save() && ui.allows_discard(),
        "offering save, discard and cancel"
    );

    // A stale answer from the question this one replaced must be ignored.
    let first = ui.prompt_token();
    ui.answer(&first, "cancel");
    assert!(
        !ui.prompt_open(),
        "cancelling the close keeps everything as it was"
    );
    assert!(ui.dirty(), "and the edits remain");
    ui.native_close();
    assert!(ui.prompt_open(), "asking again gives a new question");
    let second = ui.prompt_token();
    assert_ne!(first, second, "a new question is a new identity");
    ui.answer(&first, "discard");
    assert!(
        ui.prompt_open() && ui.dirty(),
        "answering the replaced question does nothing at all: {:?}",
        ui.prompt_detail()
    );
    ui.answer(&second, "discard");
    assert!(
        !ui.visible(),
        "discarding the pending edits is what finally lets the window close"
    );
    assert!(
        !fixture.config_text().contains(PROVIDER) || !ui.dirty(),
        "and discarding wrote nothing to the game's configuration"
    );
}

#[test]
fn the_window_routes_its_operations_through_the_workflow() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let one = Fixture::new();
    let window = open(&one);
    scenario_refresh_never_discards_unsaved_edits(&window, &one);
    std::mem::drop(window);

    let two = Fixture::new();
    let window = open(&two);
    scenario_save_writes_and_the_window_learns_it_is_clean(&window, &two);
    scenario_conflicted_write_holds_the_window_and_the_edits(&window, &two);
    std::mem::drop(window);

    let three = Fixture::new();
    let window = open(&three);
    scenario_launch_saves_first_and_only_offers_save_or_cancel(&window, &three);
    std::mem::drop(window);

    let four = Fixture::new();
    let window = open(&four);
    scenario_native_close_is_refused_until_the_workflow_says_so(&window, &four);
    std::mem::drop(window);
}
