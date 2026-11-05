//! Typing into the settings form with real pointer and key events (phase 6B form-input unit).
//!
//! The form was previously proven only through its generated `edit-field` callback, which is not
//! the same claim as a user clicking a field, typing into it, and acting on a button. Here every
//! character, Tab, Home/End/Delete/Backspace and click goes through the window's own event queue,
//! and the oracle is always an effect the application had to produce: the settings file the service
//! writes, the list the window then reads, the game config a save rewrote. Draft getters are used to
//! aim the input and to check what a field holds mid-edit, never to claim anything was persisted.
//!
//! Field and button positions are not guessed and hard-coded: they are found by clicking, typing a
//! unique marker and seeing which field or effect answered, and every later interaction aims through
//! that map. One `#[test]` per binary, official event-loop backend, read-only observer with both
//! timers stopped before it returns.

use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use rimsort_rust::application::workflow::RefreshTarget;
use rimsort_rust::domain::types::GameVersion;
use rimsort_rust::integrations::launch::Platform;
use rimsort_rust::integrations::settings::{Settings, load_settings, save_settings};
use rimsort_rust::ui::window::{DraftView, Startup, Workspace};
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle as _, LogicalPosition};
use tempfile::TempDir;

const OBSERVE: Duration = Duration::from_millis(25);
const WAIT: Duration = Duration::from_secs(10);

/// The seven text fields, in the order the form declares them.
const FIELDS: [&str; 7] = [
    "game-root",
    "content-root",
    "config-path",
    "local-mods-root",
    "workshop-root",
    "executable-override",
    "target-version",
];

/// Where a click is tried, top to bottom, inside the third pane.
/// Click bands tried down the third pane, from the first row to below the button row.
const Y_BELTS: &[f32] = &[
    20.0, 28.0, 36.0, 44.0, 52.0, 60.0, 68.0, 76.0, 84.0, 92.0, 100.0, 108.0, 116.0, 124.0, 132.0,
    140.0, 148.0, 156.0, 164.0, 172.0, 180.0, 188.0, 196.0, 204.0, 212.0, 220.0, 228.0, 236.0,
    244.0, 252.0, 260.0, 268.0, 276.0, 284.0, 292.0, 300.0, 308.0, 316.0, 324.0, 332.0, 340.0,
    348.0, 356.0, 364.0, 372.0, 380.0, 388.0, 396.0, 404.0, 412.0, 420.0, 428.0, 436.0, 444.0,
    452.0, 460.0, 468.0, 476.0, 484.0,
];
const X_FIELD: &[f32] = &[940.0, 1010.0, 1080.0];
const X_WIDE: &[f32] = &[680.0, 740.0, 800.0, 880.0, 980.0, 1080.0];

struct Fixture {
    temp: TempDir,
    config_a: PathBuf,
    config_b: PathBuf,
    settings_file: PathBuf,
}

impl Fixture {
    /// Install A is what the window loads; install B is what the form gets typed towards. B's names
    /// carry spaces and non-ASCII characters so a lost or rewritten character cannot pass unseen.
    fn new() -> Self {
        let temp = TempDir::new().expect("a temporary install");
        let root = temp.path();
        let game_b = root.join("game 二/beta game");
        let config_a = root.join("config/ModsConfig.xml");
        let config_b = game_b.join("config 二/beta mods.xml");
        let local_b = game_b.join("Mods/local 二");
        for dir in [
            root.join("game/Mods/Data"),
            root.join("Local Mods"),
            game_b.join("Mods/Data"),
            local_b.clone(),
            root.join("config"),
            config_b.parent().expect("b config parent").to_path_buf(),
        ] {
            fs::create_dir_all(dir).expect("layout");
        }
        fs::write(
            &config_a,
            "<ModsConfigData><activeMods><li>alpha.mod</li></activeMods></ModsConfigData>",
        )
        .expect("config a");
        fs::write(
            &config_b,
            "<ModsConfigData><activeMods><li>beta.mod</li></activeMods></ModsConfigData>",
        )
        .expect("config b");
        for (dir, package, name) in [
            (root.join("game/Mods/Data"), "core.mod", "core"),
            (root.join("Local Mods"), "alpha.mod", "alpha"),
            (game_b.join("Mods/Data"), "core2.mod", "core-two"),
            (local_b.clone(), "beta.mod", "beta"),
        ] {
            let about = dir.join(name).join("About");
            fs::create_dir_all(&about).expect("mod folder");
            fs::write(
                about.join("About.xml"),
                format!(
                    "<ModMetaData><packageId>{package}</packageId><name>{name}</name>\
                     <supportedVersions><li>1.5</li></supportedVersions></ModMetaData>"
                ),
            )
            .expect("metadata");
        }
        let settings_file = root.join("rimsort/settings.json");
        fs::create_dir_all(settings_file.parent().expect("settings dir")).expect("settings dir");
        save_settings(&settings_file, &install_a(root, &config_a)).expect("settings a");
        Self {
            temp,
            config_a,
            config_b,
            settings_file,
        }
    }

