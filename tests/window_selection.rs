//! Selection correctness across repaints and filtering (phase 6B prerequisite unit).
//!
//! Every row command the window takes is addressed by a row key, and the inspector, the
//! highlight and the keyboard commands all read the selected key back. These cases pin what
//! happens to that selection when the window repaints for a reason that has nothing to do
//! with the list, and when the active pane is filtered so that the visible row is no longer
//! the first underlying entry. They are the precondition for keyboard and drag work: a command
//! aimed at a stale or filtered-away row must either hit the same occurrence or refuse, never
//! silently edit a different one.
//!
//! One `#[test]` per binary, official event-loop backend, read-only observer with both timers
//! stopped before it returns.

use std::fs;

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

struct Fixture {
    temp: TempDir,
    settings: Settings,
}

impl Fixture {
    /// Two active mods whose names, sources and descriptions all differ, so a row can be
    /// told apart from its neighbour by anything the inspector shows.
    fn new() -> Self {
        let temp = TempDir::new().expect("a temporary install");
        let local = temp.path().join("Local Mods");
        let config = temp.path().join("config/ModsConfig.xml");
        fs::create_dir_all(&local).expect("layout");
        fs::create_dir_all(temp.path().join("game/Mods/Data")).expect("game content layout");
        fs::create_dir_all(config.parent().expect("parent")).expect("config folder");
        fs::write(
            &config,
            "<ModsConfigData><activeMods><li>alpha.mod</li><li>beta.mod</li>\
             </activeMods></ModsConfigData>",
        )
        .expect("the config must be writable");
        for (name, package, description, extra) in [
            (
                "alpha",
                "alpha.mod",
                "the alpha description",
                "<loadAfter><li>beta.mod</li></loadAfter>",
            ),
            ("beta", "beta.mod", "the beta description", ""),
        ] {
            let about = local.join(name).join("About");
            fs::create_dir_all(&about).expect("mod folder");
            fs::write(
                about.join("About.xml"),
                format!(
                    "<ModMetaData><packageId>{package}</packageId><name>Package {name}</name>\
                     <author>author-{name}</author>\
                     <description>{description}</description>\
                     <supportedVersions><li>1.5</li></supportedVersions>\
                     {extra}</ModMetaData>"
                ),
            )
            .expect("metadata");
        }
        let settings = Settings {
            schema_version: 1,
            game_root: Some(temp.path().join("game")),
            content_root: Some(temp.path().join("game/Mods")),
            config_path: Some(config),
            local_mods_root: Some(local),
            workshop_root: None,
            target_version: Some(
                rimsort_rust::domain::types::GameVersion::parse("1.5.4171").expect("parses"),
            ),
            executable_override: None,
            confirm_warnings: false,
        };
        Self { temp, settings }
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

/// The current keys of the active pane, in the order it is drawn.
fn active_keys(ws: &Workspace) -> Vec<String> {
    let ws = ws.clone();
    ws.with_window(|window| {
        let rows = window.get_active_rows();
        (0..rows.row_count())
            .map(|index| rows.row_data(index).expect("row").key.to_string())
            .collect()
    })
}

fn selection(ws: &Workspace) -> String {
    let ws = ws.clone();
    ws.with_window(|window| window.get_selected_key().to_string())
}

fn inspector(ws: &Workspace) -> String {
    let ws = ws.clone();
    ws.with_window(|window| {
        format!(
            "{}|{}|{}|{}",
            window.get_inspector_title(),
            window.get_inspector_source(),
            window.get_inspector_package(),
            window.get_inspector_description()
        )
    })
}

fn select(ws: &Workspace, key: &str) {
    let key = key.to_string();
    ws.with_window(|window| window.invoke_select_row(key.into()));
}

#[test]
fn a_selected_row_stays_the_same_row_across_repaints_and_filtering() {
    i_slint_backend_testing::init_integration_test_with_system_time();

    let fixture = Fixture::new();
    let ws = Workspace::open(&fixture.startup()).expect("the window must open");
    ws.show().expect("the window must be shown");
    ws.begin();
    await_until("bootstrap", &ws, Rc::new(|ws: &Workspace| ws.installed()))
        .unwrap_or_else(|why| panic!("{why}"));
    assert_eq!(ws.active_tokens(), vec!["alpha.mod", "beta.mod"]);

    // 1. Selecting the second row inspects that row.
    let second = active_keys(&ws)
        .into_iter()
        .nth(1)
        .expect("two active rows");
    select(&ws, &second);
    let text = inspector(&ws);
    assert!(
        text.contains("Package beta") && text.contains("the beta description"),
        "the inspector must describe the row that was picked, got {text}"
    );
    assert_eq!(
        selection(&ws),
        second,
        "and the window shows it as selected"
    );

    // 2. A repaint that has nothing to do with the list must not drop the selection. The
    //    warning preference only re-reads the frame, so the same row has to still be the
    //    selected one afterwards.
    ws.with_window(|window| window.invoke_toggle_warnings(true));
    assert_eq!(
        selection(&ws),
        second,
        "an unrelated repaint cannot invalidate the selection"
    );
    assert!(
        inspector(&ws).contains("Package beta"),
        "and the inspector keeps describing it: {}",
        inspector(&ws)
    );

    // 3. Filtering the pane so only the later occurrence is visible: the row on screen is
    //    underlying position 1, so its metadata has to be its own, not the first entry's.
    ws.with_window(|window| window.invoke_active_searched("beta".into()));
    let visible = active_keys(&ws);
    assert_eq!(visible.len(), 1, "the filter leaves only the beta row");
    select(&ws, &visible[0]);
    let text = inspector(&ws);
    assert!(
        text.contains("Package beta") && text.contains("the beta description"),
        "a filtered row must show the entry it really is, got {text}"
    );

    // 4. A command on that filtered row acts on that occurrence: moving it up swaps it with
    //    alpha, which is the only row still hidden by the filter.
    ws.with_window(move |window| window.invoke_move_row(visible[0].clone().into(), -1));
    assert_eq!(
        ws.active_tokens(),
        vec!["beta.mod", "alpha.mod"],
        "the move must target the visible occurrence"
    );
    let text = inspector(&ws);
    assert!(
        text.contains("Package beta"),
        "and the selection has to follow the row it moved: {text}"
    );
    // 5. A row the filter hides cannot be edited through its old key: the command is refused
    //    rather than landing on whatever is visible now.
    let hidden = active_keys(&ws);
    let second_last = hidden.last().cloned().expect("rows on screen");
    ws.with_window(|window| window.invoke_active_searched("alpha".into()));
    let before = ws.active_tokens();
    ws.with_window(move |window| window.invoke_move_row(second_last.clone().into(), -1));
    assert_eq!(
        ws.active_tokens(),
        before,
        "a command aimed at a row the filter hid must move nothing"
    );

    // 6. A sort lands an order the window did not perform. The tracked position would now
    //    describe a different row, so the selection has to go rather than follow the number.
    //    Put the list in an order the sorter will change first.
    let visible = active_keys(&ws);
    select(&ws, &visible[0]);
    let alpha = visible[0].clone();
    ws.with_window(move |window| window.invoke_move_row(alpha.into(), -1));
    ws.with_window(|window| window.invoke_active_searched("".into()));
    assert_eq!(
        ws.active_tokens(),
        vec!["alpha.mod", "beta.mod"],
        "the tracked move above put the list in an order the sorter has to fix"
    );
    let sorted_from = ws.active_tokens();
    select(&ws, &active_keys(&ws)[0]);
    assert!(
        !selection(&ws).is_empty(),
        "and something is selected before the sort lands"
    );
    ws.with_window(|window| window.invoke_sort_list());
    await_until(
        "sort landed",
        &ws,
        Rc::new(move |ws: &Workspace| ws.active_tokens() != sorted_from && !busy_sort(ws)),
    )
    .unwrap_or_else(|why| panic!("{why}"));
    assert_eq!(
        ws.active_tokens(),
        vec!["beta.mod", "alpha.mod"],
        "the sort itself reordered the list"
    );
    assert!(
        selection(&ws).is_empty(),
        "an order the window did not cause must drop the selection, not retarget it"
    );
    assert_eq!(
        inspector(&ws).split('|').next().expect("a title"),
        "Nothing selected",
        "and the inspector says so rather than keeping the old row's text"
    );
    // 7. Two active occurrences of the same package id are indistinguishable by anything but
    //    position, which is exactly why the selection tracks the occurrence and not the mod.
    let doubled = Doubled::new();
    let ws = Workspace::open(&doubled.startup()).expect("the second window must open");
    ws.show().expect("the window must be shown");
    ws.begin();
    await_until(
        "second bootstrap",
        &ws,
        Rc::new(|ws: &Workspace| ws.installed()),
    )
    .unwrap_or_else(|why| panic!("{why}"));
    assert_eq!(
        ws.active_tokens(),
        vec!["dup.mod", "other.mod", "dup.mod"],
        "the same package is active twice"
    );
    let last = active_keys(&ws)[2].clone();
    select(&ws, &last);
    for (step, expect_position) in [(0usize, 1usize), (1, 0)] {
        let key = selection(&ws);
        assert_eq!(
            key,
            active_keys(&ws)[2 - step],
            "step {step}: the selection is the occurrence picked, not another row"
        );
        ws.with_window(move |window| window.invoke_move_row(key.into(), -1));
        let moved = active_keys(&ws);
        assert_eq!(
            selection(&ws),
            moved[expect_position],
            "step {step}: the selection has to follow the row that actually moved"
        );
    }
    assert_eq!(
        ws.active_tokens(),
        vec!["dup.mod", "dup.mod", "other.mod"],
        "and both duplicate occurrences kept their own places in the list"
    );
    std::mem::drop(ws);
}

/// An install whose active list holds the same package id twice, so only position can tell
/// the two occurrences apart.
struct Doubled {
    temp: TempDir,
    settings: Settings,
}

impl Doubled {
    fn new() -> Self {
        let temp = TempDir::new().expect("a temporary install");
        let local = temp.path().join("Local Mods");
        let config = temp.path().join("config/ModsConfig.xml");
        fs::create_dir_all(local.join("dup").join("About")).expect("layout");
        fs::create_dir_all(local.join("other").join("About")).expect("layout");
        fs::create_dir_all(temp.path().join("game/Mods/Data")).expect("game content");
        fs::create_dir_all(config.parent().expect("parent")).expect("config folder");
        fs::write(
            &config,
            "<ModsConfigData><activeMods><li>dup.mod</li><li>other.mod</li><li>dup.mod</li>\
             </activeMods></ModsConfigData>",
        )
        .expect("the config must be writable");
        for (name, package) in [("dup", "dup.mod"), ("other", "other.mod")] {
            fs::write(
                local.join(name).join("About/About.xml"),
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
            config_path: Some(config),
            local_mods_root: Some(local),
            workshop_root: None,
            target_version: Some(
                rimsort_rust::domain::types::GameVersion::parse("1.5.4171").expect("parses"),
            ),
            executable_override: None,
            confirm_warnings: false,
        };
        Self { temp, settings }
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
}

fn busy_sort(ws: &Workspace) -> bool {
    let ws = ws.clone();
    ws.with_window(|window| window.get_busy_sort())
}
