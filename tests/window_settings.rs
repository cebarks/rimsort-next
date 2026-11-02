//! Behavioral tests for the configuration form and settings persistence (phase 6B unit).
//!
//! The form is the only way a first run gets paths: nothing may be auto-detected, guessed,
//! or written as a side effect. Everything here runs on the official Slint backend with its
//! real event loop and drives the generated form callbacks; observers only read state, and
//! both timers are stopped before the wait returns. Settings live in a fixture file passed
//! through `Options`, so no process-global directory is mutated.

use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use rimsort_rust::integrations::launch::Platform;
use rimsort_rust::integrations::settings::{Settings, load_settings, save_settings};
use rimsort_rust::ui::window::{Options, Workspace};
use slint::Model as _;
use tempfile::TempDir;

const WATCHDOG: Duration = Duration::from_secs(20);
const OBSERVE: Duration = Duration::from_millis(25);

const CORE: &str = "base.game";
const PROVIDER: &str = "provider.mod";

fn settings(basis: &Path, config: &Path, confirm: bool) -> Settings {
    Settings {
        schema_version: 1,
        game_root: Some(basis.join("game")),
        content_root: Some(basis.join("game/Mods")),
        config_path: Some(config.to_path_buf()),
        local_mods_root: Some(basis.join("Local Mods")),
        workshop_root: None,
        target_version: Some(
            rimsort_rust::domain::types::GameVersion::parse("1.5.4171").expect("parses"),
        ),
        executable_override: None,
        confirm_warnings: confirm,
    }
}

/// A scannable install whose loaded list is deliberately wrong-way-up, plus an unresolved
/// token so content findings exist for the warning preference to govern.
struct Fixture {
    temp: TempDir,
    config: PathBuf,
    settings_file: PathBuf,
    other_config: PathBuf,
}

impl Fixture {
    fn new(list: &str) -> Self {
        let temp = TempDir::new().expect("a temporary install");
        let data_root = temp.path().join("game/Mods/Data");
        let local_root = temp.path().join("Local Mods");
        let config = temp.path().join("config/ModsConfig.xml");
        let other_config = temp.path().join("other/ModsConfig.xml");
        for root in [&data_root, &local_root] {
            fs::create_dir_all(root).expect("layout");
        }
        fs::create_dir_all(config.parent().expect("parent")).expect("config folder");
        fs::create_dir_all(other_config.parent().expect("parent")).expect("other folder");
        fs::write(&config, list).expect("the config must be writable");
        fs::write(&other_config, list).expect("the other config must be writable");
        for (root, name, package, after) in [
            (data_root.as_path(), "core", CORE, None),
            (local_root.as_path(), "provider", PROVIDER, None),
            (
                local_root.as_path(),
                "dependent",
                "dependent.mod",
                Some(PROVIDER),
            ),
        ] {
            let about = root.join(name).join("About");
            fs::create_dir_all(&about).expect("mod folder");
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
            .expect("metadata");
        }
        let settings_file = temp.path().join("rimsort/settings.json");
        Self {
            temp,
            config,
            settings_file,
            other_config,
        }
    }

    fn options(&self, basis: Option<&Path>) -> Options {
        Options {
            settings_file: Some(self.settings_file.clone()),
            basis: basis
                .map(|b| b.to_path_buf())
                .or(Some(self.temp.path().to_path_buf())),
            platform: Some(Platform::Linux),
        }
    }

    fn config_text(&self) -> String {
        fs::read_to_string(&self.config).expect("the config must be readable")
    }

    fn settings_exists(&self) -> bool {
        self.settings_file.exists()
    }

    fn written_tokens(path: &Path) -> Vec<String> {
        rimsort_rust::integrations::config::parse_config(&fs::read(path).expect("a config"))
            .expect("the written config must parse")
            .active_tokens
    }

    fn configure(&self) -> Workspace {
        let window = Workspace::configure(&self.options(None)).expect("the window must open");
        window.show().expect("the window must be shown");
        window
    }
}

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
        "{phase}: never reached; status {:?}, note {:?}, installed {:?}",
        ws.status(),
        ws.settings_note(),
        ws.installed()
    ))
}

