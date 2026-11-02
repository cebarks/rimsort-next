//! Behavioral tests for the save/launch/close/refresh workflow (Task 5) and the
//! native launch adapter.
//!
//! Every case uses a real `ModsConfig.xml` inside its own temporary directory,
//! because a save is only worth testing when the bytes actually land. The game
//! root is a temporary directory holding a synthetic executable or bundle, so no
//! real installation is read and no real game is ever started. The only process
//! this suite spawns is a harmless fixture: this test binary asked to list its
//! tests, and a Unix shell script that writes one marker file.
//!
//! Effect sequences are compared as ordered label vectors, with the case's
//! temporary directory spelled `@tmp`, so a wrong order, a missing step or a
//! duplicate prompt shows up as the difference it is.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Child;
use std::time::{Duration, Instant};

use rimsort_rust::application::Workflow;
use rimsort_rust::application::session::Session;
use rimsort_rust::application::workflow::{
    LaunchOutcome, OperationFailure, OperationId, OperationKind, PromptAnswer, PromptId,
    RefreshOutcome, RefreshReplacement, RefreshTarget, SaveOutcome, WorkflowEffect, WorkflowEvent,
};
use rimsort_rust::domain::catalog::Catalog;
use rimsort_rust::domain::types::{
    GameVersion, ModId, ModMetadata, PackageId, ParsedMetadata, Source,
};
use rimsort_rust::domain::validation::IssueSeverity;
use rimsort_rust::integrations::config::{commit_save, parse_config};
use rimsort_rust::integrations::launch::{LaunchCommand, Platform, build_launch, spawn_launch};
use rimsort_rust::integrations::settings::Settings;
use tempfile::TempDir;

const CORE: &str = "/game/Data";
const ROYALTY: &str = "/game/Data/Royalty";
const HARMONY: &str = "/mods/harmony";
const EXTRA: &str = "/mods/extra";
const EXTRA_TOKEN: &str = "extra.mod";
const CORE_TOKEN: &str = "Ludeon.RimWorld";
const ROYALTY_TOKEN: &str = "Ludeon.RimWorld.Royalty";
const HARMONY_TOKEN: &str = "brrainz.harmony";
/// What the session writes for a copy the loaded list never named: the package
/// id as the catalog normalizes it, which is the session's own accepted spelling
/// for an activation and not something this task re-decides.
const ROYALTY_WRITTEN: &str = "ludeon.rimworld.royalty";
const MISSING_TOKEN: &str = "Not.Installed";
const TARGET: &str = "1.5.4171";

const WINDOWS_64: &str = "RimWorldWin64.exe";
const WINDOWS_32: &str = "RimWorldWin.exe";
const LINUX_64: &str = "RimWorldLinux64";
const LINUX_32: &str = "RimWorldLinux";

fn version(text: &str) -> GameVersion {
    GameVersion::parse(text).unwrap_or_else(|error| panic!("{text:?} must parse: {error}"))
}

fn package(raw: &str) -> PackageId {
    PackageId::normalize(raw).unwrap_or_else(|| panic!("{raw:?} must normalize"))
}

fn id(path: &str) -> ModId {
    ModId::new(path)
}

/// A usable copy that supports the release every case in this suite targets.
fn record(path: &str, source: Source, package_id: &str) -> ModMetadata {
    record_supported(path, source, package_id, &["1.5"])
}

/// A usable copy supporting a named release, for the candidate that is not this
/// suite's usual installation.
fn record_supported(
    path: &str,
    source: Source,
    package_id: &str,
    supported: &[&str],
) -> ModMetadata {
    let parsed = ParsedMetadata {
        package_id: Some(package(package_id)),
        name: Some("Test copy".to_string()),
        supported_versions: supported.iter().map(|text| version(text)).collect(),
        ..ParsedMetadata::default()
    };
    ModMetadata::new(id(path), source, parsed)
}

/// One installed copy per package id, so a clean list really is finding-free.
fn catalog() -> Catalog {
    Catalog::new(vec![
        record(CORE, Source::Official, "ludeon.rimworld"),
        record(ROYALTY, Source::Official, "ludeon.rimworld.royalty"),
        record(HARMONY, Source::Local, "brrainz.harmony"),
    ])
    .expect("the catalog must build")
}

/// A `ModsConfig.xml` holding `entries` verbatim, in this order.
fn document(entries: &[&str]) -> String {
    let mut text = String::from(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<ModsConfigData>\n  \
         <version>1.5</version>\n  <activeMods>\n",
    );
    for entry in entries {
        text.push_str(&format!("    <li>{entry}</li>\n"));
    }
    text.push_str("  </activeMods>\n</ModsConfigData>\n");
    text
}

/// Creates `path` as an ordinary file holding `contents`.
fn file(path: &Path, contents: &str) {
    fs::write(path, contents).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
}

/// Sets the execute bits on a file that already holds its own contents.
#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
}

/// Creates `path` as a file this host can execute.
///
/// The mode bits only exist to be set on a Unix host; a Windows host has no Unix
/// mode to give a file, which is exactly the limit the Linux selection test
/// states.
fn executable(path: &Path) {
    file(path, "not a real game\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    }
}

/// Creates `path` as an empty file, the mode the host leaves it in.
#[cfg(unix)]
fn non_executable(path: &Path) {
    file(path, "not a real game\n");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o644))
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
}

fn make_directory(path: &Path) {
    fs::create_dir_all(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
}

/// A settings record for a game root that exists, with no executable override.
fn settings_for(root: &Path) -> Settings {
    Settings {
        game_root: Some(root.to_path_buf()),
        content_root: Some(root.join("RimWorldWin_Data")),
        config_path: Some(root.join("Config").join("ModsConfig.xml")),
        target_version: Some(version(TARGET)),
        ..Settings::default()
    }
}

/// Asserts that one event produced no effects at all: a cancel, a dropped
/// result or a finished operation must leave the caller with nothing to do.
#[track_caller]
fn assert_no_effects(effects: &[WorkflowEffect], why: &str) {
    assert!(
        effects.is_empty(),
        "{why}, but the workflow emitted {effects:?}"
    );
}

/// The `OperationId` the first effect carries, which is the operation a caller
/// has to echo back in its completion or answer.
fn operation(effects: &[WorkflowEffect]) -> OperationId {
    let found = match effects.first() {
        Some(WorkflowEffect::PromptUnsavedLaunch { operation, .. })
        | Some(WorkflowEffect::PromptUnsavedClose { operation, .. })
        | Some(WorkflowEffect::PromptUnsavedRefresh { operation, .. })
        | Some(WorkflowEffect::PromptWarnings { operation, .. })
        | Some(WorkflowEffect::Save { operation, .. })
        | Some(WorkflowEffect::Launch { operation, .. })
        | Some(WorkflowEffect::Refresh { operation, .. })
        | Some(WorkflowEffect::InstallRefresh { operation, .. }) => *operation,
        other => panic!("no effect carries an operation: {other:?}"),
    };
    assert!(effects.len() == 1, "one step at a time, got {effects:?}");
    found
}

/// The `PromptId` the first effect carries, which is the prompt an answer must name.
fn prompt(effects: &[WorkflowEffect]) -> PromptId {
    let found = match effects.first() {
        Some(WorkflowEffect::PromptUnsavedLaunch { prompt, .. })
        | Some(WorkflowEffect::PromptUnsavedClose { prompt, .. })
        | Some(WorkflowEffect::PromptUnsavedRefresh { prompt, .. })
        | Some(WorkflowEffect::PromptWarnings { prompt, .. }) => *prompt,
        other => panic!("no prompt effect to answer: {other:?}"),
    };
    assert!(effects.len() == 1, "one prompt at a time, got {effects:?}");
    found
}

/// A candidate installation to re-read: these settings, this config file.
fn candidate(path: &Path) -> RefreshTarget {
    let mut settings = settings_for(path.parent().unwrap_or(path));
    settings.config_path = Some(path.to_path_buf());
    RefreshTarget { settings }
}

/// The failures in `effects`, as the (kind, message) pair the user would see.
fn failures(effects: &[WorkflowEffect]) -> Vec<(OperationKind, String)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            WorkflowEffect::ReportFailure { operation, message } => {
                Some((*operation, message.clone()))
            }
            _ => None,
        })
        .collect()
}

/// The one `Launch` effect's command.
fn only_command(effects: &[WorkflowEffect]) -> &LaunchCommand {
    let Some(WorkflowEffect::Launch { command, .. }) = effects.first() else {
        panic!("expected a launch effect, got {effects:?}");
    };
    command
}

/// Asks the adapter what `platform` would run for `settings`, resolving any
/// relative path against `basis`.
fn build_at(
    settings: &Settings,
    platform: Platform,
    basis: &Path,
) -> Result<LaunchCommand, rimsort_rust::domain::types::Error> {
    build_launch(settings, platform, basis)
}

/// A `ModsConfig.xml` holding `entries` under a named release, so a case can
/// build a valid document that differs from the one an operation prepared.
fn document_with_version(entries: &[&str], release: &str) -> String {
    let mut text = format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<ModsConfigData>\n  \
         <version>{release}</version>\n  <activeMods>\n"
    );
    for entry in entries {
        text.push_str(&format!("    <li>{entry}</li>\n"));
    }
    text.push_str("  </activeMods>\n</ModsConfigData>\n");
    text
}

/// A harmless host fixture that starts, proves it is still running, and waits to
/// be released before it finishes. `/bin/sh` is the fixture's own shell, used only
/// by these Unix cases: nothing in the adapter builds a shell, and every game
/// command this suite asserts holds a program and separate arguments.
///
/// The script bounds its own wait, so a panicked parent cannot leave a fixture
/// running forever.
#[cfg(unix)]
fn waiting_fixture(directory: &TempDir) -> LaunchCommand {
    let script = format!(
        "touch '{started}'; i=0; while [ $i -lt 300 ] && [ ! -f '{release}' ]; do sleep 0.1; \
         i=$((i+1)); done; touch '{done}'",
        started = started_path(directory).display(),
        release = release_path(directory).display(),
        done = finished_path(directory).display(),
    );
    LaunchCommand {
        program: PathBuf::from("/bin/sh"),
        args: vec![OsString::from("-c"), OsString::from(script)],
        working_directory: Some(directory.path().to_path_buf()),
    }
}

#[cfg(unix)]
fn started_path(directory: &TempDir) -> PathBuf {
    directory.path().join("started")
}

#[cfg(unix)]
fn release_path(directory: &TempDir) -> PathBuf {
    directory.path().join("release")
}

#[cfg(unix)]
fn finished_path(directory: &TempDir) -> PathBuf {
    directory.path().join("finished")
}

/// Lets the waiting fixture finish, so no case leaves it behind.
#[cfg(unix)]
fn release(directory: &TempDir) {
    file(&release_path(directory), "released\n");
}

/// A loaded installation in its own temporary directory, with the workflow and
/// session a real caller would hold.
struct Loaded {
    directory: TempDir,
    games: PathBuf,
    path: PathBuf,
    settings: Settings,
    workflow: Workflow,
    session: Session,
}

impl Loaded {
    /// Loads `entries` with warning confirmation on, which is the default.
    fn with_confirmation(entries: &[&str]) -> Self {
        Self::built(entries, true)
    }

    /// Loads `entries` with warning confirmation switched off.
    fn without_confirmation(entries: &[&str]) -> Self {
        Self::built(entries, false)
    }

    fn built(entries: &[&str], confirm_warnings: bool) -> Self {
        let directory = TempDir::new().expect("a temporary directory");
        let games = directory.path().join("games");
        make_directory(&games);
        executable(&games.join(WINDOWS_64));
        let config = directory.path().join("config");
        make_directory(&config);
        let path = config.join("ModsConfig.xml");
        let text = document(entries);
        file(&path, &text);

        let mut settings = settings_for(&games);
        settings.config_path = Some(path.clone());
        settings.content_root = Some(games.join("RimWorldWin_Data"));
        settings.confirm_warnings = confirm_warnings;

        let snapshot = parse_config(text.as_bytes()).expect("the document must load");
        let session = Session::new(catalog(), snapshot, path.clone(), version(TARGET));
        let workflow = Workflow::new(&settings, Platform::Windows, directory.path());
        Self {
            directory,
            games,
            path,
            settings,
            workflow,
            session,
        }
    }

    /// Runs one event through the workflow.
    fn reduce(&mut self, event: WorkflowEvent) -> Vec<WorkflowEffect> {
        self.workflow.reduce(&mut self.session, event)
    }