    fn root(&self) -> &Path {
        self.temp.path()
    }

    fn startup(&self) -> Startup {
        Startup {
            target: RefreshTarget {
                settings: load_settings(&self.settings_file).expect("the settings must load"),
            },
            basis: Some(self.root().to_path_buf()),
            platform: Platform::Linux,
            settings_file: Some(self.settings_file.clone()),
        }
    }

    fn settings(&self) -> Settings {
        load_settings(&self.settings_file).expect("the written settings must load")
    }

    fn settings_bytes(&self) -> Vec<u8> {
        fs::read(&self.settings_file).expect("the settings file must be readable")
    }

    fn game_b(&self) -> PathBuf {
        self.root().join("game 二/beta game")
    }

    fn typed(&self) -> [(&'static str, String); 5] {
        let game = self.game_b();
        [
            ("game-root", game.to_string_lossy().into_owned()),
            (
                "content-root",
                game.join("Mods").to_string_lossy().into_owned(),
            ),
            (
                "config-path",
                game.join("config 二/beta mods.xml")
                    .to_string_lossy()
                    .into_owned(),
            ),
            (
                "local-mods-root",
                game.join("Mods/local 二").to_string_lossy().into_owned(),
            ),
            ("target-version", "1.5.4171".to_string()),
        ]
    }
}

fn install_a(root: &Path, config_a: &Path) -> Settings {
    Settings {
        schema_version: 1,
        game_root: Some(root.join("game")),
        content_root: Some(root.join("game/Mods")),
        config_path: Some(config_a.to_path_buf()),
        local_mods_root: Some(root.join("Local Mods")),
        workshop_root: None,
        target_version: Some(GameVersion::parse("1.5.4171").expect("parses")),
        executable_override: None,
        confirm_warnings: false,
    }
}

fn field_value<'a>(view: &'a DraftView, name: &str) -> &'a str {
    match name {
        "game-root" => view.game_root.as_str(),
        "content-root" => view.content_root.as_str(),
        "config-path" => view.config_path.as_str(),
        "local-mods-root" => view.local_mods_root.as_str(),
        "workshop-root" => view.workshop_root.as_str(),
        "executable-override" => view.executable_override.as_str(),
        "target-version" => view.target_version.as_str(),
        _ => "",
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
    Err(format!("{phase}: never reached; status {:?}", ws.status()))
}

fn send(ws: &Workspace, event: WindowEvent) {
    let handle = ws.clone();
    handle.with_window(|inner| inner.window().dispatch_event(event.clone()));
}

fn press_key(ws: &Workspace, which: Key) {
    send(ws, WindowEvent::KeyPressed { text: which.into() });
    send(ws, WindowEvent::KeyReleased { text: which.into() });
}

/// One real key press per character, spaces and non-ASCII included.
fn type_text(ws: &Workspace, text: &str) {
    for ch in text.chars() {
        let mut one = String::new();
        one.push(ch);
        send(
            ws,
            WindowEvent::KeyPressed {
                text: one.clone().into(),
            },
        );
        send(ws, WindowEvent::KeyReleased { text: one.into() });
    }
}

fn ctrl(ws: &Workspace, letter: &str) {
    send(
        ws,
        WindowEvent::KeyPressed {
            text: Key::Control.into(),
        },
    );
    type_text(ws, letter);
    send(
        ws,
        WindowEvent::KeyReleased {
            text: Key::Control.into(),
        },
    );
}