fn wait_installed(phase: &str, ws: &Workspace) {
    await_until(phase, ws, Rc::new(|ws: &Workspace| ws.installed()))
        .unwrap_or_else(|why| panic!("{why}"));
}

/// The first available row's key, for an edit that must be possible after the saves.
fn ui_keys(ws: &Workspace) -> Vec<String> {
    ws.with_window(|window| {
        let rows = window.get_inactive_rows();
        (0..rows.row_count())
            .filter_map(|index| rows.row_data(index))
            .map(|row| row.key.to_string())
            .collect()
    })
}

fn field_key(ws: &Workspace, key: &str) {
    let key = key.to_string();
    ws.with_window(|window| window.invoke_activate_row(key.into()));
}

fn busy_save(ws: &Workspace) -> bool {
    ws.with_window(|window| window.get_busy_save())
}

/// Issues a save and waits **that job's own** completion: the slot going busy, then the
/// installed file reading back exactly the expected list with the slot free again. Waiting on
/// `!dirty` alone is not a wait at all - a list that was already clean satisfies it before the
/// job even starts, and the next operation would then collide with a write still in flight.
fn save_and_wait(
    phase: &str,
    ws: &Workspace,
    installed: &Path,
    expected: &[String],
    backup: Option<&Path>,
) {
    assert!(
        !busy_save(ws),
        "{phase}: a save must not be issued while another write is still in flight"
    );
    let before_backup = backup.map(|p| fs::read(p).ok());
    ws.with_window(|window| window.invoke_save());
    let expected = expected.to_vec();
    let installed = installed.to_path_buf();
    await_until(phase, ws, {
        Rc::new(move |ws: &Workspace| {
            !busy_save(ws) && Fixture::written_tokens(&installed) == expected
        })
    })
    .unwrap_or_else(|why| panic!("{phase}: the save never completed on its own — {why}"));
    if let (Some(path), Some(before)) = (backup, before_backup) {
        match before {
            Some(bytes) => assert_eq!(
                fs::read(path).expect("the backup must be readable"),
                bytes,
                "the backup of a previous write is not disturbed by this one"
            ),
            None => assert!(
                path.exists(),
                "the first accepted write must leave a backup behind"
            ),
        }
    }
}

fn field(ws: &Workspace, name: &str, value: &str) {
    let (name, value) = (name.to_string(), value.to_string());
    ws.with_window(|window| window.invoke_edit_field(name.into(), value.into()));
}