    /// The user asked to save the working list on its own, under the warning
    /// preference the caller currently has.
    fn save_request(&mut self) -> Vec<WorkflowEffect> {
        let confirm_warnings = self.settings.confirm_warnings;
        self.reduce(WorkflowEvent::SaveRequested { confirm_warnings })
    }

    /// The user asked to start the game.
    fn launch_request(&mut self) -> Vec<WorkflowEffect> {
        let confirm_warnings = self.settings.confirm_warnings;
        self.reduce(WorkflowEvent::LaunchRequested { confirm_warnings })
    }

    /// The user asked to close the application.
    fn close_request(&mut self) -> Vec<WorkflowEffect> {
        let confirm_warnings = self.settings.confirm_warnings;
        self.reduce(WorkflowEvent::CloseRequested { confirm_warnings })
    }

    /// The user asked to re-read the loaded installation.
    fn refresh_request(&mut self) -> Vec<WorkflowEffect> {
        let path = self.path.clone();
        let confirm_warnings = self.settings.confirm_warnings;
        self.reduce(WorkflowEvent::RefreshRequested {
            target: candidate(&path),
            confirm_warnings,
        })
    }

    /// The user asked to re-read `chosen`, the installation the caller's settings
    /// now describe.
    fn refresh_request_for(&mut self, chosen: &Path) -> Vec<WorkflowEffect> {
        let target = candidate(chosen);
        let confirm_warnings = self.settings.confirm_warnings;
        self.reduce(WorkflowEvent::RefreshRequested {
            target,
            confirm_warnings,
        })
    }

    /// The user asked to re-read exactly this candidate.
    fn refresh_target(&mut self, target: RefreshTarget) -> Vec<WorkflowEffect> {
        let confirm_warnings = self.settings.confirm_warnings;
        self.reduce(WorkflowEvent::RefreshRequested {
            target,
            confirm_warnings,
        })
    }

    /// Answers the prompt `asked` raised, with the identity that effect carries.
    /// Handing over an older `asked` vector deliberately answers an older prompt.
    fn answer(&mut self, asked: &[WorkflowEffect], answer: PromptAnswer) -> Vec<WorkflowEffect> {
        let prompt = prompt(asked);
        self.reduce(WorkflowEvent::PromptAnswered { prompt, answer })
    }

    /// Carries out the `Save` effect for real, on this case's own file, and reports
    /// what the filesystem did.
    fn save_outcome(&self, effects: &[WorkflowEffect]) -> SaveOutcome {
        let Some(WorkflowEffect::Save {
            path,
            baseline,
            prepared,
            ..
        }) = effects.first()
        else {
            panic!("expected a save effect, got {effects:?}");
        };
        match commit_save(path, baseline, prepared) {
            Ok(committed) => SaveOutcome::Committed(committed),
            Err(error) => SaveOutcome::Failed(OperationFailure::from_error(&error)),
        }
    }

    /// The event that reports this case's own save effect.
    fn save_event(&self, effects: &[WorkflowEffect]) -> WorkflowEvent {
        let operation = operation(effects);
        WorkflowEvent::SaveFinished {
            operation,
            outcome: self.save_outcome(effects),
        }
    }

    /// Saves the effect for real and reports the result, returning what followed.
    fn save_landed(&mut self, effects: &[WorkflowEffect]) -> Vec<WorkflowEffect> {
        let event = self.save_event(effects);
        self.reduce(event)
    }

    /// Reports a launch the operating system accepted.
    fn spawn_accepted(&mut self, effects: &[WorkflowEffect]) -> Vec<WorkflowEffect> {
        let operation = operation(effects);
        self.reduce(WorkflowEvent::LaunchFinished {
            operation,
            outcome: LaunchOutcome::Spawned { process_id: 4242 },
        })
    }

    /// Reports a launch the operating system refused.
    fn spawn_refused(&mut self, effects: &[WorkflowEffect]) -> Vec<WorkflowEffect> {
        let operation = operation(effects);
        self.reduce(WorkflowEvent::LaunchFinished {
            operation,
            outcome: LaunchOutcome::Failed(OperationFailure::new(
                "the operating system refused the process".to_string(),
            )),
        })
    }

    /// Reports a refresh that read this case's file, catalog and release.
    fn refresh_reloaded(&mut self, effects: &[WorkflowEffect]) -> Vec<WorkflowEffect> {
        let operation = operation(effects);
        let path = self.path.clone();
        let replacement = RefreshReplacement {
            origin: candidate(&path),
            config: parse_config(self.bytes().as_slice()).expect("the file must reload"),
            catalog: catalog(),
            target_version: version(TARGET),
            diagnostics: Vec::new(),
        };
        self.reduce(WorkflowEvent::RefreshFinished {
            operation,
            outcome: RefreshOutcome::Reloaded(Box::new(replacement)),
        })
    }

    /// Reports a refresh that could not read anything.
    fn refresh_failed(&mut self, effects: &[WorkflowEffect]) -> Vec<WorkflowEffect> {
        let operation = operation(effects);
        self.reduce(WorkflowEvent::RefreshFinished {
            operation,
            outcome: RefreshOutcome::Failed(OperationFailure::new(
                "the drive is not there".to_string(),
            )),
        })
    }

    /// Adds one installed copy to the working list, as a user editing would.
    fn activate(&mut self, path: &str) {
        self.session
            .activate(&id(path))
            .unwrap_or_else(|error| panic!("activating {path} must work: {error}"));
    }

    /// The bytes on disk right now.
    fn bytes(&self) -> Vec<u8> {
        fs::read(&self.path).unwrap_or_else(|error| panic!("{}: {error}", self.path.display()))
    }

    /// Writes different bytes behind the session's back, as another program would.
    fn overwritten_by_another_writer(&self) {
        file(&self.path, &document(&[CORE_TOKEN, HARMONY_TOKEN]));
    }

    /// The directory this case owns, for stable labels.
    fn root(&self) -> &Path {
        self.directory.path()
    }

    /// A path spelled without the volatile temporary prefix.
    fn shown(&self, path: &Path) -> String {
        match path.strip_prefix(self.root()) {
            Ok(relative) => format!("@tmp/{}", relative.display()),
            Err(_) => path.display().to_string(),
        }
    }

    /// The ordered effects, as labels a case can compare with a literal.
    fn labels(&self, effects: &[WorkflowEffect]) -> Vec<String> {
        effects
            .iter()
            .map(|effect| match effect {
                WorkflowEffect::PromptUnsavedLaunch { .. } => "prompt-unsaved-launch".to_string(),
                WorkflowEffect::PromptUnsavedClose { .. } => "prompt-unsaved-close".to_string(),
                WorkflowEffect::PromptUnsavedRefresh { .. } => "prompt-unsaved-refresh".to_string(),
                WorkflowEffect::PromptWarnings { issues, .. } => {
                    format!("prompt-warnings({})", issues.len())
                }
                WorkflowEffect::Save { path, .. } => format!("save:{}", self.shown(path)),
                WorkflowEffect::Launch { command, .. } => {
                    format!("launch:{}", self.shown(&command.program))
                }
                WorkflowEffect::Refresh { target, .. } => format!(
                    "refresh:{}",
                    self.shown(target.config_path().unwrap_or(Path::new("<none>")))
                ),
                WorkflowEffect::InstallRefresh { target, .. } => format!(
                    "install-refresh:{}",
                    self.shown(target.config_path().unwrap_or(Path::new("<none>")))
                ),
                WorkflowEffect::PromptUnsavedReplace { .. } => "prompt-unsaved-replace".to_string(),
                WorkflowEffect::InstallReplacement { .. } => "install-replacement".to_string(),
                WorkflowEffect::Close => "close".to_string(),
                WorkflowEffect::ReportFailure { operation, message } => {
                    format!("report-{}:{message}", operation.word())
                }
            })
            .collect()
    }

    /// The working list, as `path@token` per row.
    fn working(&self) -> Vec<String> {
        self.session
            .working_entries()
            .iter()
            .map(|entry| format!("{}@{}", entry.original_token(), entry.mod_id().is_some()))
            .collect()
    }
}

// The reducer: what has to happen, in what order.

#[test]
fn a_clean_list_launches_one_command_and_writes_nothing() {
    let mut case = Loaded::with_confirmation(&[CORE_TOKEN, ROYALTY_TOKEN]);
    assert_eq!(
        case.session.validate(),
        Vec::new(),
        "the premise: this list carries no findings at all"
    );
    assert!(!case.session.is_dirty(), "the premise: nothing is pending");

    let effects = case.launch_request();
    assert_eq!(
        case.labels(&effects),
        vec!["launch:@tmp/games/RimWorldWin64.exe"],
        "a clean list launches: no prompt, no save, no refresh"
    );

    let command = only_command(&effects);
    assert_eq!(
        command.program,
        case.games.join(WINDOWS_64),
        "the launch is built from the game root this installation was loaded with"
    );
    assert_eq!(
        command.working_directory.as_deref(),
        Some(case.games.as_path()),
        "and it starts in that game root"
    );
    assert_eq!(
        case.bytes(),
        document(&[CORE_TOKEN, ROYALTY_TOKEN]).as_bytes()
    );
    assert_eq!(case.session.revision(), 0);
}

#[test]
fn an_unsaved_launch_prompts_before_anything_else_runs() {
    let mut case = Loaded::without_confirmation(&[CORE_TOKEN]);
    case.activate(HARMONY);

    let effects = case.launch_request();
    assert_eq!(
        case.labels(&effects),
        vec!["prompt-unsaved-launch"],
        "unsaved edits mean the only first step is the question"
    );
    assert_eq!(
        case.bytes(),
        document(&[CORE_TOKEN]).as_bytes(),
        "nothing is written before the answer"
    );
    assert!(case.session.is_dirty());
}

#[test]
fn launching_with_unsaved_edits_saves_first_and_then_launches() {
    let mut case = Loaded::without_confirmation(&[CORE_TOKEN]);
    case.activate(HARMONY);

    let prompted = case.launch_request();
    let saved = case.answer(&prompted, PromptAnswer::Save);
    assert_eq!(
        case.labels(&saved),
        vec![format!("save:{}", case.shown(&case.path))],
        "the save runs before the launch, not alongside it"
    );

    let launched = case.save_landed(&saved);
    assert_eq!(
        case.labels(&launched),
        vec!["launch:@tmp/games/RimWorldWin64.exe"],
        "the launch follows the committed save"
    );
    assert_eq!(
        case.bytes(),
        document(&[CORE_TOKEN, HARMONY_TOKEN]).as_bytes(),
        "the edits are on disk before the game is started"
    );
    assert!(
        !case.session.is_dirty(),
        "a list that just saved is not still pending"
    );
}

#[test]
fn a_cancelled_unsaved_launch_changes_nothing_at_all() {
    let mut case = Loaded::without_confirmation(&[CORE_TOKEN]);
    case.activate(HARMONY);
    let revision = case.session.revision();
    let edits = case.working();

    let prompted = case.launch_request();
    let answered = case.answer(&prompted, PromptAnswer::Cancel);

    assert_no_effects(&answered, "cancel runs no effect at all");
    assert_eq!(case.working(), edits, "the exact edits survive");
    assert_eq!(case.session.revision(), revision);
    assert!(case.session.is_dirty());
    assert_eq!(case.bytes(), document(&[CORE_TOKEN]).as_bytes());
}

#[test]
fn discarding_is_not_an_answer_to_an_unsaved_launch() {
    // There is no launch-without-save transition: the question offers saving or
    // cancelling, and an answer that would launch over unsaved edits is refused
    // without touching them.
    let mut case = Loaded::without_confirmation(&[CORE_TOKEN]);
    case.activate(HARMONY);
    let edits = case.working();

    let prompted = case.launch_request();
    let refused = case.answer(&prompted, PromptAnswer::Discard);

    let reported = failures(&refused);
    assert_eq!(
        reported.len(),
        1,
        "the refusal is reported, got {refused:?}"
    );
    assert_eq!(reported[0].0, OperationKind::Launch);
    assert!(
        reported[0].1.contains("not an answer"),
        "the message says the answer is unavailable: {}",
        reported[0].1
    );
    assert!(
        reported[0].1.contains("save or cancel"),
        "and names what the question does offer: {}",
        reported[0].1
    );
    assert_eq!(case.working(), edits, "the edits were not discarded");
    assert!(case.session.is_dirty());
    assert_eq!(case.bytes(), document(&[CORE_TOKEN]).as_bytes());

    // The question is still outstanding, so the real answer still works.
    let cancelled = case.answer(&prompted, PromptAnswer::Cancel);
    assert_no_effects(
        &cancelled,
        "cancelling the unanswered question runs nothing",
    );
}