fn alt(ws: &Workspace, which: Key) {
    send(
        ws,
        WindowEvent::KeyPressed {
            text: Key::Alt.into(),
        },
    );
    press_key(ws, which);
    send(
        ws,
        WindowEvent::KeyReleased {
            text: Key::Alt.into(),
        },
    );
}

fn click(ws: &Workspace, x: f32, y: f32) {
    let position = LogicalPosition::new(x, y);
    send(
        ws,
        WindowEvent::PointerPressed {
            position,
            button: PointerEventButton::Left,
        },
    );
    send(
        ws,
        WindowEvent::PointerReleased {
            position,
            button: PointerEventButton::Left,
        },
    );
}

fn selected(ws: &Workspace) -> String {
    let handle = ws.clone();
    handle.with_window(|window| window.get_selected_key().to_string())
}

fn dirty(ws: &Workspace) -> bool {
    let handle = ws.clone();
    handle.with_window(|window| window.get_dirty())
}

fn busy_save(ws: &Workspace) -> bool {
    let handle = ws.clone();
    handle.with_window(|window| window.get_busy_save())
}

fn warnings_checked(ws: &Workspace) -> bool {
    let handle = ws.clone();
    handle.with_window(|window| window.get_set_confirm_warnings())
}

fn copy_open(ws: &Workspace) -> bool {
    let handle = ws.clone();
    handle.with_window(|window| window.get_copy_open())
}

fn prompt_open(ws: &Workspace) -> bool {
    let handle = ws.clone();
    handle.with_window(|window| window.get_prompt_open())
}

/// Clears the named field through Home/Delete, types `value`, and commits it with Return.
fn retype(ws: &Workspace, name: &str, y: f32, value: &str) {
    for &x in X_FIELD {
        let total = field_value(&ws.draft(), name).chars().count();
        click(ws, x, y);
        press_key(ws, Key::Home);
        for _ in 0..total {
            press_key(ws, Key::Delete);
        }
        type_text(ws, value);
        press_key(ws, Key::Return);
        if field_value(&ws.draft(), name) == value {
            return;
        }
    }
    panic!(
        "no click at y {y} let typed text become {name}'s whole value; status {:?}",
        ws.status()
    );
}