#[test]
fn the_form_configures_the_window_and_the_service_persists_it() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let loaded = format!(
        "<ModsConfigData><activeMods><li>{DEPENDENT_TOKEN}</li><li>{PROVIDER}</li>\
         </activeMods></ModsConfigData>",
        DEPENDENT_TOKEN = "dependent.mod"
    );
    let fixture = Fixture::new(&loaded);
    fs::create_dir_all(fixture.settings_file.parent().expect("parent")).expect("settings dir");
    save_settings(
        &fixture.settings_file,
        &settings(fixture.temp.path(), &fixture.config, true),
    )
    .expect("the fixture settings must be writable");

    // 1. A configured run loads through the service and installs its candidate.
    let window = fixture.configure();
    wait_installed("configured bootstrap", &window);
    assert_eq!(
        window.active_tokens(),
        vec!["dependent.mod".to_string(), PROVIDER.to_string()],
        "the loaded list is the configured candidate's own file"
    );
    assert_eq!(
        window.draft().config_path,
        fixture.config.to_string_lossy(),
        "the form shows the path the settings service provided"
    );

    // 2. A first run with nothing configured configures nothing by itself.
    let empty = Fixture::new(
        "<ModsConfigData><activeMods><li>not.installed</li></activeMods></ModsConfigData>",
    );
    let window = empty.configure();
    await_until("first run reports it", &window, {
        let w = window.clone();
        Rc::new(move |ws: &Workspace| {
            let _ = &w;
            !ws.settings_note().is_empty() || ws.installed()
        })
    })
    .expect("a run with no settings must say so rather than sit silent");
    assert!(
        !window.installed(),
        "nothing can be installed without configured paths: {}",
        window.status()
    );
    assert!(
        !empty.settings_exists(),
        "and the window never writes a settings file on its own"
    );
    let mut created = Vec::new();
    for entry in walk(empty.temp.path()) {
        if entry.ends_with("ModsConfig.xml")
            && !entry.contains("config")
            && !entry.contains("other")
        {
            created.push(entry);
        }
    }
    assert!(
        created.is_empty(),
        "no game configuration is created anywhere to make a run work: {created:?}"
    );

    // 3. Filling the form is what enables the first read; empty optionals stay absent.
    field(
        &window,
        "game-root",
        &empty.temp.path().join("game").to_string_lossy(),
    );
    field(
        &window,
        "content-root",
        &empty.temp.path().join("game/Mods").to_string_lossy(),
    );
    field(
        &window,
        "config-path",
        &empty
            .temp
            .path()
            .join("config/ModsConfig.xml")
            .to_string_lossy(),
    );
    field(
        &window,
        "local-mods-root",
        &empty.temp.path().join("Local Mods").to_string_lossy(),
    );
    field(&window, "workshop-root", "");
    field(&window, "target-version", "1.5.4171");
    window.with_window(|w| w.invoke_refresh_from_form());
    wait_installed("form configured read", &window);
    assert!(
        window.draft().workshop_root.is_empty(),
        "an empty optional stays empty rather than becoming a guessed path"
    );
    assert!(
        !empty.settings_exists(),
        "reading through the form does not persist it; saving is a separate, asked-for act"
    );

    // 4. An invalid release is refused, and the installed installation is left alone.
    let installed = window.active_tokens();
    field(&window, "target-version", "not-a-release");
    window.with_window(|w| w.invoke_refresh_from_form());
    await_until("invalid release refused", &window, {
        let w = window.clone();
        Rc::new(move |ws: &Workspace| {
            let _ = &w;
            !ws.settings_note().is_empty()
        })
    })
    .expect("an invalid target version must be reported");
    assert_eq!(
        window.active_tokens(),
        installed,
        "a refused form value cannot replace the loaded list"
    );
    assert!(
        !empty.settings_exists(),
        "and a refused save must not write a settings file either"
    );
    window.with_window(|w| w.invoke_save_settings());
    await_until("refused save settles", &window, {
        let w = window.clone();
        Rc::new(move |ws: &Workspace| {
            let _ = &w;
            ws.settings_note().contains("version") || ws.settings_note().contains("target")
        })
    })
    .expect("saving an invalid form is refused with a reason");
    assert!(
        !empty.settings_exists(),
        "an invalid form is never persisted: {:?}",
        empty.settings_file
    );

    // 5. Saving the form persists it, and the warning preference governs the NEXT operation.
    let persisted_file = empty.settings_file.clone();
    field(&window, "target-version", "1.5.4171");
    window.with_window(|w| w.invoke_save_settings());
    await_until("settings persisted", &window, {
        let w = window.clone();
        Rc::new(move |ws: &Workspace| {
            let _ = &w;
            ws.settings_note().contains("saved") || persisted_file.exists()
        })
    })
    .expect("a valid form must persist through the accepted service");
    assert!(
        empty.settings_exists(),
        "saving settings writes the service's own file"
    );
    let persisted = load_settings(&empty.settings_file).expect("the written settings must load");
    assert_eq!(
        persisted.config_path.as_deref(),
        Some(empty.temp.path().join("config/ModsConfig.xml").as_path()),
        "the persisted candidate is the one the form named"
    );
    assert!(
        persisted.confirm_warnings,
        "the default preference is to ask, and it round-trips"
    );

    // 5. With findings and the preference on, a save asks first. Changing the preference while
    // that question is open must not rebuild or replace it: the same prompt identity has to
    // survive, and dismissing still writes nothing.
    let installed = empty.config.clone();
    let backup = {
        let mut name = installed.as_os_str().to_os_string();
        name.push(".rimsort-rust.bak");
        PathBuf::from(name)
    };
    let before = empty.config_text();
    window.with_window(|w| w.invoke_save());
    await_until("warning question", &window, {
        let w = window.clone();
        Rc::new(move |ws: &Workspace| {
            let _ = &w;
            ws.with_window(|win| win.get_prompt_open())
        })
    })
    .expect("a findings confirmation must appear while the preference is on");
    let token = window.with_window(|w| w.get_prompt_token().to_string());
    assert!(!token.is_empty(), "the question carries an identity");
    assert!(
        !busy_save(&window),
        "a warning question is asked before any write is dispatched"
    );

    // The preference is changed and persisted *while the question is still outstanding*.
    window.with_window(|w| w.invoke_toggle_warnings(false));
    window.with_window(|w| w.invoke_save_settings());
    await_until("preference persisted mid-question", &window, {
        let w = window.clone();
        Rc::new(move |ws: &Workspace| {
            let _ = &w;
            ws.settings_note().contains("saved")
        })
    })
    .expect("the form must still persist while the question is open");
    let persisted_mid = load_settings(&empty.settings_file).expect("the settings must load");
    assert!(
        !persisted_mid.confirm_warnings,
        "the new preference is what persisted"
    );
    assert_eq!(
        window.with_window(|w| w.get_prompt_token()).to_string(),
        token,
        "and the outstanding question keeps its own identity: the in-flight operation keeps the \
         preference it captured"
    );
    assert!(
        window.with_window(|w| w.get_prompt_open()),
        "the question is still the question it was"
    );

    window.with_window(|w| {
        w.invoke_answer_prompt(token.clone().into(), "dismiss".into());
    });
    await_until("dismiss settles", &window, {
        let w = window.clone();
        Rc::new(move |ws: &Workspace| {
            let _ = &w;
            !ws.with_window(|win| win.get_prompt_open())
        })
    })
    .expect("the question closes");
    assert_eq!(empty.config_text(), before, "dismissing wrote nothing");
    assert!(
        !busy_save(&window),
        "and no write was ever dispatched for it"
    );

    // The next operation is the one governed by the changed preference, and its own job is
    // what the wait follows.
    let expected = window.active_tokens();
    save_and_wait(
        "save with the preference off",
        &window,
        &installed,
        &expected,
        Some(&backup),
    );
    let persisted = load_settings(&empty.settings_file).expect("the settings must load");
    assert!(
        !persisted.confirm_warnings,
        "the next save went ahead without a question because the preference is now off"
    );
    assert!(
        !window.with_window(|w| w.get_prompt_open()),
        "and no question opened for it"
    );

    // 6. Retyping a path in the form cannot redirect the loaded installation's write.
    let other_before = fs::read(&empty.other_config).expect("the other file must be readable");
    field(
        &window,
        "config-path",
        &empty.other_config.to_string_lossy(),
    );
    assert_eq!(
        window.draft().config_path,
        empty.other_config.to_string_lossy(),
        "the form really shows the newly typed path"
    );
    let available = ui_keys(&window);
    assert!(!available.is_empty(), "some copy is still available to add");
    field_key(&window, &available[0]);
    let expected_now = window.active_tokens();
    assert_ne!(
        expected_now, expected,
        "and the list the save must write has changed"
    );
    save_and_wait(
        "save after the form was retargeted",
        &window,
        &installed,
        &expected_now,
        Some(&backup),
    );
    assert_eq!(
        fs::read(&empty.other_config).expect("the other file must still be readable"),
        other_before,
        "the path only typed into the form was never written by this operation"
    );
    assert!(
        window.with_window(|w| w.get_prompt_open()) || !window.with_window(|w| w.get_dirty()),
        "the operation ended one way the user can see: a question or a clean list"
    );
    std::mem::drop(window);
    let _ = fixture;

    // 7. A corrupt settings file is reported and left for the user to fix.
    let broken = Fixture::new(&loaded);
    fs::create_dir_all(broken.settings_file.parent().expect("parent")).expect("dir");
    fs::write(&broken.settings_file, "{\"targetVersion\": not json").expect("corrupt write");
    let corrupt_bytes = fs::read(&broken.settings_file).expect("bytes");
    let window = broken.configure();
    await_until("corrupt reported", &window, {
        let w = window.clone();
        Rc::new(move |ws: &Workspace| {
            let _ = &w;
            !ws.settings_note().is_empty() && !ws.installed()
        })
    })
    .expect("a corrupt settings file must be surfaced, not swallowed");
    assert!(
        !window.installed(),
        "nothing is installed from unreadable settings"
    );
    assert_eq!(
        fs::read(&broken.settings_file).expect("bytes"),
        corrupt_bytes,
        "and the user's file is untouched until they act"
    );

    // 8. No working directory is reported, not invented.
    let nobasis = Fixture::new(&loaded);
    save_settings(
        &nobasis.settings_file,
        &settings(nobasis.temp.path(), &nobasis.config, true),
    )
    .expect("settings must persist");
    let window = Workspace::configure(&Options {
        settings_file: Some(nobasis.settings_file.clone()),
        basis: None,
        platform: Some(Platform::Linux),
    })
    .expect("the window must still open");
    window.show().expect("the window must be shown");
    await_until("basis refused", &window, {
        let w = window.clone();
        Rc::new(move |ws: &Workspace| {
            let _ = &w;
            ws.status().contains("working directory")
                && ws.settings_note().contains("basis")
                && !ws.bootstrap_job().is_some()
        })
    })
    .expect("an unreadable working directory must be reported");
    assert!(
        !window.installed(),
        "no installation is loaded with a fabricated basis: {}",
        window.status()
    );
}