#[test]
fn a_failed_save_launches_nothing_and_keeps_the_exact_edits() {
    let mut case = Loaded::without_confirmation(&[CORE_TOKEN]);
    case.activate(HARMONY);
    let edits = case.working();

    let prompted = case.launch_request();
    let saved = case.answer(&prompted, PromptAnswer::Save);
    case.overwritten_by_another_writer();
    let reported = case.save_landed(&saved);

    assert_eq!(
        failures(&reported),
        vec![(
            OperationKind::Save,
            format!(
                "{} changed since it was loaded; reload it or cancel this save",
                case.path.display()
            )
        )],
        "the conflict is reported for the save, and nothing else runs"
    );
    assert_eq!(case.working(), edits, "the edits survive a refused write");
    assert!(case.session.is_dirty());
    assert_eq!(
        case.session.loaded_config().raw,
        document(&[CORE_TOKEN]).as_bytes(),
        "the session still describes the load it came from"
    );
}

#[test]
fn a_successful_save_then_a_failed_spawn_leaves_the_list_clean() {
    let mut case = Loaded::without_confirmation(&[CORE_TOKEN]);
    case.activate(HARMONY);

    let prompted = case.launch_request();
    let saved = case.answer(&prompted, PromptAnswer::Save);
    let launched = case.save_landed(&saved);
    let reported = case.spawn_refused(&launched);

    assert_eq!(
        case.labels(&reported),
        vec!["report-launch:the operating system refused the process"],
        "the launch failure is reported on its own"
    );
    assert!(
        !case.session.is_dirty(),
        "the save that did land is not undone by a spawn that did not"
    );
    assert_eq!(
        case.bytes(),
        document(&[CORE_TOKEN, HARMONY_TOKEN]).as_bytes(),
        "the written list stays written"
    );
}

#[test]
fn a_second_launch_while_the_spawn_is_pending_is_refused() {
    let mut case = Loaded::with_confirmation(&[CORE_TOKEN, ROYALTY_TOKEN]);

    let first = case.launch_request();
    let second = case.launch_request();

    assert_eq!(
        case.labels(&first),
        vec!["launch:@tmp/games/RimWorldWin64.exe"]
    );
    let reported = failures(&second);
    assert_eq!(reported.len(), 1, "one spawn at a time, got {second:?}");
    assert_eq!(reported[0].0, OperationKind::Launch);
    assert!(
        reported[0].1.contains("still running"),
        "the refusal says why: {}",
        reported[0].1
    );
    assert!(
        case.labels(&second)
            .iter()
            .all(|label| !label.starts_with("launch:")),
        "no second command is emitted while the first is unaccepted"
    );

    let accepted = case.spawn_accepted(&first);
    assert_no_effects(&accepted, "an accepted spawn ends the operation");
    let third = case.launch_request();
    assert_eq!(
        case.labels(&third),
        vec!["launch:@tmp/games/RimWorldWin64.exe"],
        "and a later launch is allowed"
    );
}

#[test]
fn an_operation_in_flight_refuses_a_conflicting_request() {
    let mut case = Loaded::without_confirmation(&[CORE_TOKEN]);
    case.activate(HARMONY);

    let saving = case.save_request();
    let closing = case.close_request();
    let reported = failures(&closing);
    assert_eq!(
        reported.len(),
        1,
        "the close is refused while the save runs, got {closing:?}"
    );
    assert_eq!(reported[0].0, OperationKind::Close);
    assert!(reported[0].1.contains("save"));

    let landed = case.save_landed(&saving);
    assert_no_effects(&landed, "a standalone save ends when it lands");
    let closed = case.close_request();
    assert_eq!(case.labels(&closed), vec!["close"], "then the close runs");
}

#[test]
fn a_standalone_save_writes_the_working_list_and_nothing_else() {
    let mut case = Loaded::without_confirmation(&[CORE_TOKEN]);
    case.activate(ROYALTY);

    let saving = case.save_request();
    let landed = case.save_landed(&saving);

    assert_no_effects(&landed, "a save on its own launches and closes nothing");
    assert_eq!(
        case.bytes(),
        document(&[CORE_TOKEN, ROYALTY_WRITTEN]).as_bytes(),
        "the working list is what lands, token for token"
    );
    assert!(!case.session.is_dirty());
}

// Closing.

#[test]
fn closing_a_clean_list_closes_without_prompting() {
    let mut case = Loaded::with_confirmation(&[CORE_TOKEN, ROYALTY_TOKEN]);

    let effects = case.close_request();
    assert_eq!(
        case.labels(&effects),
        vec!["close"],
        "no pending edits and nothing warned about means nothing to ask"
    );
    assert_eq!(
        case.bytes(),
        document(&[CORE_TOKEN, ROYALTY_TOKEN]).as_bytes()
    );
}

#[test]
fn saving_then_closing_writes_before_it_closes() {
    let mut case = Loaded::without_confirmation(&[CORE_TOKEN]);
    case.activate(HARMONY);

    let prompted = case.close_request();
    assert_eq!(case.labels(&prompted), vec!["prompt-unsaved-close"]);
    let saved = case.answer(&prompted, PromptAnswer::Save);
    assert_eq!(
        case.labels(&saved),
        vec![format!("save:{}", case.shown(&case.path))]
    );
    let closed = case.save_landed(&saved);

    assert_eq!(case.labels(&closed), vec!["close"]);
    assert_eq!(
        case.bytes(),
        document(&[CORE_TOKEN, HARMONY_TOKEN]).as_bytes(),
        "the file holds the edits the close was asked to keep"
    );
    assert!(!case.session.is_dirty());
}

#[test]
fn a_discarded_close_closes_without_writing() {
    let mut case = Loaded::without_confirmation(&[CORE_TOKEN]);
    let loaded = case.bytes();
    case.activate(HARMONY);

    let prompted = case.close_request();
    let closed = case.answer(&prompted, PromptAnswer::Discard);

    assert_eq!(case.labels(&closed), vec!["close"]);
    assert_eq!(
        case.bytes(),
        loaded,
        "discarding never writes, so the file still holds the loaded list"
    );
    assert!(
        !case.session.is_dirty(),
        "the discarded edits are gone, not left pending"
    );
}

#[test]
fn a_cancelled_close_preserves_the_pending_edits() {
    let mut case = Loaded::without_confirmation(&[CORE_TOKEN]);
    case.activate(HARMONY);
    let edits = case.working();

    let prompted = case.close_request();
    let answered = case.answer(&prompted, PromptAnswer::Cancel);

    assert_no_effects(&answered, "cancel closes nothing and writes nothing");
    assert_eq!(case.working(), edits);
    assert!(case.session.is_dirty());
    assert_eq!(case.bytes(), document(&[CORE_TOKEN]).as_bytes());
}

#[test]
fn pending_close_tracks_owned_operation_not_stale_answers_or_refused_requests() {
    let mut case = Loaded::with_confirmation(&[CORE_TOKEN, MISSING_TOKEN]);
    case.activate(HARMONY);
    assert!(!case.workflow.has_pending_close());
    let first = case.close_request();
    assert!(case.workflow.has_pending_close());
    case.answer(&first, PromptAnswer::Cancel);
    assert!(!case.workflow.has_pending_close());
    let second = case.close_request();
    case.answer(&first, PromptAnswer::Cancel);
    assert!(case.workflow.has_pending_close());
    let warnings = case.answer(&second, PromptAnswer::Save);
    assert_eq!(case.labels(&warnings), vec!["prompt-warnings(1)"]);
    assert!(case.workflow.has_pending_close());
    case.answer(&warnings, PromptAnswer::Dismiss);
    assert!(!case.workflow.has_pending_close());

    let save_warnings = case.save_request();
    case.close_request();
    assert!(!case.workflow.has_pending_close());
    case.answer(&save_warnings, PromptAnswer::Dismiss);
    let close = case.close_request();
    let warnings = case.answer(&close, PromptAnswer::Save);
    let saved = case.answer(&warnings, PromptAnswer::Confirm);
    assert!(case.workflow.has_pending_close());
    case.answer(&first, PromptAnswer::Cancel);
    case.close_request();
    assert!(case.workflow.has_pending_close());
    fs::remove_file(&case.path).unwrap();
    let reported = case.save_landed(&saved);
    assert!(case.labels(&reported)[0].starts_with("report-save:"));
    assert!(!case.workflow.has_pending_close());
    assert!(case.session.is_dirty());
    let again = case.close_request();
    assert!(case.workflow.has_pending_close());
    let closed = case.answer(&again, PromptAnswer::Discard);
    assert_eq!(case.labels(&closed), vec!["close"]);
    assert!(!case.workflow.has_pending_close());
}

#[test]
fn a_failed_close_save_keeps_the_application_open() {
    let mut case = Loaded::without_confirmation(&[CORE_TOKEN]);
    case.activate(HARMONY);
    let edits = case.working();

    let prompted = case.close_request();
    let saved = case.answer(&prompted, PromptAnswer::Save);
    fs::remove_file(&case.path).expect("the configuration file must be removable");
    let reported = case.save_landed(&saved);

    let reported_labels = case.labels(&reported);
    assert_eq!(
        reported_labels.len(),
        1,
        "one report, no close, got {reported_labels:?}"
    );
    assert!(
        reported_labels[0].starts_with("report-save:"),
        "the failure is about the save: {reported_labels:?}"
    );
    assert!(
        !reported_labels.iter().any(|label| label == "close"),
        "a failed save never closes the window"
    );
    assert_eq!(case.working(), edits, "and never drops the edits");
    assert!(case.session.is_dirty());
}

// Refreshing, which stays bound to the installation it was loaded from.

#[test]
fn a_clean_refresh_reads_the_loaded_installation() {
    let mut case = Loaded::with_confirmation(&[CORE_TOKEN, ROYALTY_TOKEN]);

    let effects = case.refresh_request();
    assert_eq!(
        case.labels(&effects),
        vec![format!("refresh:{}", case.shown(&case.path))],
        "a clean list goes straight to the re-read, at the loaded path"
    );
}

#[test]
fn a_dirty_refresh_prompts_and_reads_only_after_the_save_lands() {
    let mut case = Loaded::without_confirmation(&[CORE_TOKEN]);
    case.activate(HARMONY);

    let prompted = case.refresh_request();
    assert_eq!(case.labels(&prompted), vec!["prompt-unsaved-refresh"]);
    let saved = case.answer(&prompted, PromptAnswer::Save);
    assert_eq!(
        case.labels(&saved),
        vec![format!("save:{}", case.shown(&case.path))]
    );
    let refreshed = case.save_landed(&saved);
    assert_eq!(
        case.labels(&refreshed),
        vec![format!("refresh:{}", case.shown(&case.path))],
        "the re-read comes after the write, never before it"
    );

    let installed = case.refresh_reloaded(&refreshed);
    assert_eq!(
        case.labels(&installed),
        vec![format!("install-refresh:{}", case.shown(&case.path))],
        "a current re-read is the one that may be installed"
    );
    assert!(!case.session.is_dirty());
}

#[test]
fn a_refresh_after_discarding_reads_without_writing() {
    let mut case = Loaded::without_confirmation(&[CORE_TOKEN]);
    let loaded = case.bytes();
    case.activate(HARMONY);

    let prompted = case.refresh_request();
    let refreshed = case.answer(&prompted, PromptAnswer::Discard);

    assert_eq!(
        case.labels(&refreshed),
        vec![format!("refresh:{}", case.shown(&case.path))]
    );
    assert_eq!(case.bytes(), loaded, "discarding writes nothing");
    assert!(!case.session.is_dirty());
}

#[test]
fn cancelling_a_refresh_keeps_the_edits_and_reads_nothing() {
    let mut case = Loaded::without_confirmation(&[CORE_TOKEN]);
    case.activate(HARMONY);
    let edits = case.working();

    let prompted = case.refresh_request();
    let answered = case.answer(&prompted, PromptAnswer::Cancel);

    assert_no_effects(&answered, "a cancelled refresh reads nothing");
    assert_eq!(case.working(), edits);
    assert!(case.session.is_dirty());
}

