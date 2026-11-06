//! Keyboard control of the two lists (phase 6B input unit).
//!
//! These cases do not call a "keyboard command" hook: they put the window's own platform events
//! on the queue - modifier presses, key presses, pointer clicks - and let Slint route them to
//! whatever item holds focus. That is the same routing a desktop backend feeds the item tree, so
//! the assertions are about the focused pane, the modifier state and the guard rails, not about a
//! callback we invoked ourselves. It is still headless platform-event routing, which is a
//! different claim from a real window-manager key press; the two are reported apart.
//!
//! Row *selection* is set up through the same generated callback a row click already uses; the
//! keys under test are always delivered as window events.
//!
//! One `#[test]` per binary, official event-loop backend, read-only observer with both timers
//! stopped before it returns.

use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use rimsort_rust::application::workflow::RefreshTarget;
use rimsort_rust::integrations::launch::Platform;
use rimsort_rust::integrations::settings::Settings;
use rimsort_rust::ui::window::{Startup, Workspace};
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle as _, LogicalPosition, Model as _};
use tempfile::TempDir;

const OBSERVE: Duration = Duration::from_millis(25);
const WAIT: Duration = Duration::from_secs(10);

/// Rows are 42 logical pixels tall, so a click somewhere inside the first row's band selects it.
const ROW_BANDS: [f32; 6] = [60.0, 78.0, 96.0, 114.0, 132.0, 150.0];

struct Fixture {
    temp: TempDir,
    config: PathBuf,
    settings: Settings,
}

impl Fixture {
    /// Three active rows - two installed and one placeholder no copy can answer - plus available
    /// copies, one of which duplicates an active package id so activating it has to ask.
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
            "<ModsConfigData><activeMods><li>alpha.mod</li><li>gone.mod</li><li>beta.mod</li>\
             </activeMods></ModsConfigData>",
        )
        .expect("the config must be writable");
        for (root, name, package) in [
            (data.as_path(), "core", "base.game"),
            (local.as_path(), "alpha", "alpha.mod"),
            (local.as_path(), "beta", "beta.mod"),
            (local.as_path(), "dup", "dup.mod"),
            (workshop.as_path(), "dup", "dup.mod"),
            (local.as_path(), "gamma", "gamma.mod"),
        ] {
            let about = root.join(name).join("About");
            fs::create_dir_all(&about).expect("mod folder");
            fs::write(
                about.join("About.xml"),
                format!(
                    "<ModMetaData><packageId>{package}</packageId><name>Copy {name}</name>\
                     <supportedVersions><li>1.5</li></supportedVersions></ModMetaData>"
                ),
            )
            .expect("metadata");
        }
        let settings = Settings {
            schema_version: 1,
            game_root: Some(temp.path().join("game")),
            content_root: Some(temp.path().join("game/Mods")),
            config_path: Some(config.clone()),
            local_mods_root: Some(local),
            workshop_root: Some(workshop),
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
    let (busy, acquisition_status) =
        ws.with_acquisition(|window| (window.get_busy(), window.get_status().to_string()));
    Err(format!(
        "{phase}: never reached; status {:?}; acquisition busy={busy}, status={acquisition_status:?}",
        ws.status()
    ))
}

/// One event on the window's own queue: Slint, not the test, decides who sees it.
fn send(ws: &Workspace, event: WindowEvent) {
    let handle = ws.clone();
    handle.with_window(|inner| inner.window().dispatch_event(event.clone()));
}

fn press_key(ws: &Workspace, which: Key) {
    send(ws, WindowEvent::KeyPressed { text: which.into() });
    send(ws, WindowEvent::KeyReleased { text: which.into() });
}

fn letter(ws: &Workspace, text: &str) {
    send(ws, WindowEvent::KeyPressed { text: text.into() });
    send(ws, WindowEvent::KeyReleased { text: text.into() });
}

/// Holds Alt across the key, the way a backend reports it.
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