/// Paths under a directory, as strings, for the "nothing was created" checks.
fn walk(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Some(text) = path.to_str() {
                found.push(text.to_string());
            }
        }
    }
    found
}

/// Writes the smoke fixture through the accepted service and prints the path it resolved.
///
/// Ignored by default: it reads the process's `XDG_DATA_HOME`, so it is only run by hand for
/// the native fixture check, never as part of the suite, and it adds no shipped command-line
/// behaviour. The printed path is what the native run must then use — the fixture is never
/// placed at a hand-typed directory name.
#[test]
#[ignore]
fn write_smoke_fixture_settings() {
    let path = rimsort_rust::integrations::settings::settings_path()
        .expect("the settings path must resolve");
    let root = std::env::var("XDG_DATA_HOME").expect("the caller must set XDG_DATA_HOME");
    let root = Path::new(&root)
        .canonicalize()
        .expect("the redirected data root must exist");
    let resolved = path.parent().expect("the settings file has a parent");
    assert!(
        resolved.starts_with(&root),
        "the service resolved {} outside the owned root {}",
        resolved.display(),
        root.display()
    );
    let fixture =
        std::env::var("RIMSORT_SMOKE_FIXTURE").expect("the caller must set the fixture dir");
    let fixture = Path::new(&fixture)
        .canonicalize()
        .expect("the fixture dir must exist");
    save_settings(
        &path,
        &Settings {
            schema_version: 1,
            game_root: Some(fixture.join("game")),
            content_root: Some(fixture.join("game/Mods")),
            config_path: Some(fixture.join("config/ModsConfig.xml")),
            local_mods_root: Some(fixture.join("Local Mods")),
            workshop_root: None,
            target_version: Some(
                rimsort_rust::domain::types::GameVersion::parse("1.5.4171").expect("parses"),
            ),
            executable_override: None,
            confirm_warnings: false,
        },
    )
    .expect("the fixture settings must be writable through the service");
    println!("SETTINGS_PATH={}", path.display());
}