#[test]
fn a_failed_refresh_leaves_the_old_installation_in_force() {
    let mut case = Loaded::without_confirmation(&[CORE_TOKEN]);
    let loaded_bytes = case.bytes();

    let refreshed = case.refresh_request();
    let reported = case.refresh_failed(&refreshed);

    assert_eq!(
        case.labels(&reported),
        vec!["report-refresh:the drive is not there"],
        "the failure is reported and nothing is installed"
    );

    // The old session is still the one that saves, to the old path and against
    // the old bytes.
    case.activate(HARMONY);
    let saving = case.save_request();
    assert_eq!(
        case.labels(&saving),
        vec![format!("save:{}", case.shown(&case.path))]
    );
    let WorkflowEffect::Save { path, baseline, .. } = saving.first().expect("the save effect")
    else {
        panic!("expected a save effect");
    };
    assert_eq!(path, &case.path, "a failed refresh redirects nothing");
    assert_eq!(
        baseline.raw, loaded_bytes,
        "and the save is still against the bytes this session loaded"
    );
}

#[test]
fn a_stale_refresh_completion_installs_nothing() {
    let mut case = Loaded::without_confirmation(&[CORE_TOKEN]);

    let refreshed = case.refresh_request();
    let operation = operation(&refreshed);
    case.activate(HARMONY);
    let reported = case.reduce(WorkflowEvent::RefreshFinished {
        operation,
        outcome: RefreshOutcome::Reloaded(Box::new(RefreshReplacement {
            origin: candidate(&case.path),
            config: parse_config(document(&[CORE_TOKEN]).as_bytes())
                .expect("the document must load"),
            catalog: catalog(),
            target_version: version(TARGET),
            diagnostics: Vec::new(),
        })),
    });

    let labels = case.labels(&reported);
    assert_eq!(
        labels,
        vec![
            "report-refresh:the list changed while the refresh was running, so the re-read was not installed"
        ],
        "the stale result is reported and nothing is installed"
    );
    assert!(
        case.session.is_dirty(),
        "the edit made while the refresh ran is still pending"
    );
    assert_eq!(
        case.working(),
        vec![
            format!("{CORE_TOKEN}@true"),
            format!("{HARMONY_TOKEN}@true")
        ],
        "and the session is exactly what the user left it"
    );
    assert_eq!(
        case.session.loaded_config().raw,
        document(&[CORE_TOKEN]).as_bytes(),
        "the loaded installation is untouched by a dropped re-read"
    );
}

#[test]
fn pending_form_edits_never_redirect_the_old_session() {
    let mut case = Loaded::without_confirmation(&[CORE_TOKEN]);
    // The user is editing the settings form for a different installation while
    // this session still belongs to the one it was loaded from.
    case.settings.game_root = Some(case.root().join("other"));
    case.settings.config_path = Some(case.root().join("other/ModsConfig.xml"));
    case.settings.executable_override = Some(case.root().join("other/RimWorldWin.exe"));
    case.settings.confirm_warnings = true;
    case.activate(HARMONY);

    let saving = case.save_request();
    let WorkflowEffect::Save { path, baseline, .. } = saving.first().expect("the save effect")
    else {
        panic!("expected a save effect");
    };
    assert_eq!(path, &case.path, "the save stays on the loaded path");
    assert_eq!(
        baseline.raw,
        document(&[CORE_TOKEN]).as_bytes(),
        "and against the loaded bytes"
    );

    let landed = case.save_landed(&saving);
    assert_no_effects(&landed, "a standalone save ends without a launch");

    let launched = case.launch_request();
    let command = only_command(&launched);
    assert_eq!(
        command.program,
        case.games.join(WINDOWS_64),
        "the launch uses the game root captured at load, not the form's override"
    );
    assert_eq!(
        case.bytes(),
        document(&[CORE_TOKEN, HARMONY_TOKEN]).as_bytes()
    );
}

// Warning confirmation: once per operation, at the revision it was given.

#[test]
fn save_and_launch_confirms_warnings_once() {
    let mut case = Loaded::with_confirmation(&[CORE_TOKEN, MISSING_TOKEN]);
    case.activate(HARMONY);

    let prompted = case.launch_request();
    let warned = case.answer(&prompted, PromptAnswer::Save);
    assert_eq!(
        case.labels(&warned),
        vec!["prompt-warnings(1)"],
        "the unsaved question is answered by one warnings prompt, not two"
    );
    let unresolved: Vec<IssueSeverity> = match warned.first() {
        Some(WorkflowEffect::PromptWarnings { issues, .. }) => {
            issues.iter().map(|issue| issue.severity()).collect()
        }
        _ => panic!("expected the warnings prompt"),
    };
    assert_eq!(unresolved, vec![IssueSeverity::Warning]);

    let saved = case.answer(&warned, PromptAnswer::Confirm);
    assert_eq!(
        case.labels(&saved),
        vec![format!("save:{}", case.shown(&case.path))]
    );
    let launched = case.save_landed(&saved);
    assert_eq!(
        case.labels(&launched),
        vec!["launch:@tmp/games/RimWorldWin64.exe"],
        "the launch does not ask the same question again"
    );
}

#[test]
fn save_and_close_confirms_warnings_once() {
    let mut case = Loaded::with_confirmation(&[CORE_TOKEN, MISSING_TOKEN]);
    case.activate(HARMONY);

    let prompted = case.close_request();
    let warned = case.answer(&prompted, PromptAnswer::Save);
    assert_eq!(case.labels(&warned), vec!["prompt-warnings(1)"]);
    let saved = case.answer(&warned, PromptAnswer::Confirm);
    let closed = case.save_landed(&saved);

    assert_eq!(
        case.labels(&closed),
        vec!["close"],
        "one confirmation covers the whole operation"
    );
    assert_eq!(
        case.bytes(),
        document(&[CORE_TOKEN, MISSING_TOKEN, HARMONY_TOKEN]).as_bytes(),
        "the inherited unresolved token is preserved by the save it unblocked"
    );
}

#[test]
fn an_edit_while_a_warning_prompt_is_open_invalidates_the_answer() {
    let mut case = Loaded::with_confirmation(&[CORE_TOKEN, MISSING_TOKEN]);
    let revision = case.session.revision();

    let warned = case.save_request();
    assert_eq!(case.labels(&warned), vec!["prompt-warnings(1)"]);

    // The list changed while the question was open, so what the user was shown
    // is no longer what would be saved.
    case.activate(HARMONY);
    assert!(case.session.revision() > revision);
    let answered = case.answer(&warned, PromptAnswer::Confirm);

    assert_eq!(
        case.labels(&answered),
        vec!["prompt-warnings(1)"],
        "the acknowledgement is tied to the revision it was asked at"
    );
    let saving = case.answer(&answered, PromptAnswer::Confirm);
    assert_eq!(
        case.labels(&saving),
        vec![format!("save:{}", case.shown(&case.path))],
        "and a confirmation for the current revision is accepted"
    );
}

#[test]
fn dismissing_the_warnings_cancels_the_whole_operation() {
    let mut case = Loaded::with_confirmation(&[CORE_TOKEN, MISSING_TOKEN]);
    case.activate(HARMONY);
    let edits = case.working();

    let prompted = case.launch_request();
    let warned = case.answer(&prompted, PromptAnswer::Save);
    assert_eq!(case.labels(&warned), vec!["prompt-warnings(1)"]);
    let dismissed = case.answer(&warned, PromptAnswer::Dismiss);

    assert_no_effects(&dismissed, "no save, no launch, no report");
    assert_eq!(case.working(), edits, "the exact edits are still there");
    assert!(case.session.is_dirty());
    assert_eq!(
        case.bytes(),
        document(&[CORE_TOKEN, MISSING_TOKEN]).as_bytes(),
        "nothing was written"
    );

    // The cancelled operation left nothing in flight, and its refused
    // acknowledgement does not carry over to the next operation.
    let again = case.save_request();
    assert_eq!(
        case.labels(&again),
        vec!["prompt-warnings(1)"],
        "the next operation is asked to confirm again"
    );
    let saving = case.answer(&again, PromptAnswer::Confirm);
    assert_eq!(
        case.labels(&saving),
        vec![format!("save:{}", case.shown(&case.path))]
    );
}

#[test]
fn a_warning_answer_from_a_finished_operation_is_dropped() {
    let mut case = Loaded::with_confirmation(&[CORE_TOKEN, MISSING_TOKEN]);

    let warned = case.save_request();
    let dismissed = case.answer(&warned, PromptAnswer::Dismiss);
    assert_no_effects(&dismissed, "dismissing the warnings cancels the save");

    // The next operation gets a fresh prompt; the old id must not answer it.
    let again = case.save_request();
    assert_eq!(case.labels(&again), vec!["prompt-warnings(1)"]);
    let stray = case.answer(&warned, PromptAnswer::Confirm);
    assert_no_effects(&stray, "an answer for a finished operation runs nothing");
    let answered = case.answer(&again, PromptAnswer::Confirm);
    assert_eq!(
        case.labels(&answered),
        vec![format!("save:{}", case.shown(&case.path))],
        "and the real question is still outstanding"
    );
}

#[test]
fn turning_warning_confirmation_off_saves_a_warned_list_without_prompting() {
    let mut case = Loaded::without_confirmation(&[CORE_TOKEN, MISSING_TOKEN]);
    case.activate(HARMONY);

    let warned = case.save_request();
    assert_eq!(
        case.labels(&warned),
        vec![format!("save:{}", case.shown(&case.path))],
        "no question is asked when the installation says not to ask"
    );
    let landed = case.save_landed(&warned);
    assert_no_effects(&landed, "the save is the whole of the operation");
    assert_eq!(
        case.bytes(),
        document(&[CORE_TOKEN, MISSING_TOKEN, HARMONY_TOKEN]).as_bytes(),
        "the unresolved token is preserved exactly, and the save still happened"
    );
}

#[test]
fn an_error_severity_finding_is_confirmed_and_never_blocks_the_save() {
    // A repeated identity is reported as an `Error`-severity finding. It is a
    // content finding, so it is confirmed - and confirming it saves the list
    // faithfully, including the repetition.
    let mut case = Loaded::with_confirmation(&[CORE_TOKEN, CORE_TOKEN]);
    let severities: Vec<IssueSeverity> = case
        .session
        .validate()
        .iter()
        .map(|issue| issue.severity())
        .collect();
    assert!(
        severities.contains(&IssueSeverity::Error),
        "the premise: this list carries an Error-severity finding, got {severities:?}"
    );

    let warned = case.save_request();
    assert_eq!(case.labels(&warned), vec!["prompt-warnings(1)"]);
    let saving = case.answer(&warned, PromptAnswer::Confirm);
    assert_eq!(
        case.labels(&saving),
        vec![format!("save:{}", case.shown(&case.path))],
        "an Error-severity label is not an operational block"
    );

    let landed = case.save_landed(&saving);
    assert_no_effects(&landed, "the save is the whole of the operation");
    assert_eq!(
        case.bytes(),
        document(&[CORE_TOKEN, CORE_TOKEN]).as_bytes(),
        "the repeated entry is preserved, not dropped or merged"
    );
    assert!(!case.session.is_dirty());
}

#[test]
fn an_operational_failure_is_reported_even_with_confirmation_off() {
    // Switching warning confirmation off never bypasses a failure: the list
    // changed on disk, so the save cannot happen, whatever the setting says.
    let mut case = Loaded::without_confirmation(&[CORE_TOKEN]);
    case.activate(HARMONY);

    let saving = case.save_request();
    case.overwritten_by_another_writer();
    let landed = case.save_landed(&saving);

    let reported = failures(&landed);
    assert_eq!(
        reported.len(),
        1,
        "the conflict is the only thing that follows, got {landed:?}"
    );
    assert_eq!(reported[0].0, OperationKind::Save);
    assert!(
        reported[0].1.contains("changed since it was loaded"),
        "the message is the conflict itself: {}",
        reported[0].1
    );
    assert!(case.session.is_dirty(), "and the edits are still pending");
}