/// Finds the y band whose click puts the keyboard into each field, by typing a marker that is
/// unique per belt and asking which field the committed marker ended up in.
fn discover_fields(ws: &Workspace) -> Vec<(&'static str, f32)> {
    let mut found: Vec<(&'static str, f32)> = Vec::new();
    for &y in Y_BELTS {
        if found.len() == FIELDS.len() {
            break;
        }
        for &x in X_FIELD {
            let marker = format!("#{y}#{x}");
            let before: Vec<(&'static str, String)> = FIELDS
                .iter()
                .map(|name| (*name, field_value(&ws.draft(), name).to_string()))
                .collect();
            click(ws, x, y);
            type_text(ws, &marker);
            press_key(ws, Key::Return);
            let hit = before
                .iter()
                .find(|(name, _)| field_value(&ws.draft(), name).contains(&marker));
            if let Some((name, original)) = hit {
                // Take the marker back out, leaving the field exactly as it was found: the caret
                // is wherever the click put it, so the whole value is cleared and retyped rather
                // than a count of the characters that were added.
                let total = field_value(&ws.draft(), name).chars().count();
                click(ws, x, y);
                press_key(ws, Key::Home);
                for _ in 0..total {
                    press_key(ws, Key::Delete);
                }
                type_text(ws, original);
                press_key(ws, Key::Return);
                assert_eq!(
                    field_value(&ws.draft(), name),
                    before
                        .iter()
                        .find(|(seen, _)| seen == name)
                        .map(|(_, value)| value.as_str())
                        .expect("the field was read above"),
                    "erasing the probe marker must leave {name} exactly as it was"
                );
                if !found.iter().any(|(seen, _)| *seen == *name) {
                    found.push((*name, y));
                }
                break;
            }
        }
    }
    found
}

/// Clicks until a press produces `effect`, which is how the buttons are found: by the outcome they
/// are meant to have, never by a guessed position. Returns the click that produced it.
fn discover_button(
    ws: &Workspace,
    fixture: &Fixture,
    effect: impl Fn(&Workspace, &Fixture) -> bool,
) -> ((f32, f32), bool) {
    for &y in Y_BELTS {
        for &x in X_WIDE {
            let warnings_before_click = warnings_checked(ws);
            click(ws, x, y);
            if effect(ws, fixture) {
                return ((x, y), warnings_before_click);
            }
        }
    }
    panic!("no click in the form produced that effect");
}

#[test]
fn typing_into_the_form_reaches_the_settings_service() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let fixture = Fixture::new();
    let ws = Workspace::open(&fixture.startup()).expect("the window must open");
    ws.show().expect("the window must be shown");
    ws.begin();
    await_until("bootstrap", &ws, Rc::new(|ws: &Workspace| ws.installed()))
        .unwrap_or_else(|why| panic!("{why}"));
    assert_eq!(ws.active_tokens(), vec!["alpha.mod".to_string()]);
    let settings_at_start = fixture.settings_bytes();

    // 1. Every text field is reachable by a click and takes typed characters as its own text.
    let fields = discover_fields(&ws);
    let names: Vec<&str> = fields.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        names,
        FIELDS.to_vec(),
        "the form's seven text fields have to be reachable by typing into them"
    );
    assert_eq!(
        fixture.settings_bytes(),
        settings_at_start,
        "typing and committing alone must never write the settings file"
    );

    // 2. Row chords cannot fire while a text field holds the keyboard.
    let game_y = fields[0].1;
    click(&ws, X_FIELD[1], game_y);
    let tokens = ws.active_tokens();
    let selection = selected(&ws);
    for which in [Key::UpArrow, Key::DownArrow, Key::Delete, Key::Return] {
        press_key(&ws, which);
        alt(&ws, Key::UpArrow);
    }
    assert_eq!(
        ws.active_tokens(),
        tokens,
        "no row action may fire while a form field has the focus, status {:?}",
        ws.status()
    );
    assert_eq!(selected(&ws), selection, "and none may move the selection");

    // 3. The save chord writes the installed game list even while the typed draft points
    //    somewhere else, and never redirects to the settings file.
    let typed = fixture.typed();
    retype(&ws, "config-path", fields[2].1, &typed[2].1);
    let game_a_before = fs::read(&fixture.config_a).expect("config a");
    let game_b_before = fs::read(&fixture.config_b).expect("config b");
    ctrl(&ws, "s");
    await_until(
        "game save completed",
        &ws,
        Rc::new(move |ws: &Workspace| !busy_save(ws) && !dirty(ws)),
    )
    .unwrap_or_else(|why| panic!("{why}"));
    assert_eq!(
        fs::read(&fixture.config_b).expect("config b"),
        game_b_before,
        "the config the draft points at is not the one a save writes"
    );
    let game_a_after = fs::read(&fixture.config_a).expect("config a");
    assert!(
        String::from_utf8_lossy(&game_a_after).contains("<li>alpha.mod</li>"),
        "the save wrote the list the window holds into the installed config"
    );
    assert_eq!(
        game_a_after, game_a_before,
        "and nothing else in it moved, because the list had not changed"
    );
    assert_eq!(
        fixture.settings_bytes(),
        settings_at_start,
        "and the typed draft was never persisted by a game save"
    );

    // 4. An unfinished edit survives a progress paint: half a path, a real repaint, then the rest.
    let (head, tail) = typed[2].1.split_at(typed[2].1.len() / 2);
    let total = field_value(&ws.draft(), "config-path").chars().count();
    click(&ws, X_FIELD[1], fields[2].1);
    press_key(&ws, Key::Home);
    for _ in 0..total {
        press_key(&ws, Key::Delete);
    }
    type_text(&ws, head);
    ctrl(&ws, "s");
    await_until(
        "save during an unfinished edit",
        &ws,
        Rc::new(move |ws: &Workspace| !busy_save(ws)),
    )
    .unwrap_or_else(|why| panic!("{why}"));
    type_text(&ws, tail);
    press_key(&ws, Key::Return);
    assert_eq!(
        field_value(&ws.draft(), "config-path"),
        typed[2].1,
        "a progress paint must not erase half-typed input or move the caret"
    );

    // 5. Fill the whole form by typing, leave both optional roots blank, then act on the button.
    let warnings_before_typing = warnings_checked(&ws);
    for (name, value) in &typed {
        let y = fields
            .iter()
            .find(|(found, _)| *found == *name)
            .map(|(_, y)| *y)
            .expect("the field was found above");
        retype(&ws, name, y, value);
    }
    for name in ["workshop-root", "executable-override"] {
        let y = fields
            .iter()
            .find(|(found, _)| *found == name)
            .map(|(_, y)| *y)
            .expect("the field was found above");
        retype(&ws, name, y, "");
    }
    assert_eq!(
        fixture.settings_bytes(),
        settings_at_start,
        "still nothing written until the button is pressed"
    );
    assert_eq!(
        warnings_checked(&ws),
        warnings_before_typing,
        "typing into the fields never moved the warning checkbox"
    );
    let (save, warnings_before_save) = discover_button(&ws, &fixture, |_ws, fixture| {
        fixture.settings_bytes() != settings_at_start
    });
    // The discovery clicks can toggle the checkbox before reaching Save. Compare with the
    // state at the saving click, not the state before that pointer-driven search began.
    let warnings_at_save = warnings_checked(&ws);
    assert_eq!(
        warnings_at_save, warnings_before_save,
        "Save must not toggle the checkbox"
    );
    let saved = fixture.settings();
    assert_eq!(
        saved.game_root.as_deref(),
        Some(fixture.game_b().as_path()),
        "the typed game root, spaces and non-ASCII included"
    );
    assert_eq!(
        saved.content_root.as_deref(),
        Some(fixture.game_b().join("Mods").as_path()),
        "the typed content root"
    );
    assert_eq!(
        saved.config_path.as_deref(),
        Some(fixture.game_b().join("config 二/beta mods.xml").as_path()),
        "the typed config path, including the edit that spanned a paint"
    );
    assert_eq!(
        saved.local_mods_root.as_deref(),
        Some(fixture.game_b().join("Mods/local 二").as_path()),
        "the typed local mods root"
    );
    assert_eq!(
        saved.workshop_root, None,
        "a field typed to nothing stays absent, never a guessed path"
    );
    assert_eq!(saved.executable_override, None, "and so does the override");
    assert_eq!(
        saved.target_version.map(|v| v.to_string()),
        Some("1.5.4171".to_string()),
        "the typed version round-trips"
    );
    assert_eq!(
        saved.confirm_warnings, warnings_at_save,
        "and what persisted is the checkbox the window showed, not a value typing invented"
    );

    // 6. Saving settings persists and nothing else: the loaded installation keeps answering for
    //    its own captured target until the user asks for a re-read.
    assert_eq!(
        ws.active_tokens(),
        vec!["alpha.mod".to_string()],
        "writing the settings file must not silently reload the window"
    );
    // The button beside the one that saved is the re-read. It is found by the read it starts, on
    // the same row.
    let mut refresh = None;
    for &x in X_WIDE {
        if (x - save.0).abs() < 1.0 {
            continue;
        }
        click(&ws, x, save.1);
        let reached = await_until(
            "re-read the typed install",
            &ws,
            Rc::new(|ws: &Workspace| ws.active_tokens() == vec!["beta.mod".to_string()]),
        );
        if reached.is_ok() {
            refresh = Some((x, save.1));
            break;
        }
    }
    let refresh = refresh.unwrap_or_else(|| {
        panic!(
            "no click on the save button's row started a read of the \
         typed install; status {:?}",
            ws.status()
        )
    });
    assert_eq!(
        refresh.1, save.1,
        "the re-read lives on the same row as the save, where it was found"
    );
    assert_eq!(
        ws.active_tokens(),
        vec!["beta.mod".to_string()],
        "and the list on screen belongs to the install that was typed"
    );
    let good = fixture.settings_bytes();

    // 7. An invalid version is refused with the file and the loaded list untouched.
    retype(&ws, "target-version", fields[6].1, "9.9 not a version");
    click(&ws, save.0, save.1);
    assert_eq!(
        fixture.settings_bytes(),
        good,
        "a refused form never replaces the settings file, status {:?}",
        ws.status()
    );
    assert_eq!(
        ws.active_tokens(),
        vec!["beta.mod".to_string()],
        "and the loaded list is not disturbed by the refusal"
    );
    assert!(
        !prompt_open(&ws) && !copy_open(&ws),
        "and the refusal left no question behind"
    );
    std::mem::drop(ws);
}