/// Holds Control across the letter. Slint maps the macOS Command key to this same modifier, so
/// this is the chord a Cmd press arrives as as well.
fn ctrl(ws: &Workspace, text: &str) {
    send(
        ws,
        WindowEvent::KeyPressed {
            text: Key::Control.into(),
        },
    );
    letter(ws, text);
    send(
        ws,
        WindowEvent::KeyReleased {
            text: Key::Control.into(),
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

macro_rules! reader {
    ($name:ident, $getter:ident, $ty:ty) => {
        fn $name(ws: &Workspace) -> $ty {
            let handle = ws.clone();
            handle.with_window(|window| window.$getter().into())
        }
    };
}

reader!(selected, get_selected_key, String);
reader!(inactive_search, get_inactive_search, String);
reader!(active_search, get_active_search, String);
reader!(copy_open, get_copy_open, bool);
reader!(busy_save, get_busy_save, bool);
reader!(dirty, get_dirty, bool);

fn prompt_open(ws: &Workspace) -> bool {
    let handle = ws.clone();
    handle.with_window(|window| window.get_prompt_open())
}

fn prompt_token(ws: &Workspace) -> String {
    let handle = ws.clone();
    handle.with_window(|window| window.get_prompt_token().to_string())
}

fn keys_of(ws: &Workspace, property: &str) -> Vec<String> {
    let handle = ws.clone();
    handle.with_window(|window| {
        let rows = if property == "active" {
            window.get_active_rows()
        } else {
            window.get_inactive_rows()
        };
        (0..rows.row_count())
            .map(|index| rows.row_data(index).expect("row").key.to_string())
            .collect()
    })
}

fn startup_recovered(ws: &Workspace, phase: &str) -> Result<(), String> {
    await_until(
        phase,
        ws,
        Rc::new(|ws| ws.with_acquisition(|window| !window.get_busy())),
    )?;
    let (status, question) = ws.with_acquisition(|window| {
        (
            window.get_status().to_string(),
            window.get_question_token().to_string(),
        )
    });
    if !status.is_empty() || !question.is_empty() {
        return Err(format!(
            "{phase}: startup recovery did not succeed: {status:?}, question={question:?}"
        ));
    }
    if !ws.with_window(|window| window.get_reset_enabled()) {
        return Err(format!("{phase}: startup still protects list operations"));
    }
    Ok(())
}

fn install(fixture: &Fixture, phase: &str) -> Workspace {
    let ws = Workspace::open(&fixture.startup()).expect("the window must open");
    ws.show().expect("the window must be shown");
    ws.begin();
    await_until(phase, &ws, Rc::new(|ws: &Workspace| ws.installed()))
        .unwrap_or_else(|why| panic!("{why}"));
    startup_recovered(&ws, phase).unwrap_or_else(|why| panic!("{why}"));
    ws
}

/// Clicks until a pointer press inside the given pane's first row is honoured, which is also how
/// the pane acquires keyboard focus. Returns the y that worked.
fn click_first_row(ws: &Workspace, pane: &str) -> f32 {
    let expect = keys_of(ws, pane).into_iter().next().expect("a row");
    for y in ROW_BANDS {
        let x = if pane == "active" { 400.0 } else { 100.0 };
        click(ws, x, y);
        if selected(ws) == expect {
            return y;
        }
    }
    panic!("no click in the {pane} pane's first row band selected it");
}

#[test]
fn keys_reach_the_focused_pane_and_nothing_else() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let fixture = Fixture::new(false);
    let ws = install(&fixture, "bootstrap");
    assert_eq!(
        ws.active_tokens(),
        vec!["alpha.mod", "gone.mod", "beta.mod"]
    );

    // 1. The window opens with the active pane holding the keyboard, so a plain Down moves the
    //    selection through that pane's visible rows and changes nothing else.
    let rows = keys_of(&ws, "active");
    // With nothing picked yet, the first Down takes the row it is heading towards...
    press_key(&ws, Key::DownArrow);
    assert_eq!(
        selected(&ws),
        rows[0],
        "Down has to select a visible row of the focused pane, status {:?}",
        ws.status()
    );
    // ...and every press after that steps one row further, up and down.
    press_key(&ws, Key::DownArrow);
    assert_eq!(selected(&ws), rows[1], "and the next Down steps one row on");
    press_key(&ws, Key::UpArrow);
    assert_eq!(selected(&ws), rows[0], "and Up steps back");
    assert_eq!(
        ws.active_tokens(),
        vec!["alpha.mod", "gone.mod", "beta.mod"]
    );
    assert!(!dirty(&ws), "selecting is not an edit");

    // 2. Alt+Up moves that occurrence, and the selection keeps following the row it moved.
    press_key(&ws, Key::DownArrow);
    assert_eq!(
        selected(&ws),
        rows[1],
        "the placeholder row is selected now"
    );
    alt(&ws, Key::UpArrow);
    assert_eq!(
        ws.active_tokens(),
        vec!["gone.mod", "alpha.mod", "beta.mod"],
        "Alt+Up moves the selected row up one place, status {:?}",
        ws.status()
    );
    assert_eq!(
        selected(&ws),
        keys_of(&ws, "active")[0],
        "the selection follows the row that moved"
    );
    alt(&ws, Key::UpArrow);
    assert_eq!(
        ws.active_tokens(),
        vec!["gone.mod", "alpha.mod", "beta.mod"],
        "the top row cannot move past the top"
    );

    // 3. Delete removes the selected active entry at its own position - here the placeholder
    //    no copy can answer for, which has nothing else to identify it by.
    press_key(&ws, Key::Delete);
    assert_eq!(
        ws.active_tokens(),
        vec!["alpha.mod", "beta.mod"],
        "Delete removed the selected row and not its neighbour, status {:?}",
        ws.status()
    );
    assert!(
        selected(&ws).is_empty(),
        "and the selection does not stay on a row that is gone"
    );

    // 4. Typing belongs to a text field, and while a field holds the keyboard no row action may
    //    fire. A field commits its text on Return, which is how the keystrokes become readable -
    //    and it also shows Return is the field's own key here, not the pane's activate.
    let list_before = ws.active_tokens();
    let selection_before = selected(&ws);
    let mut focused_field = None;
    for (x, name) in [(100.0, "available search"), (420.0, "active search")] {
        for y in [18.0, 26.0, 34.0] {
            click(&ws, x, y);
            letter(&ws, "q");
            press_key(&ws, Key::Return);
            if inactive_search(&ws).as_str() == "q" || active_search(&ws).as_str() == "q" {
                focused_field = Some(name);
                break;
            }
        }
        if focused_field.is_some() {
            break;
        }
    }
    let focused_field = focused_field.expect("a search field took the typed text and committed it");
    assert_eq!(
        ws.active_tokens(),
        list_before,
        "typing and committing in {focused_field} touched no row"
    );
    assert_eq!(
        selected(&ws),
        selection_before,
        "and did not move the selection"
    );
    for which in [Key::UpArrow, Key::DownArrow, Key::Delete, Key::Return] {
        press_key(&ws, which);
        alt(&ws, Key::UpArrow);
    }
    assert_eq!(
        ws.active_tokens(),
        list_before,
        "no row action may fire while {focused_field} has the focus"
    );
    assert_eq!(
        selected(&ws),
        selection_before,
        "and none of them may move the selection"
    );

    // 4b. The one chord that *is* global: the save reaches the workflow even from a text field,
    //     because it is taken on the way in, and nothing else is taken there.
    assert!(dirty(&ws), "the keyboard delete above left a pending edit");
    ctrl(&ws, "s");
    await_until(
        "save from a field",
        &ws,
        Rc::new(move |ws: &Workspace| !busy_save(ws) && !dirty(ws)),
    )
    .unwrap_or_else(|why| panic!("{why}"));
    assert!(
        fixture
            .config_text()
            .contains(&format!("<li>{}</li>", list_before[0])),
        "and it wrote the game list"
    );

    // Put the filter back the way it was, through the window's own state.
    let handle = ws.clone();
    handle.with_window(|window| window.invoke_inactive_searched("".into()));
    let handle = ws.clone();
    handle.with_window(|window| window.invoke_active_searched("".into()));

    // 5. Enter on an available copy goes through the route the Add button uses. For a package
    //    that is not active yet it activates it; for the second physical copy of a package that
    //    *is* active it has to raise the copy question rather than swap it in.
    let row_y = click_first_row(&ws, "available");
    let available = keys_of(&ws, "available");
    let index_of = |ws: &Workspace, wanted: &str| -> usize {
        let handle = ws.clone();
        handle.with_window(|window| {
            let rows = window.get_inactive_rows();
            (0..rows.row_count())
                .find(|index| {
                    rows.row_data(*index)
                        .is_some_and(|row| row.detail.contains(wanted))
                })
                .expect("a row with that source")
        })
    };
    let local = index_of(&ws, "local · dup.mod");
    click(&ws, 100.0, row_y + 42.0 * local as f32);
    assert_eq!(
        selected(&ws),
        available[local],
        "the local copy is selected"
    );
    let before_activate = ws.active_tokens();
    press_key(&ws, Key::Return);
    assert_eq!(
        ws.active_tokens().len(),
        before_activate.len() + 1,
        "Enter activated it through the session, status {:?}",
        ws.status()
    );

    // Now the other copy of the same package id: Enter must ask, not replace.
    let available = keys_of(&ws, "available");
    let workshop = index_of(&ws, "workshop · dup.mod");
    click(&ws, 100.0, row_y + 42.0 * workshop as f32);
    assert_eq!(
        selected(&ws),
        available[workshop],
        "the duplicate copy's own row is selected"
    );
    let before_question = ws.active_tokens();
    press_key(&ws, Key::Return);
    assert!(
        copy_open(&ws),
        "Enter on a duplicate copy has to raise the copy question, status {:?}",
        ws.status()
    );
    assert_eq!(
        ws.active_tokens(),
        before_question,
        "and asking changes nothing"
    );

    // 6. Neither a move nor the save chord may get around an outstanding question.
    let on_disk = fixture.config_text();
    alt(&ws, Key::UpArrow);
    ctrl(&ws, "s");
    assert!(
        copy_open(&ws),
        "the copy question is still the question it was"
    );
    assert!(!busy_save(&ws), "and no save started behind it");
    assert_eq!(
        ws.active_tokens(),
        before_question,
        "and Alt+Up did not move a row while a question is open"
    );
    assert_eq!(
        fixture.config_text(),
        on_disk,
        "and the chord wrote nothing"
    );
    let nonce = {
        let handle = ws.clone();
        handle.with_window(|window| window.get_copy_nonce().to_string())
    };
    let handle = ws.clone();
    handle.with_window(|window| window.invoke_answer_copy(nonce.clone().into(), "cancel".into()));
    assert!(!copy_open(&ws), "and the answer closes it");

    // 7. With the way clear, the chords work again: select, delete, save - and the delete has to
    //    hit the occurrence that is selected, not the row next to it.
    let row_y = click_first_row(&ws, "active");
    let rows = keys_of(&ws, "active");
    press_key(&ws, Key::DownArrow);
    assert_eq!(selected(&ws), rows[1], "arrows still walk the pane");
    // Back to the top row, which with no filter on is the list's first occurrence.
    click(&ws, 400.0, row_y);
    assert_eq!(
        selected(&ws),
        rows[0],
        "and the first row is selected again"
    );
    let before = ws.active_tokens();
    press_key(&ws, Key::Delete);
    let expected = ws.active_tokens();
    assert_eq!(
        expected,
        before[1..],
        "Delete removed exactly the selected occurrence, not its neighbour"
    );
    ctrl(&ws, "s");
    await_until(
        "save completed",
        &ws,
        Rc::new(move |ws: &Workspace| !busy_save(ws) && !dirty(ws)),
    )
    .unwrap_or_else(|why| panic!("{why}"));
    let written = fixture.config_text();
    assert_ne!(written, on_disk, "the save chord reached the game config");
    for token in &expected {
        assert!(
            written.contains(&format!("<li>{token}</li>")),
            "and wrote exactly the list the window holds: {written}"
        );
    }
    // 8. A question the workflow itself asked is the same wall: no chord gets past it, and the
    //    question survives unchanged until it is answered.
    let warned = Fixture::new(true);
    let ws = install(&warned, "warned bootstrap");
    ctrl(&ws, "s");
    await_until(
        "warning question",
        &ws,
        Rc::new(|ws: &Workspace| prompt_open(ws)),
    )
    .expect("the workflow asks about its findings before saving");
    let token = prompt_token(&ws);
    let list_before = ws.active_tokens();
    let on_disk = warned.config_text();
    for which in [Key::DownArrow, Key::Delete, Key::Return] {
        press_key(&ws, which);
        alt(&ws, Key::UpArrow);
        ctrl(&ws, "s");
    }
    assert!(
        prompt_open(&ws) && prompt_token(&ws) == token,
        "the workflow's question is still the same question"
    );
    assert_eq!(
        ws.active_tokens(),
        list_before,
        "and no chord moved, deleted or activated a row behind it"
    );
    assert_eq!(
        warned.config_text(),
        on_disk,
        "and nothing was written while it was outstanding"
    );
    let handle = ws.clone();
    handle
        .with_window(|window| window.invoke_answer_prompt(token.clone().into(), "confirm".into()));
    await_until(
        "question answered",
        &ws,
        Rc::new(|ws: &Workspace| !prompt_open(ws) && !dirty(ws)),
    )
    .expect("and it can still be answered normally");
    std::mem::drop(ws);

    let broken = Fixture::new(false);
    fs::write(
        broken.temp.path().join(".rimsort-next-Local Mods"),
        b"not a management directory",
    )
    .expect("the owned malformed management entry must be created");
    let ws = Workspace::open(&broken.startup()).expect("the broken fixture still opens");
    ws.show().expect("the window must be shown");
    ws.begin();
    await_until(
        "broken startup installed",
        &ws,
        Rc::new(|ws| ws.installed()),
    )
    .expect("the session can install before root recovery fails");
    let error = startup_recovered(&ws, "broken startup recovery")
        .expect_err("idle recovery with a terminal error is not fixture readiness");
    assert!(error.contains("Recovery required:"), "{error}");
}