#[test]
fn a_stale_save_completion_is_never_acknowledged_or_claimed_to_roll_back() {
    let mut case = Loaded::without_confirmation(&[CORE_TOKEN]);
    case.activate(HARMONY);

    let prompted = case.launch_request();
    let saving = case.answer(&prompted, PromptAnswer::Save);
    assert_eq!(
        case.labels(&saving),
        vec![format!("save:{}", case.shown(&case.path))]
    );

    // An edit lands while the write is in flight, and then the write succeeds.
    case.activate(ROYALTY);
    let landed = case.save_landed(&saving);

    let reported = failures(&landed);
    assert_eq!(
        reported.len(),
        1,
        "the mismatch is reported and nothing else runs, got {landed:?}"
    );
    assert_eq!(reported[0].0, OperationKind::Save);
    assert!(
        reported[0].1.contains("still unsaved"),
        "the current edits are still pending: {}",
        reported[0].1
    );
    assert!(
        reported[0].1.contains("were written"),
        "the message states the disk fact: {}",
        reported[0].1
    );
    for word in ["cancel", "rolled", "reverted", "undid"] {
        assert!(
            !reported[0].1.to_lowercase().contains(word),
            "a completed write is never described as {} : {}",
            word,
            reported[0].1
        );
    }
    assert!(
        !case
            .labels(&landed)
            .iter()
            .any(|label| label.starts_with("launch")),
        "no launch follows a save that no longer matches: {:?}",
        case.labels(&landed)
    );
    assert!(
        case.session.is_dirty(),
        "the newer edit set is not marked as saved"
    );
    assert_eq!(
        case.session.loaded_config().raw,
        document(&[CORE_TOKEN]).as_bytes(),
        "the baseline is not advanced by a stale result"
    );
    assert_eq!(
        case.bytes(),
        document(&[CORE_TOKEN, HARMONY_TOKEN]).as_bytes(),
        "the file really does hold the bytes that were written"
    );
}

#[test]
fn a_completion_from_a_finished_operation_changes_nothing() {
    let mut case = Loaded::without_confirmation(&[CORE_TOKEN]);
    case.activate(HARMONY);

    let prompted = case.launch_request();
    let saving = case.answer(&prompted, PromptAnswer::Save);
    let save_operation = operation(&saving);
    let launched = case.save_landed(&saving);
    assert_eq!(
        case.labels(&launched),
        vec!["launch:@tmp/games/RimWorldWin64.exe"]
    );

    let repeated = case.reduce(WorkflowEvent::SaveFinished {
        operation: save_operation,
        outcome: SaveOutcome::Committed(
            parse_config(document(&[CORE_TOKEN]).as_bytes()).expect("the document must load"),
        ),
    });
    assert_no_effects(
        &repeated,
        "a second report for a finished save is dropped, not re-applied",
    );
    assert!(
        !case.session.is_dirty(),
        "and the acknowledged baseline stays where the first one put it"
    );
    assert_eq!(
        case.session.loaded_config().raw,
        document(&[CORE_TOKEN, HARMONY_TOKEN]).as_bytes()
    );
}

#[test]
fn saving_twice_uses_the_committed_baseline_not_the_old_bytes() {
    let mut case = Loaded::without_confirmation(&[CORE_TOKEN]);

    // First edit and save.
    case.activate(HARMONY);
    let first_saving = case.save_request();
    let first_landed = case.save_landed(&first_saving);
    assert_no_effects(&first_landed, "the first save ends on its own");
    let committed = case.bytes();
    assert_eq!(committed, document(&[CORE_TOKEN, HARMONY_TOKEN]).as_bytes());

    // A second edit is saved against what is on disk now, not against the
    // original load, so the conflict check passes.
    case.activate(ROYALTY);
    let second_saving = case.save_request();
    let WorkflowEffect::Save { baseline, .. } = second_saving.first().expect("the save effect")
    else {
        panic!("expected a save effect");
    };
    assert_eq!(
        baseline.raw, committed,
        "the second save is prepared against the bytes the first one wrote"
    );
    let second_landed = case.save_landed(&second_saving);
    assert_no_effects(&second_landed, "the second save ends on its own");
    assert_eq!(
        case.bytes(),
        document(&[CORE_TOKEN, HARMONY_TOKEN, ROYALTY_WRITTEN]).as_bytes()
    );
    assert!(!case.session.is_dirty());
}

// The launch adapter: what each platform would run.

#[test]
fn windows_starts_the_64_bit_executable_from_the_game_root() {
    let directory = TempDir::new().expect("a temporary directory");
    let root = directory.path().join("RimWorld");
    make_directory(&root);
    file(&root.join(WINDOWS_64), "synthetic\n");
    file(&root.join(WINDOWS_32), "synthetic\n");

    let command = build_at(&settings_for(&root), Platform::Windows, directory.path())
        .expect("a configured game root with an executable must build a command");
    assert_eq!(command.program, root.join(WINDOWS_64));
    assert_eq!(command.args, Vec::<OsString>::new(), "no arguments at all");
    assert_eq!(command.working_directory.as_deref(), Some(root.as_path()));
}

#[test]
fn windows_falls_back_to_the_32_bit_executable() {
    let directory = TempDir::new().expect("a temporary directory");
    let root = directory.path().join("RimWorld");
    make_directory(&root);
    file(&root.join(WINDOWS_32), "synthetic\n");

    let command = build_at(&settings_for(&root), Platform::Windows, directory.path())
        .expect("the 32-bit default must be found");
    assert_eq!(command.program, root.join(WINDOWS_32));
    assert_eq!(command.working_directory.as_deref(), Some(root.as_path()));
}

#[test]
fn linux_starts_the_64_bit_executable() {
    let directory = TempDir::new().expect("a temporary directory");
    let root = directory.path().join("RimWorld");
    make_directory(&root);
    executable(&root.join(LINUX_64));
    executable(&root.join(LINUX_32));

    let command = build_at(&settings_for(&root), Platform::Linux, directory.path())
        .expect("the 64-bit Linux default must be found");
    assert_eq!(command.program, root.join(LINUX_64));
    assert_eq!(command.args, Vec::<OsString>::new());
    assert_eq!(command.working_directory.as_deref(), Some(root.as_path()));
}

#[test]
#[cfg(unix)]
fn linux_skips_a_candidate_the_host_cannot_execute() {
    // Only a host with Unix permission bits can enforce this, so this case is
    // the host-specific half of the Linux default; a Windows host has no mode to
    // read and is not asked to certify it.
    let directory = TempDir::new().expect("a temporary directory");
    let root = directory.path().join("RimWorld");
    make_directory(&root);
    non_executable(&root.join(LINUX_64));
    executable(&root.join(LINUX_32));

    let command = build_at(&settings_for(&root), Platform::Linux, directory.path())
        .expect("the executable candidate must be chosen over the non-executable one");
    assert_eq!(command.program, root.join(LINUX_32));
}

#[test]
fn a_missing_game_executable_names_every_candidate_it_tried() {
    let directory = TempDir::new().expect("a temporary directory");
    let root = directory.path().join("RimWorld");
    make_directory(&root);

    let error = build_at(&settings_for(&root), Platform::Windows, directory.path())
        .expect_err("an empty game root has no executable to start");
    let message = error.to_string();
    assert!(
        message.contains(WINDOWS_64) && message.contains(WINDOWS_32),
        "the refusal lists what was looked for: {message}"
    );
    assert!(
        message.contains(&root.display().to_string()),
        "and names the root it looked in: {message}"
    );
}

#[test]
fn misconfigured_paths_are_refused_with_the_setting_named() {
    let directory = TempDir::new().expect("a temporary directory");
    let root = directory.path().join("RimWorld");

    let unconfigured = build_at(&Settings::default(), Platform::Windows, directory.path())
        .expect_err("nothing is configured, so nothing can be started");
    assert!(
        unconfigured.to_string().contains("game_root"),
        "the refusal says which setting is missing: {unconfigured}"
    );

    file(&root, "a file is not a game root\n");
    let wrong_kind = build_at(&settings_for(&root), Platform::Linux, directory.path())
        .expect_err("a file cannot be the game root");
    assert!(
        wrong_kind.to_string().contains("game_root"),
        "and which setting is the wrong kind: {wrong_kind}"
    );
}

#[test]
fn macos_opens_a_new_instance_of_the_configured_bundle() {
    let directory = TempDir::new().expect("a temporary directory");
    let bundle = directory.path().join("RimWorld.app");
    make_directory(&bundle);

    let command = build_at(&settings_for(&bundle), Platform::MacOS, directory.path())
        .expect("the configured bundle must open");
    assert_eq!(
        command.program,
        PathBuf::from("/usr/bin/open"),
        "the bundle is started through the macOS launcher, as a program, never a shell line"
    );
    assert_eq!(
        command.args,
        vec![OsString::from("-n"), OsString::from(&bundle)],
        "a new instance, with the bundle as its own argument"
    );
    assert_eq!(
        command.working_directory, None,
        "the start directory of `open` is not this build's business"
    );
}

#[test]
fn macos_never_opens_the_content_root() {
    let directory = TempDir::new().expect("a temporary directory");
    let bundle = directory.path().join("RimWorld.app");
    make_directory(&bundle);
    // The resource directory a 1.7-style install puts beside the bundle, and a
    // decoy bundle the settings point at as content rather than as an app.
    let resources = bundle.join("Contents/RimWorldWin_Data");
    make_directory(&resources);
    let decoy = directory.path().join("Decoy.app");
    make_directory(&decoy);

    let mut settings = settings_for(&bundle);
    settings.content_root = Some(decoy);
    let command = build_at(&settings, Platform::MacOS, directory.path())
        .expect("the configured bundle must open");
    assert_eq!(
        command.args[1],
        OsString::from(&bundle),
        "content_root is data, not an application to open"
    );
}

#[test]
fn macos_opens_the_overridden_bundle_instead() {
    let directory = TempDir::new().expect("a temporary directory");
    let configured = directory.path().join("RimWorld.app");
    make_directory(&configured);
    let override_bundle = directory.path().join("Rim World OTHER.app");
    make_directory(&override_bundle);

    let mut settings = settings_for(&configured);
    settings.executable_override = Some(override_bundle.clone());
    let command =
        build_at(&settings, Platform::MacOS, directory.path()).expect("the override must be used");
    assert_eq!(
        command.args,
        vec![OsString::from("-n"), OsString::from(&override_bundle)],
        "an explicit override chooses the bundle, spaces and all"
    );
}

#[test]
fn a_macos_launch_without_a_configured_bundle_is_refused() {
    let directory = TempDir::new().expect("a temporary directory");
    let settings = Settings {
        content_root: Some(directory.path().join("RimWorldWin_Data")),
        ..Settings::default()
    };

    let error = build_at(&settings, Platform::MacOS, directory.path())
        .expect_err("no bundle is configured, so there is nothing to open");
    let message = error.to_string();
    assert!(
        message.contains("game_root") && message.contains("executable_override"),
        "the refusal names both settings that could have named a bundle: {message}"
    );
}

#[test]
fn an_override_with_spaces_and_non_ascii_stays_one_argument() {
    let directory = TempDir::new().expect("a temporary directory");
    let root = directory.path().join("Rim World");
    make_directory(&root);
    let override_path = root.join("RimWorld 测试 1.5.exe");
    file(&override_path, "synthetic\n");

    let mut settings = settings_for(&root);
    settings.executable_override = Some(override_path.clone());
    let command = build_at(&settings, Platform::Windows, directory.path())
        .expect("an explicit path is used verbatim");
    assert_eq!(
        command.args,
        Vec::<OsString>::new(),
        "the path is the program, never an argument to something else"
    );
    assert_eq!(command.program, override_path);
    assert_eq!(command.working_directory.as_deref(), Some(root.as_path()));
}

#[test]
fn an_override_that_is_not_a_file_is_refused_with_its_path() {
    let directory = TempDir::new().expect("a temporary directory");
    let root = directory.path().join("RimWorld");
    make_directory(&root);
    let missing = root.join("Not There.exe");

    let mut settings = settings_for(&root);
    settings.executable_override = Some(missing.clone());
    let error = build_at(&settings, Platform::Windows, directory.path())
        .expect_err("an override that is not there is not a silent fallback");
    let message = error.to_string();
    assert!(
        message.contains(&missing.display().to_string()),
        "the refusal names the path it was given: {message}"
    );
    assert!(
        !message.contains(WINDOWS_64),
        "and never quietly falls back to a default: {message}"
    );
}

// The process boundary, exercised on this host with a harmless fixture.

#[test]
fn the_adapter_actually_starts_a_process_on_the_host() {
    let directory = TempDir::new().expect("a temporary directory");
    let command = LaunchCommand {
        program: std::env::current_exe().expect("this test binary must know itself"),
        // Listing the suite's tests is a run that does real work, exits by
        // itself, and cannot touch anything outside the parent's own binary.
        args: vec![OsString::from("--list")],
        working_directory: Some(directory.path().to_path_buf()),
    };

    let started = Instant::now();
    let mut child = spawn_launch(&command).expect("the fixture process must start");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "starting returned instead of waiting for the child"
    );
    assert_ne!(
        child.id(),
        std::process::id(),
        "the child is another process"
    );

    // Reaping here is the test's own choice, to prove the process really ran.
    let status = child
        .wait()
        .expect("the fixture process must be waitable by its owner");
    assert!(
        status.success(),
        "the fixture process exited cleanly: {status:?}"
    );
}

#[test]
fn starting_a_program_that_is_not_there_is_reported_with_its_path() {
    let directory = TempDir::new().expect("a temporary directory");
    let missing = directory.path().join("no-such-program");
    let command = LaunchCommand {
        program: missing.clone(),
        args: Vec::new(),
        working_directory: None,
    };

    let error = spawn_launch(&command).expect_err("a missing program cannot start");
    match error {
        rimsort_rust::domain::types::Error::Io { path, source } => {
            assert_eq!(
                path, missing,
                "the failure names the program the operating system would not start"
            );
            assert_eq!(
                source.kind(),
                std::io::ErrorKind::NotFound,
                "and carries the operating system's own reason"
            );
        }
        other => panic!("expected the filesystem refusal for {missing:?}, got {other}"),
    }
}

#[test]
#[cfg(unix)]
fn the_adapter_returns_while_the_fixture_is_still_running() {
    let directory = TempDir::new().expect("a temporary directory");
    let command = waiting_fixture(&directory);

    let mut child = spawn_launch(&command).expect("the fixture must start");

    // The handshake, not a wall clock: the fixture is still running when the
    // adapter returned, so returning was not waiting.
    assert!(
        child
            .try_wait()
            .expect("this child is ours to poll")
            .is_none(),
        "the fixture had not finished when spawn_launch returned"
    );

    release(&directory);
    let status = child
        .wait()
        .expect("the fixture must be waitable by its owner");
    assert!(
        status.success() && finished_path(&directory).exists(),
        "the released fixture ran to completion: {status:?}"
    );
}

#[test]
#[cfg(unix)]
fn a_dropped_handle_leaves_the_fixture_running_and_it_is_released_for_cleanup() {
    let directory = TempDir::new().expect("a temporary directory");
    let command = waiting_fixture(&directory);

    let child: Child = spawn_launch(&command).expect("the fixture must start");
    // Dropping the manager's handle is what closing the application does.
    drop(child);

    release(&directory);
    // A generous watchdog, for cleanup only: it bounds what this case can cost if
    // the fixture ever fails to run, and asserts nothing about speed.
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline && !finished_path(&directory).exists() {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        finished_path(&directory).exists(),
        "the fixture completed after its handle was dropped, so nothing here kills the game"
    );
}

// Review findings 1 to 7: adversarial sequences written before the repair.

#[test]
fn confirming_warnings_after_an_edit_made_a_clean_launch_dirty_asks_the_unsaved_question() {
    // A clean list with one finding goes straight to the warning question. An edit
    // made while that question is open must not be waved through by the
    // confirmation: a warning acknowledgement is not consent to launch over
    // unsaved edits.
    let mut case = Loaded::with_confirmation(&[CORE_TOKEN, MISSING_TOKEN]);
    assert!(!case.session.is_dirty(), "the premise: nothing pending");

    let warned = case.launch_request();
    assert_eq!(case.labels(&warned), vec!["prompt-warnings(1)"]);

    case.activate(HARMONY);
    let answered = case.answer(&warned, PromptAnswer::Confirm);
    assert_eq!(
        case.labels(&answered),
        vec!["prompt-unsaved-launch"],
        "launching now needs the save-or-cancel decision, not another confirmation"
    );

    let cancelled = case.answer(&answered, PromptAnswer::Cancel);
    assert_no_effects(
        &cancelled,
        "cancelling the unsaved question launches nothing",
    );
    assert!(case.session.is_dirty(), "and the edits are still pending");
    assert_eq!(
        case.bytes(),
        document(&[CORE_TOKEN, MISSING_TOKEN]).as_bytes()
    );
}

#[test]
fn confirming_warnings_after_the_last_finding_was_removed_still_asks_the_unsaved_question() {
    // The variant where the intervening edit leaves no findings at all: there is
    // nothing left to confirm, and yet launching over pending edits still has to
    // be refused until Save or Cancel is answered.
    let mut case = Loaded::with_confirmation(&[CORE_TOKEN, MISSING_TOKEN]);
    let warned = case.launch_request();
    assert_eq!(case.labels(&warned), vec!["prompt-warnings(1)"]);

    case.session
        .deactivate(1)
        .expect("the unresolved row is removable");
    assert_eq!(
        case.session.validate(),
        Vec::new(),
        "the premise: this edit removed the last finding"
    );
    assert!(case.session.is_dirty(), "and left a pending edit behind");

    let answered = case.answer(&warned, PromptAnswer::Confirm);
    assert_eq!(
        case.labels(&answered),
        vec!["prompt-unsaved-launch"],
        "no findings means no confirmation, but the unsaved gate still holds"
    );

    let saving = case.answer(&answered, PromptAnswer::Save);
    assert_eq!(
        case.labels(&saving),
        vec![format!("save:{}", case.shown(&case.path))],
        "and the only way forward through it is a save"
    );
    let launched = case.save_landed(&saving);
    assert_eq!(
        case.labels(&launched),
        vec!["launch:@tmp/games/RimWorldWin64.exe"]
    );
    assert_eq!(case.bytes(), document(&[CORE_TOKEN]).as_bytes());
}

#[test]
fn an_operation_never_borrows_another_sessions_bytes_through_its_own_path() {
    // Two sessions, one workflow. Same loaded bytes, same revision, different
    // files and different edits: answering the first session's question with the
    // second one may not produce a write at all, let alone a combination of the
    // first path with the second list.
    let mut a = Loaded::with_confirmation(&[CORE_TOKEN, MISSING_TOKEN]);
    let mut b = Loaded::with_confirmation(&[CORE_TOKEN, MISSING_TOKEN]);
    a.activate(ROYALTY);
    b.activate(HARMONY);
    assert_eq!(
        a.session.revision(),
        b.session.revision(),
        "the premise: the revision check alone cannot tell these apart"
    );

    let warned = a.save_request();
    assert_eq!(a.labels(&warned), vec!["prompt-warnings(1)"]);
    let b_baseline = b.session.loaded_config().raw.clone();
    let b_edits = b.working();

    let answered = a.workflow.reduce(
        &mut b.session,
        WorkflowEvent::PromptAnswered {
            prompt: prompt(&warned),
            answer: PromptAnswer::Confirm,
        },
    );
    assert_eq!(
        a.labels(&answered),
        Vec::<String>::new(),
        "a foreign session gets no effect, and above all no misdirected write"
    );
    assert_eq!(b.working(), b_edits, "the foreign session is untouched");
    assert_eq!(b.session.loaded_config().raw, b_baseline);
    assert!(b.session.is_dirty());
    assert!(
        a.session.is_dirty(),
        "and the real operation is still pending"
    );

    let same = a.answer(&warned, PromptAnswer::Confirm);
    assert_eq!(
        a.labels(&same),
        vec![format!("save:{}", a.shown(&a.path))],
        "the outstanding question still answers for its own session"
    );
}

#[test]
fn a_foreign_workflows_committed_save_never_closes_or_clears_this_one() {
    // Two independent workflows for two installations that hold identical bytes
    // and identical edits. One completed save from the first may not be what
    // marks the second clean and closes it, because the second file was never
    // written.
    let mut other = Loaded::without_confirmation(&[CORE_TOKEN]);
    other.activate(HARMONY);
    let saving = other.save_request();
    let foreign_operation = operation(&saving);
    let outcome = other.save_outcome(&saving);
    assert_no_effects(
        &other.reduce(WorkflowEvent::SaveFinished {
            operation: foreign_operation,
            outcome: outcome.clone(),
        }),
        "the other workflow adopts its own save",
    );

    let mut case = Loaded::without_confirmation(&[CORE_TOKEN]);
    case.activate(HARMONY);
    let prompted = case.close_request();
    assert_eq!(case.labels(&prompted), vec!["prompt-unsaved-close"]);
    let waiting = case.answer(&prompted, PromptAnswer::Save);
    assert_eq!(
        case.labels(&waiting),
        vec![format!("save:{}", case.shown(&case.path))],
        "this operation's own save has not run yet"
    );

    let adopted = case.reduce(WorkflowEvent::SaveFinished {
        operation: foreign_operation,
        outcome,
    });
    assert_no_effects(
        &adopted,
        "a completion from another workflow is not this one's save",
    );
    assert!(case.session.is_dirty(), "so the list is still unsaved here");
    assert_eq!(case.bytes(), document(&[CORE_TOKEN]).as_bytes());

    let landed = case.save_landed(&waiting);
    assert_eq!(
        case.labels(&landed),
        vec!["close"],
        "and its own completion is still accepted"
    );
}

#[test]
fn a_committed_document_that_is_not_the_one_this_operation_prepared_is_not_adopted() {
    // A valid `ModsConfig.xml` whose active list matches this session row for row
    // but whose other bytes say a different release wrote the file. Adopting it
    // would make unrelated bytes the baseline and launch or close on top of them.
    let mut case = Loaded::without_confirmation(&[CORE_TOKEN]);
    case.activate(HARMONY);
    let saving = case.save_request();
    let operation = operation(&saving);
    let unrelated =
        parse_config(document_with_version(&[CORE_TOKEN, HARMONY_TOKEN], "1.6").as_bytes())
            .expect("the unrelated document must be a valid configuration");

    let landed = case.reduce(WorkflowEvent::SaveFinished {
        operation,
        outcome: SaveOutcome::Committed(unrelated),
    });
    let reported = failures(&landed);
    assert_eq!(
        reported.len(),
        1,
        "the mismatch is reported and nothing continues, got {landed:?}"
    );
    assert_eq!(reported[0].0, OperationKind::Save);
    assert!(
        reported[0].1.contains("not the document"),
        "and says which fact failed: {}",
        reported[0].1
    );
    assert!(
        reported[0].1.contains("still unsaved"),
        "the current edits stay pending: {}",
        reported[0].1
    );
    for word in ["cancel", "rolled", "reverted", "undid"] {
        assert!(
            !reported[0].1.to_lowercase().contains(word),
            "a completed write is never described as {}: {}",
            word,
            reported[0].1
        );
    }
    assert!(case.session.is_dirty(), "nothing was adopted");
    assert_eq!(
        case.session.loaded_config().raw,
        document(&[CORE_TOKEN]).as_bytes(),
        "the baseline is still the load this session came from"
    );
    assert_eq!(
        case.bytes(),
        document(&[CORE_TOKEN]).as_bytes(),
        "nothing was written"
    );
}

#[test]
fn a_duplicate_confirm_for_an_older_prompt_does_not_authorize_the_new_one() {
    let mut case = Loaded::with_confirmation(&[CORE_TOKEN, MISSING_TOKEN]);
    let first = case.save_request();
    assert_eq!(case.labels(&first), vec!["prompt-warnings(1)"]);

    case.activate(HARMONY);
    let second = case.answer(&first, PromptAnswer::Confirm);
    assert_eq!(
        case.labels(&second),
        vec!["prompt-warnings(1)"],
        "the findings changed, so a fresh prompt is owed"
    );

    let replayed = case.answer(&first, PromptAnswer::Confirm);
    assert_no_effects(
        &replayed,
        "an answer that belongs to the superseded prompt authorizes nothing",
    );

    let saving = case.answer(&second, PromptAnswer::Confirm);
    assert_eq!(
        case.labels(&saving),
        vec![format!("save:{}", case.shown(&case.path))],
        "and the answer to the outstanding prompt still works"
    );
}

#[test]
fn a_duplicate_dismiss_for_an_older_prompt_does_not_cancel_the_new_one() {
    let mut case = Loaded::with_confirmation(&[CORE_TOKEN, MISSING_TOKEN]);
    let first = case.save_request();
    case.activate(HARMONY);
    let second = case.answer(&first, PromptAnswer::Confirm);
    assert_eq!(case.labels(&second), vec!["prompt-warnings(1)"]);

    let replayed = case.answer(&first, PromptAnswer::Dismiss);
    assert_no_effects(
        &replayed,
        "an answer that belongs to the superseded prompt cancels nothing",
    );
    let saving = case.answer(&second, PromptAnswer::Confirm);
    assert_eq!(
        case.labels(&saving),
        vec![format!("save:{}", case.shown(&case.path))],
        "the operation is still alive and its own question is still open"
    );
}

#[test]
fn an_unsaved_answer_replayed_from_an_older_prompt_resolves_nothing() {
    // The unsaved question and the confirmation question are different stages of
    // one operation: an answer carried over from the first may not start the save.
    let mut case = Loaded::with_confirmation(&[CORE_TOKEN, MISSING_TOKEN]);
    case.activate(HARMONY);
    let unsaved = case.launch_request();
    assert_eq!(case.labels(&unsaved), vec!["prompt-unsaved-launch"]);
    let warned = case.answer(&unsaved, PromptAnswer::Save);
    assert_eq!(case.labels(&warned), vec!["prompt-warnings(1)"]);

    let replayed = case.answer(&unsaved, PromptAnswer::Save);
    assert_no_effects(
        &replayed,
        "a replayed answer to the resolved prompt runs nothing",
    );
    assert!(
        case.session.is_dirty(),
        "and saves nothing behind the confirmation"
    );

    let saving = case.answer(&warned, PromptAnswer::Confirm);
    assert_eq!(
        case.labels(&saving),
        vec![format!("save:{}", case.shown(&case.path))]
    );
}

#[test]
fn relative_launch_paths_resolve_against_one_captured_basis() {
    let directory = TempDir::new().expect("a temporary directory");
    let games = directory.path().join("install");
    make_directory(&games);
    make_directory(&games.join("bin"));
    let override_path = games.join("bin/Rim World 测试");
    executable(&override_path);

    let settings = Settings {
        game_root: Some(PathBuf::from("install")),
        executable_override: Some(PathBuf::from("install/bin/Rim World 测试")),
        ..Settings::default()
    };
    let command = build_at(&settings, Platform::Linux, directory.path())
        .expect("a relative root and override must resolve against the captured basis");
    assert_eq!(
        command.program, override_path,
        "the program the adapter validated is the program it names, absolutely"
    );
    assert_eq!(command.working_directory.as_deref(), Some(games.as_path()));
}

#[test]
#[cfg(unix)]
fn a_relative_root_and_override_actually_spawn_from_the_resolved_directory() {
    let directory = TempDir::new().expect("a temporary directory");
    let games = directory.path().join("install");
    make_directory(&games.join("bin"));
    let marker = directory.path().join("spawned");
    let launcher = games.join("bin/Rim World 测试");
    file(
        &launcher,
        &format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    );
    make_executable(&launcher);

    let settings = Settings {
        game_root: Some(PathBuf::from("install")),
        executable_override: Some(PathBuf::from("install/bin/Rim World 测试")),
        ..Settings::default()
    };
    let command = build_at(&settings, Platform::Linux, directory.path())
        .expect("the relative installation must be resolvable");
    let mut child = spawn_launch(&command).expect("the resolved relative launcher must start");
    let status = child.wait().expect("the fixture must be waitable");

    assert!(
        status.success() && marker.exists(),
        "the process started from the resolved directory, not from a path the \
         adapter validated somewhere else: {status:?}"
    );
}

#[test]
fn macos_refuses_a_root_that_is_not_an_explicit_bundle() {
    let directory = TempDir::new().expect("a temporary directory");
    let resources = directory.path().join("RimWorldWin_Data");
    make_directory(&resources);

    let error = build_at(&settings_for(&resources), Platform::MacOS, directory.path())
        .expect_err("a resource directory is not an application to open");
    let message = error.to_string();
    assert!(
        message.contains(".app") && message.contains("game_root"),
        "the refusal names the setting and the bundle kind it expects: {message}"
    );
}

#[test]
fn macos_refuses_a_non_bundle_override_without_falling_back() {
    let directory = TempDir::new().expect("a temporary directory");
    let bundle = directory.path().join("RimWorld.app");
    make_directory(&bundle);
    let resources = directory.path().join("RimWorldWin_Data");
    make_directory(&resources);

    let settings = Settings {
        executable_override: Some(resources.clone()),
        ..settings_for(&bundle)
    };
    let error = build_at(&settings, Platform::MacOS, directory.path())
        .expect_err("an override that is not a bundle is not quietly replaced");
    let message = error.to_string();
    assert!(
        message.contains("executable_override")
            && message.contains(&resources.display().to_string()),
        "the refusal names the override it rejected: {message}"
    );
    assert!(
        !message.contains(&bundle.display().to_string()),
        "and never falls back to the configured root: {message}"
    );
}

#[test]
fn a_changed_warning_preference_applies_to_the_next_operation() {
    // The owner choice: the latest preference is captured per new operation, so
    // switching confirmation off takes effect without rebuilding or refreshing.
    let mut case = Loaded::with_confirmation(&[CORE_TOKEN, MISSING_TOKEN]);
    case.settings.confirm_warnings = false;

    let saving = case.save_request();
    assert_eq!(
        case.labels(&saving),
        vec![format!("save:{}", case.shown(&case.path))],
        "a new operation sees the preference the user set"
    );

    let mut switched = Loaded::without_confirmation(&[CORE_TOKEN, MISSING_TOKEN]);
    switched.settings.confirm_warnings = true;
    let warned = switched.save_request();
    assert_eq!(
        switched.labels(&warned),
        vec!["prompt-warnings(1)"],
        "and switching it back on takes effect for the next operation too"
    );
}

#[test]
fn an_operation_in_flight_keeps_the_preference_it_started_with() {
    let mut case = Loaded::without_confirmation(&[CORE_TOKEN, MISSING_TOKEN]);
    case.activate(HARMONY);

    let unsaved = case.launch_request();
    assert_eq!(case.labels(&unsaved), vec!["prompt-unsaved-launch"]);
    case.settings.confirm_warnings = true;

    let saving = case.answer(&unsaved, PromptAnswer::Save);
    assert_eq!(
        case.labels(&saving),
        vec![format!("save:{}", case.shown(&case.path))],
        "the operation that started without confirmation keeps that preference"
    );
    let launched = case.save_landed(&saving);
    assert_eq!(
        case.labels(&launched),
        vec!["launch:@tmp/games/RimWorldWin64.exe"],
        "and is not interrupted by a prompt it never agreed to"
    );
    assert_no_effects(
        &case.spawn_accepted(&launched),
        "the accepted spawn ends this operation",
    );

    let next = case.save_request();
    assert_eq!(
        case.labels(&next),
        vec!["prompt-warnings(1)"],
        "the next operation captures the current preference"
    );
}

#[test]
fn a_refresh_reads_the_candidate_it_was_requested_for() {
    let mut case = Loaded::without_confirmation(&[CORE_TOKEN, ROYALTY_TOKEN]);
    let candidate = case.root().join("next/ModsConfig.xml");
    case.settings.config_path = Some(candidate.clone());

    let refreshed = case.refresh_request_for(candidate.as_path());
    assert_eq!(
        case.labels(&refreshed),
        vec![format!("refresh:{}", case.shown(&candidate))],
        "the request names the installation to re-read, not whatever path is \
         current when the result arrives"
    );
}

#[test]
fn a_saved_refresh_still_advances_the_old_baseline_when_the_re_read_fails() {
    let mut case = Loaded::without_confirmation(&[CORE_TOKEN]);
    case.activate(HARMONY);

    let prompted = case.refresh_request();
    let saving = case.answer(&prompted, PromptAnswer::Save);
    let refreshed = case.save_landed(&saving);
    assert_eq!(
        case.labels(&refreshed),
        vec![format!("refresh:{}", case.shown(&case.path))]
    );
    let reported = case.refresh_failed(&refreshed);

    assert_eq!(
        case.labels(&reported),
        vec!["report-refresh:the drive is not there"]
    );
    assert_eq!(
        case.session.loaded_config().raw,
        document(&[CORE_TOKEN, HARMONY_TOKEN]).as_bytes(),
        "the save that really landed is not rolled back by a failed re-read"
    );
    assert!(!case.session.is_dirty());
}

// Task 5 fix 2: the three gaps the second review left open, written first.

/// A loaded installation whose working list carries a finding, so a save inside any
/// operation owes the warning question when the user asked to be asked.
fn warned_and_dirty(case: &mut Loaded) {
    case.activate(HARMONY);
    assert!(
        !case.session.validate().is_empty(),
        "the premise: this list carries a content finding"
    );
    assert!(case.session.is_dirty(), "and a pending edit");
}

#[test]
fn refreshing_a_dirty_warned_list_confirms_before_it_saves() {
    // R1: the save that a refresh is gated behind is a save like any other, so the
    // warning preference the user holds governs it. Refreshing is not a reason to
    // write a warned-about list without asking.
    let mut case = Loaded::with_confirmation(&[CORE_TOKEN, MISSING_TOKEN]);
    warned_and_dirty(&mut case);

    let prompted = case.refresh_request();
    assert_eq!(case.labels(&prompted), vec!["prompt-unsaved-refresh"]);
    let warned = case.answer(&prompted, PromptAnswer::Save);
    assert_eq!(
        case.labels(&warned),
        vec!["prompt-warnings(1)"],
        "the save step asks the content question before it writes"
    );
    let saving = case.answer(&warned, PromptAnswer::Confirm);
    assert_eq!(
        case.labels(&saving),
        vec![format!("save:{}", case.shown(&case.path))]
    );
    let refreshed = case.save_landed(&saving);
    assert_eq!(
        case.labels(&refreshed),
        vec![format!("refresh:{}", case.shown(&case.path))],
        "and the re-read follows the save it was gated behind"
    );
}

#[test]
fn dismissing_the_refresh_warnings_neither_saves_nor_reads() {
    let mut case = Loaded::with_confirmation(&[CORE_TOKEN, MISSING_TOKEN]);
    warned_and_dirty(&mut case);
    let loaded = case.bytes();

    let prompted = case.refresh_request();
    let warned = case.answer(&prompted, PromptAnswer::Save);
    assert_eq!(case.labels(&warned), vec!["prompt-warnings(1)"]);
    let dismissed = case.answer(&warned, PromptAnswer::Dismiss);

    assert_no_effects(&dismissed, "dismissing cancels the save and the re-read");
    assert_eq!(case.bytes(), loaded, "nothing was written");
    assert!(case.session.is_dirty(), "and nothing was discarded either");
}

#[test]
fn a_preference_toggled_while_a_refresh_waits_does_not_change_what_it_captured() {
    let mut case = Loaded::with_confirmation(&[CORE_TOKEN, MISSING_TOKEN]);
    warned_and_dirty(&mut case);

    let prompted = case.refresh_request();
    case.settings.confirm_warnings = false;
    let warned = case.answer(&prompted, PromptAnswer::Save);
    assert_eq!(
        case.labels(&warned),
        vec!["prompt-warnings(1)"],
        "the operation keeps the preference it started with"
    );
    let saving = case.answer(&warned, PromptAnswer::Confirm);
    assert_eq!(
        case.labels(&saving),
        vec![format!("save:{}", case.shown(&case.path))]
    );
}

#[test]
fn a_refresh_without_a_save_reads_without_confirming_the_old_list() {
    // A clean list needs no content confirmation to be re-read: nothing is written
    // and the game is not started.
    let mut case = Loaded::with_confirmation(&[CORE_TOKEN, MISSING_TOKEN]);
    assert!(!case.session.is_dirty(), "the premise: nothing pending");

    let refreshed = case.refresh_request();
    assert_eq!(
        case.labels(&refreshed),
        vec![format!("refresh:{}", case.shown(&case.path))],
        "the re-read goes straight ahead"
    );
}

#[test]
fn a_candidate_that_cannot_say_where_it_reads_is_not_a_refresh_target() {
    // R2: there is one authoritative location, read out of the captured settings, so
    // a candidate cannot name one file here and another there. What it can do is fail
    // to name any, and that is refused before anything is emitted.
    let mut case = Loaded::without_confirmation(&[CORE_TOKEN]);
    let mut target = candidate(&case.path);
    target.settings.config_path = None;

    let refused = case.refresh_target(target);
    let reported = failures(&refused);
    assert_eq!(
        reported.len(),
        1,
        "the unusable candidate is reported and nothing else runs: {refused:?}"
    );
    assert_eq!(reported[0].0, OperationKind::Refresh);
    assert!(
        reported[0]
            .1
            .starts_with("the refresh target is not usable"),
        "and says what is missing: {}",
        reported[0].1
    );
    assert!(
        !case
            .labels(&refused)
            .iter()
            .any(|label| label.starts_with("refresh:")),
        "no re-read is dispatched for a candidate that cannot say where it reads"
    );
    assert!(
        !case.session.is_dirty(),
        "a refused candidate touches nothing at all"
    );
}

#[test]
fn a_candidate_without_a_target_release_is_not_a_refresh_target() {
    let mut case = Loaded::without_confirmation(&[CORE_TOKEN]);
    let mut target = candidate(&case.path);
    target.settings.target_version = None;

    let refused = case.refresh_target(target);
    let reported = failures(&refused);
    assert_eq!(
        reported.len(),
        1,
        "a replacement cannot be validated against a release nobody named: {refused:?}"
    );
    assert!(
        reported[0]
            .1
            .starts_with("the refresh target is not usable"),
        "and the refusal names what is missing: {}",
        reported[0].1
    );
}

#[test]
fn a_refresh_result_for_a_different_release_installs_nothing() {
    let mut case = Loaded::without_confirmation(&[CORE_TOKEN]);
    let refreshed = case.refresh_request();
    assert_eq!(
        case.labels(&refreshed),
        vec![format!("refresh:{}", case.shown(&case.path))]
    );
    let operation = operation(&refreshed);

    let mismatched = case.reduce(WorkflowEvent::RefreshFinished {
        operation,
        outcome: RefreshOutcome::Reloaded(Box::new(RefreshReplacement {
            origin: candidate(&case.path),
            config: parse_config(document(&[CORE_TOKEN]).as_bytes()).expect("the file must load"),
            catalog: catalog(),
            target_version: version("1.6.0"),
            diagnostics: Vec::new(),
        })),
    });

    let labels = case.labels(&mismatched);
    assert_eq!(
        labels,
        vec!["report-refresh:the refresh result names a different release than this target"],
        "a result scanned for another release is not this candidate's replacement"
    );
}

#[test]
fn a_refresh_result_that_is_not_the_load_of_its_own_bytes_installs_nothing() {
    let mut case = Loaded::without_confirmation(&[CORE_TOKEN]);
    let refreshed = case.refresh_request();
    let operation = operation(&refreshed);
    let mut config = parse_config(document(&[CORE_TOKEN]).as_bytes()).expect("the file must load");
    config.version = None;

    let mismatched = case.reduce(WorkflowEvent::RefreshFinished {
        operation,
        outcome: RefreshOutcome::Reloaded(Box::new(RefreshReplacement {
            origin: candidate(&case.path),
            config,
            catalog: catalog(),
            target_version: version(TARGET),
            diagnostics: Vec::new(),
        })),
    });

    let labels = case.labels(&mismatched);
    assert_eq!(
        labels,
        vec!["report-refresh:the refresh result is not the load of its own bytes"],
        "an internally inconsistent snapshot is never installed"
    );
}

#[test]
fn a_windows_target_still_resolves_relative_paths_under_the_captured_basis() {
    // Path syntax belongs to the host, so this Linux case can only show what the
    // adapter promises on every host: a relative configured root is read from the
    // captured basis, the file that gets validated is the file the command names,
    // and nothing is left for the child's own directory to decide. The Windows
    // drive-relative rule itself is pinned by the Windows-guarded cases in
    // `src/integrations/launch.rs`, which cannot run on this host.
    let directory = TempDir::new().expect("a temporary directory");
    let games = directory.path().join("install");
    make_directory(&games);
    executable(&games.join(WINDOWS_64));
    let basis = directory.path().to_path_buf();
    let settings = Settings {
        game_root: Some(PathBuf::from("install")),
        ..Settings::default()
    };

    let command = build_at(&settings, Platform::Windows, &basis)
        .expect("a relative root resolves under the captured basis");
    assert_eq!(
        command.program,
        games.join(WINDOWS_64),
        "the validated file is the program that is named"
    );
    assert!(
        command.program.is_absolute(),
        "and the command carries no path whose meaning depends on a current directory"
    );
    assert_eq!(command.working_directory.as_deref(), Some(games.as_path()));

    let outside = build_at(&settings, Platform::Windows, Path::new("relative/basis"))
        .expect_err("a basis that is itself relative cannot anchor anything");
    assert!(
        outside.to_string().contains("not an absolute directory"),
        "the refusal says why: {outside}"
    );
}

#[test]
fn a_complete_refresh_replacement_is_installed_and_the_new_pair_runs_on_it() {
    // R2 acceptance: the whole owned payload - a different config file, a different
    // catalog, a different release, different launch settings and a real diagnostic -
    // survives unchanged into exactly one matching InstallRefresh, and a caller can
    // build the next session and workflow from that payload alone, with no reference
    // to whatever settings it happens to hold now.
    let mut case = Loaded::without_confirmation(&[CORE_TOKEN]);
    let other = Loaded::without_confirmation(&[CORE_TOKEN, ROYALTY_TOKEN]);

    let mut target = candidate(&other.path);
    target.settings.game_root = Some(other.games.clone());
    target.settings.target_version = Some(version("1.6.0"));
    let requested = target.clone();

    let refreshed = case.refresh_target(target);
    assert_eq!(
        case.labels(&refreshed),
        vec![format!("refresh:{}", case.shown(&other.path))],
        "the re-read names the candidate it was asked for"
    );
    let refreshed_operation = operation(&refreshed);

    // The caller keeps editing its own form after the request. None of it may reach
    // the operation that is already running.
    case.settings.config_path = Some(case.root().join("later/ModsConfig.xml"));
    case.settings.game_root = Some(case.root().join("later"));

    let replacement = RefreshReplacement {
        origin: requested.clone(),
        config: parse_config(document(&[CORE_TOKEN, ROYALTY_TOKEN]).as_bytes())
            .expect("the candidate's file must load"),
        catalog: other_catalog(),
        target_version: version("1.6.0"),
        diagnostics: vec![rimsort_rust::integrations::scan::ScanDiagnostic::new(
            rimsort_rust::domain::types::DiagnosticSeverity::Warning,
            other.games.join("broken/About.xml"),
            "reading About.xml",
            "the file could not be read",
        )],
    };
    let installed = case.reduce(WorkflowEvent::RefreshFinished {
        operation: refreshed_operation,
        outcome: RefreshOutcome::Reloaded(Box::new(replacement)),
    });

    let labels = case.labels(&installed);
    assert_eq!(
        labels,
        vec![format!("install-refresh:{}", case.shown(&other.path))],
        "exactly one installation effect, for the captured candidate"
    );
    let Some(WorkflowEffect::InstallRefresh {
        target: installed_target,
        replacement: delivered,
        ..
    }) = installed.first()
    else {
        panic!("expected one install effect, got {installed:?}");
    };
    assert_eq!(
        installed_target, &requested,
        "the candidate the caller asked for arrives unchanged"
    );
    assert_eq!(
        delivered.target_version,
        version("1.6.0"),
        "with the release it was validated against"
    );
    assert_eq!(
        delivered.diagnostics.len(),
        1,
        "and the diagnostics the read produced, not a summary of them"
    );
    assert_eq!(
        delivered.config.raw,
        document(&[CORE_TOKEN, ROYALTY_TOKEN]).as_bytes(),
        "and the configuration bytes that were read"
    );

    // The next installation, built only out of what the workflow accepted.
    let Some(WorkflowEffect::InstallRefresh {
        target: installed_target,
        replacement,
        ..
    }) = installed.into_iter().next()
    else {
        panic!("expected the install effect");
    };
    let replacement = *replacement;
    let path = installed_target
        .config_path()
        .expect("the candidate names its own file")
        .to_path_buf();
    let mut session = Session::new(
        replacement.catalog,
        replacement.config,
        path,
        replacement.target_version,
    );
    let mut workflow = Workflow::new(&installed_target.settings, Platform::Windows, case.root());

    // The applied installation is the one in force: a save on the new pair writes the
    // candidate's file and a launch starts the candidate's game root, neither of them
    // the caller's edited settings.
    session
        .activate(&id(EXTRA))
        .expect("the candidate's catalog is the one that was installed");
    let saving = workflow.reduce(
        &mut session,
        WorkflowEvent::SaveRequested {
            confirm_warnings: false,
        },
    );
    let Some(WorkflowEffect::Save {
        path: save_path,
        baseline,
        prepared,
        ..
    }) = saving.first()
    else {
        panic!("expected a save effect for the new installation, got {saving:?}")
    };
    assert_eq!(
        save_path, &other.path,
        "the new installation saves where it came from"
    );
    assert_eq!(
        baseline.raw,
        document(&[CORE_TOKEN, ROYALTY_TOKEN]).as_bytes(),
        "against the bytes the payload carried, not a re-read of anything"
    );
    let outcome = match commit_save(save_path, baseline, prepared) {
        Ok(committed) => SaveOutcome::Committed(committed),
        Err(error) => panic!("the new installation must save for real: {error}"),
    };
    let saved_operation = operation(&saving);
    assert_no_effects(
        &workflow.reduce(
            &mut session,
            WorkflowEvent::SaveFinished {
                operation: saved_operation,
                outcome,
            },
        ),
        "and the save ends normally",
    );
    assert_eq!(
        fs::read(&other.path).expect("the candidate's file must be readable"),
        document(&[CORE_TOKEN, ROYALTY_TOKEN, EXTRA_TOKEN]).as_bytes(),
        "the candidate's own file holds the new list"
    );

    let launched = workflow.reduce(
        &mut session,
        WorkflowEvent::LaunchRequested {
            confirm_warnings: false,
        },
    );
    let command = only_command(&launched);
    assert_eq!(
        command.program,
        other.games.join(WINDOWS_64),
        "the launch is built from the installed candidate's settings"
    );
    assert_eq!(
        command.working_directory.as_deref(),
        Some(other.games.as_path()),
        "not from whatever the caller's form says now"
    );
}

/// The other installation's catalog: a different set of copies, supporting the
/// release that candidate names.
fn other_catalog() -> Catalog {
    Catalog::new(vec![
        record_supported(CORE, Source::Official, "ludeon.rimworld", &["1.6"]),
        record_supported(
            ROYALTY,
            Source::Official,
            "ludeon.rimworld.royalty",
            &["1.6"],
        ),
        record_supported(EXTRA, Source::Local, "extra.mod", &["1.6"]),
    ])
    .expect("the other catalog must build")
}

#[test]
fn a_refresh_result_for_another_candidate_installs_nothing() {
    // An operation id says which request a result answers. It does not say the result
    // is about the installation that request named, so the payload carries its own
    // origin and the workflow compares it with the captured candidate before anything
    // may be installed.
    let mut case = Loaded::without_confirmation(&[CORE_TOKEN]);
    let refreshed = case.refresh_request();
    let operation = operation(&refreshed);
    let elsewhere = case.root().join("other-install/ModsConfig.xml");

    let mismatched = case.reduce(WorkflowEvent::RefreshFinished {
        operation,
        outcome: RefreshOutcome::Reloaded(Box::new(RefreshReplacement {
            origin: candidate(&elsewhere),
            config: parse_config(document(&[CORE_TOKEN]).as_bytes()).expect("the file must load"),
            catalog: catalog(),
            target_version: version(TARGET),
            diagnostics: Vec::new(),
        })),
    });

    assert_eq!(
        case.labels(&mismatched),
        vec![
            "report-refresh:the refresh result is not about the candidate this operation asked for"
        ],
        "a result read for another installation is refused, not installed"
    );
    assert!(
        !case.session.is_dirty(),
        "the old session is untouched, and the failed refresh leaves it in force"
    );
    assert_eq!(
        case.session.loaded_config().raw,
        document(&[CORE_TOKEN]).as_bytes(),
        "the old installation's own bytes are still its baseline"
    );
}

#[test]
fn a_dirty_warned_refresh_with_confirmation_disabled_saves_and_still_reads() {
    // The positive control for the disabled branch: the list carries findings and is
    // dirty, but the preference this operation captured says not to ask, so the save
    // runs without a content prompt - and the re-read it was gating still follows.
    let mut case = Loaded::without_confirmation(&[CORE_TOKEN, MISSING_TOKEN]);
    warned_and_dirty(&mut case);

    let prompted = case.refresh_request();
    assert_eq!(case.labels(&prompted), vec!["prompt-unsaved-refresh"]);
    let saving = case.answer(&prompted, PromptAnswer::Save);
    assert_eq!(
        case.labels(&saving),
        vec![format!("save:{}", case.shown(&case.path))],
        "the preference this operation started with is off, so nothing is asked"
    );
    let refreshed = case.save_landed(&saving);
    assert_eq!(
        case.labels(&refreshed),
        vec![format!("refresh:{}", case.shown(&case.path))],
        "and the refresh proceeds on the save it gated behind"
    );
    assert_eq!(
        case.bytes(),
        document(&[CORE_TOKEN, MISSING_TOKEN, HARMONY_TOKEN]).as_bytes(),
        "the inherited unresolved token is preserved by the write it unblocked"
    );
}
