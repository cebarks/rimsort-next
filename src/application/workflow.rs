//! The save, launch, close and refresh workflow: decisions, in order.
//!
//! A workflow owns no state outside the decisions it makes. It turns one
//! [`WorkflowEvent`] into the ordered [`WorkflowEffect`] list that answers it, and
//! it is the only place that decides *sequence*: which question must be answered
//! first, whether the pending edits have to be resolved before anything else runs,
//! whether the content findings have been confirmed for the revision they were
//! seen at, and whether a result that arrived still belongs to the operation it
//! claims to finish.
//!
//! Nothing here writes a file or starts a process. A `Save` effect
//! carries the bytes [`crate::integrations::config::commit_save`] needs and
//! nothing else; the caller performs the write and reports back with
//! [`WorkflowEvent::SaveFinished`]. The same holds for `Launch` and `Refresh`.
//! Reading is another matter: preparing a save re-reads the loaded document, and
//! [`build_launch`] looks up which executable the platform would start, because a
//! command built without looking cannot say what it tried. Neither writes.
//! That separation is what lets a test assert the exact ordered sequence of a
//! save-and-launch, of a failed save that must not launch or close, and of a late
//! result that must not discard anybody's edits.
//!
//! # What an operation is bound to
//!
//! One workflow belongs to one loaded session and one installation. It captures
//! the [`Settings`] and the path basis the session was loaded with, so a launch is
//! always built from the game root and executable override of *that* installation:
//! a settings form the user is still editing, and a refresh that failed, can never
//! redirect the old session's save or launch. Each operation is additionally bound
//! to the session it started on and, for a refresh, to the candidate it was asked
//! for, so no operation can pair one installation's path with another's bytes. A
//! successful refresh hands the caller the complete replacement it read for that
//! candidate, and the workflow that goes with the new installation is built
//! separately.
//!
//! # Warning confirmation is not permission to fail quietly
//!
//! Content findings - an unresolved token, a repeated identity, an unusable
//! record - are confirmed once per operation against the revision they were seen
//! at, when the preference the user held when that operation started asks for
//! confirmation. A preference changed afterwards belongs to the next operation,
//! never to one already in flight. A finding whose severity is
//! [`IssueSeverity::Error`] is still only a finding: it does not block a faithful
//! save or launch on its own. Confirmation covers nothing else. An operational
//! failure - a save that did not commit, a spawn the operating system refused -
//! is reported and stops the operation whatever `confirm_warnings` says.
//!
//! [`IssueSeverity::Error`]: crate::domain::validation::IssueSeverity::Error

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::application::session::Session;
use crate::domain::catalog::Catalog;
use crate::domain::types::{Error, GameVersion};
use crate::domain::validation::Issue;
use crate::integrations::config::{ConfigSnapshot, PreparedSave, parse_config, prepare_save};
use crate::integrations::launch::{LaunchCommand, Platform, build_launch};
use crate::integrations::scan::ScanDiagnostic;
use crate::integrations::settings::Settings;

/// Every operation and every prompt this process ever issues gets its own number
/// from here, so an identity handed out by one workflow can never be mistaken for
/// an identity handed out by another. Collisions are a safety problem, not a
/// tidiness problem: a completion that looks like its own would let one
/// installation adopt another installation's write.
static NEXT_IDENTITY: AtomicU64 = AtomicU64::new(1);

/// The next operation or prompt identity.
fn take_identity() -> u64 {
    NEXT_IDENTITY.fetch_add(1, Ordering::Relaxed)
}

/// Which user request or completed effect the reducer is being told about.
///
/// A completion always names the operation it belongs to, so a result that arrives
/// after its operation is gone cannot be mistaken for a current one.
#[derive(Debug)]
pub enum WorkflowEvent {
    /// The user asked to save the working list on its own.
    SaveRequested {
        /// Whether this operation asks before content findings, as the user's
        /// preference reads right now.
        confirm_warnings: bool,
    },
    /// The user asked to start the game.
    LaunchRequested { confirm_warnings: bool },
    /// The user asked to close the application.
    CloseRequested { confirm_warnings: bool },
    /// The user asked to re-read the installation the given candidate describes.
    RefreshRequested {
        target: RefreshTarget,
        /// Whether the save this refresh may need asks before content findings,
        /// as the user's preference reads right now. A re-read that needs no save
        /// asks nothing.
        confirm_warnings: bool,
    },
    /// The user answered the prompt this workflow is waiting on.
    PromptAnswered {
        /// The identity of the prompt effect that was answered. An answer for a
        /// prompt that has been superseded authorizes nothing.
        prompt: PromptId,
        answer: PromptAnswer,
    },
    /// A `Save` effect finished. A committed save is a filesystem fact: this
    /// reports what happened, it does not offer to undo it.
    SaveFinished {
        operation: OperationId,
        outcome: SaveOutcome,
    },
    /// A `Launch` effect finished, with the process identifier the operating
    /// system accepted or the reason it refused.
    LaunchFinished {
        operation: OperationId,
        outcome: LaunchOutcome,
    },
    /// A `Refresh` effect finished, with the list that was re-read or the reason
    /// nothing was.
    RefreshFinished {
        operation: OperationId,
        outcome: RefreshOutcome,
    },
}

/// The user's answer to a prompt the workflow emitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptAnswer {
    /// Save the pending edits as part of this operation.
    Save,
    /// Drop the pending edits. Never available before a launch: launching means
    /// saving first, or not launching.
    Discard,
    /// Stop this operation and change nothing.
    Cancel,
    /// Accept the content findings and continue.
    Confirm,
    /// Refuse the content findings, which cancels the whole operation.
    Dismiss,
}

/// The result of a `Save` effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SaveOutcome {
    /// The bytes were replaced. This is the snapshot they read as now, which is
    /// what the session's next baseline has to be.
    Committed(ConfigSnapshot),
    /// The file was not replaced: it changed under us, it could not be written,
    /// or the write failed. The pending edits stay.
    Failed(OperationFailure),
}

/// The result of a `Launch` effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchOutcome {
    /// The operating system accepted the process. The game may still fail to
    /// load, which is not this build's claim to make.
    Spawned { process_id: u32 },
    /// No process was created.
    Failed(OperationFailure),
}

/// The result of a `Refresh` effect.
#[derive(Debug)]
pub enum RefreshOutcome {
    /// The candidate was read completely. This is the whole replacement for it -
    /// configuration, catalog, target release and the path-aware problems that
    /// came with them - owned by the event and handed on unchanged.
    Reloaded(Box<RefreshReplacement>),
    /// Nothing was read successfully, so the old installation stays in force.
    Failed(OperationFailure),
}

/// The installation a refresh was requested for: one captured settings record,
/// held from the moment the request arrived so nothing later edited by the caller
/// can retarget the operation.
///
/// There are deliberately no separate `path` or version fields to keep in step with
/// these settings: where the candidate's list is read from and which release it is
/// validated against are read out of the one captured record, so a candidate cannot
/// name one file in one field and another in the next.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefreshTarget {
    /// The candidate's own paths, target release and executable setup.
    pub settings: Settings,
}

impl RefreshTarget {
    /// The configuration file this candidate's list is read from.
    #[must_use]
    pub fn config_path(&self) -> Option<&Path> {
        self.settings.config_path.as_deref()
    }

    /// The release this candidate's mod list is validated against.
    #[must_use]
    pub fn target_version(&self) -> Option<&GameVersion> {
        self.settings.target_version.as_ref()
    }

    /// Why this candidate cannot be read at all, if it cannot.
    ///
    /// A refresh that cannot say which file it reads, or which release the result is
    /// for, would produce a replacement nobody can validate, so the request is
    /// refused before anything is dispatched.
    fn refusal(&self) -> Option<String> {
        if self.config_path().is_none() {
            return Some("no config_path is configured".to_string());
        }
        if self.target_version().is_none() {
            return Some("no target_version is configured".to_string());
        }
        None
    }
}

/// Everything one refresh read for a candidate: the complete replacement, not a
/// pointer back to mutable state the caller may have edited since.
#[derive(Debug)]
pub struct RefreshReplacement {
    /// The candidate this result was produced for, carried back so the workflow can
    /// compare it with the one it captured instead of trusting an operation id.
    pub origin: RefreshTarget,
    /// The candidate's configuration, as read.
    pub config: ConfigSnapshot,
    /// The installed copies that configuration was resolved against.
    pub catalog: Catalog,
    /// The release validation and sorting were performed against.
    pub target_version: GameVersion,
    /// The path-aware problems the read produced, which the caller still has to
    /// show rather than drop.
    pub diagnostics: Vec<ScanDiagnostic>,
}

/// Something the caller has to do, in the order the workflow decided it.
///
/// The effects of one reduce call are returned as an ordered list, and every
/// effect that has to be carried out carries the [`OperationId`] the completion
/// event must echo back. `Save` carries the whole `commit_save` argument list
/// because the effect must stay bound to the exact baseline its bytes were
/// prepared against, whoever ends up performing it and whenever.
#[derive(Debug)]
pub enum WorkflowEffect {
    /// Ask whether the pending edits should be saved before launching. Only
    /// `Save` and `Cancel` are answers to this question.
    PromptUnsavedLaunch {
        prompt: PromptId,
        operation: OperationId,
    },
    /// Ask whether the pending edits should be saved or discarded before closing.
    PromptUnsavedClose {
        prompt: PromptId,
        operation: OperationId,
    },
    /// Ask whether the pending edits should be saved or discarded before the
    /// installation is re-read.
    PromptUnsavedRefresh {
        prompt: PromptId,
        operation: OperationId,
    },
    /// Show these findings about the working list and ask whether to continue.
    /// Every asking of this question is a separate prompt identity.
    PromptWarnings {
        prompt: PromptId,
        operation: OperationId,
        issues: Vec<Issue>,
    },
    /// Replace the configuration at `path` with `prepared`, which was validated
    /// against `baseline`.
    ///
    /// The prepared bytes are boxed because they are the only part of an effect
    /// that is bigger than a pointer, and most effects are nothing but one.
    Save {
        operation: OperationId,
        path: PathBuf,
        baseline: ConfigSnapshot,
        prepared: Box<PreparedSave>,
    },
    /// Start this command, built from the settings this installation was loaded
    /// with.
    Launch {
        operation: OperationId,
        command: LaunchCommand,
    },
    /// Re-read the installation this request named. The target travels with the
    /// effect, so the caller reads the candidate that was asked for and nothing
    /// the settings form happens to say later.
    Refresh {
        operation: OperationId,
        target: RefreshTarget,
    },
    /// This refresh's own result was validated against the captured `target` and is
    /// current: that installation may now be replaced atomically by this complete
    /// replacement, and by nothing else.
    ///
    /// The replacement is boxed because it is the one effect payload bigger than a
    /// pointer - a whole catalog travels with it - and every other effect is
    /// nothing but identities.
    InstallRefresh {
        operation: OperationId,
        target: RefreshTarget,
        replacement: Box<RefreshReplacement>,
    },
    /// Close the application. Nothing follows it, and it is never emitted when a
    /// save failed.
    Close,
    /// Tell the user that `operation` did not happen, and why. An operational
    /// failure is reported even when warning confirmation is switched off.
    ReportFailure {
        operation: OperationKind,
        message: String,
    },
}

/// The kind of work an operation does, which is what a report is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationKind {
    /// Writing the configuration.
    Save,
    /// Starting the game.
    Launch,
    /// Re-reading the installation.
    Refresh,
    /// Closing the application.
    Close,
}

impl OperationKind {
    /// The word a diagnostic uses for this kind of work.
    #[must_use]
    pub const fn word(self) -> &'static str {
        match self {
            OperationKind::Save => "save",
            OperationKind::Launch => "launch",
            OperationKind::Refresh => "refresh",
            OperationKind::Close => "close",
        }
    }
}

/// One operation the workflow owns, from its request to its last effect.
///
/// Ids come from a process-wide sequence, so they identify exactly one operation
/// of exactly one workflow: a completion delivered to the wrong workflow is
/// recognizable as foreign even when both were each workflow's first operation and
/// everything about the two installations otherwise matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct OperationId(u64);

/// One prompt this workflow issued.
///
/// A prompt, not an operation, is what an answer belongs to: a warning question
/// asked at one revision is superseded the moment the list changes, and the
/// duplicate click of an old dialog must not authorize or cancel the newer one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PromptId(u64);

/// Why an operation did not complete, in the words the user should see.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationFailure {
    pub message: String,
}

impl OperationFailure {
    /// Wraps a message a caller produced.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Wraps the error that stopped the work, keeping its own text.
    #[must_use]
    pub fn from_error(error: &Error) -> Self {
        Self {
            message: error.to_string(),
        }
    }
}

/// One step of an operation. Every step is a kind of work the workflow can also
/// be asked to do on its own, so the two are one vocabulary, not two.
type Step = OperationKind;

/// The save/launch/close/refresh state machine for one loaded installation.
///
/// Build one with the settings that installed the session and the platform the
/// game runs on, then reduce every user request and every completion through it.
/// It is single-threaded by construction: it holds the one operation that is
/// running, the one question outstanding, and the warnings confirmed for it.
#[derive(Debug)]
pub struct Workflow {
    /// The settings this installation was loaded with, captured so no later form
    /// edit can reach into an operation already under way. Only the launch setup
    /// is frozen here; the warning preference belongs to each operation.
    installation: Settings,
    platform: Platform,
    /// The one directory relative launch paths are resolved against, captured with
    /// the installation.
    basis: PathBuf,
    /// The loaded session this workflow serves, from the first event it was given.
    binding: Option<u64>,
    /// The operation the workflow owns, from request to last effect.
    pending: Option<Pending>,
    /// The question whose answer has not arrived yet.
    awaiting: Option<Awaiting>,
    /// The findings the user accepted, tied to the operation and the revision
    /// they were shown at.
    acknowledged: Option<Acknowledgement>,
}

/// The operation under way: what was asked for, what is left to run and what is
/// waiting for a result.
#[derive(Debug)]
struct Pending {
    operation: OperationId,
    /// The user request this operation answers, which is what a refusal is about.
    request: OperationKind,
    /// The session this operation was started for. Every later event is checked
    /// against it, so one operation can never combine this installation's path
    /// with another session's baseline, catalog or edits.
    session: u64,
    /// The configuration file the loaded installation reads and writes, captured
    /// when the request arrived so a save or a re-read can only ever be about it.
    config_path: PathBuf,
    /// The warning preference this operation started with. An operation in flight
    /// keeps it; only the next operation captures what the user now has.
    confirmation: bool,
    /// The candidate a refresh reads and replaces, when this operation was asked
    /// to re-read. Nothing else about the operation depends on it.
    refresh: Option<RefreshTarget>,
    /// The steps left to run, in order.
    steps: Vec<Step>,
    /// The step whose completion is still expected, with the revision its effects
    /// were prepared at. A completion is only adopted when that revision is still
    /// the session's current one.
    running: Option<(Step, u64)>,
    /// The exact document the save now in flight writes. A completion must match
    /// it, because a file that merely reads alike is not the write this operation
    /// asked for.
    expected: Option<ConfigSnapshot>,
}

impl Pending {
    /// Whether this operation was started for `session`. The workflow's own binding
    /// already rejects a foreign session, and this is the same rule stated where the
    /// operation's data is used: an identity, a path, a baseline and an expected
    /// document may only ever be combined with the session they came from.
    fn belongs(&self, session: &Session) -> bool {
        self.session == session.generation()
    }
}

/// A question the workflow asked, with the operation and revision it was asked
/// at. An answer only counts for the question that is outstanding, and a
/// confirmation only holds while the revision it was shown at is current.
#[derive(Clone, Copy, Debug)]
struct Awaiting {
    prompt: PromptId,
    operation: OperationId,
    question: Question,
    revision: u64,
}

/// The content findings the user accepted for one operation at one revision.
#[derive(Clone, Copy, Debug)]
struct Acknowledgement {
    operation: OperationId,
    revision: u64,
}

/// The pending-edits question an operation asks before it touches anything, or
/// the content confirmation that precedes a write and a spawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Question {
    Unsaved(UnsavedQuestion),
    Warnings,
}

impl Question {
    /// The answers this question offers. Launching over unsaved edits means
    /// saving them: discarding is never an answer to the launch question, because
    /// nothing in this build starts the game while edits are pending.
    fn allows(self, answer: PromptAnswer) -> bool {
        match self {
            Self::Unsaved(UnsavedQuestion::Launch) => {
                matches!(answer, PromptAnswer::Save | PromptAnswer::Cancel)
            }
            Self::Unsaved(UnsavedQuestion::Close | UnsavedQuestion::Refresh) => matches!(
                answer,
                PromptAnswer::Save | PromptAnswer::Discard | PromptAnswer::Cancel
            ),
            Self::Warnings => matches!(answer, PromptAnswer::Confirm | PromptAnswer::Dismiss),
        }
    }

    /// What the question is about, in a refusal.
    fn word(self) -> &'static str {
        match self {
            Self::Unsaved(UnsavedQuestion::Launch) => "unsaved launch",
            Self::Unsaved(UnsavedQuestion::Close) => "unsaved close",
            Self::Unsaved(UnsavedQuestion::Refresh) => "unsaved refresh",
            Self::Warnings => "warning confirmation",
        }
    }

    /// The answers a user may choose, spelled the way the refusal names them.
    fn choices(self) -> &'static str {
        match self {
            Self::Unsaved(UnsavedQuestion::Launch) => "save or cancel",
            Self::Unsaved(_) => "save, discard or cancel",
            Self::Warnings => "confirm or dismiss",
        }
    }
}

/// The three unsaved-edits questions, one per operation that cannot simply run
/// over a list the user has not saved or discarded yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnsavedQuestion {
    Launch,
    Close,
    Refresh,
}

impl UnsavedQuestion {
    /// The effect that asks this question, as the prompt with this identity.
    fn prompt(self, prompt: PromptId, operation: OperationId) -> WorkflowEffect {
        match self {
            Self::Launch => WorkflowEffect::PromptUnsavedLaunch { prompt, operation },
            Self::Close => WorkflowEffect::PromptUnsavedClose { prompt, operation },
            Self::Refresh => WorkflowEffect::PromptUnsavedRefresh { prompt, operation },
        }
    }
}

/// The word a diagnostic uses for one answer.
fn answer_word(answer: PromptAnswer) -> &'static str {
    match answer {
        PromptAnswer::Save => "saving",
        PromptAnswer::Discard => "discarding",
        PromptAnswer::Cancel => "cancelling",
        PromptAnswer::Confirm => "confirming",
        PromptAnswer::Dismiss => "dismissing",
    }
}

/// The report a step produces when it did not complete.
fn reported(step: Step, message: String) -> WorkflowEffect {
    WorkflowEffect::ReportFailure {
        operation: step,
        message,
    }
}

/// The refusal for a request that arrived while another operation owns the
/// workflow: one write, one spawn and one re-read at a time.
fn refused_while_running(request: OperationKind, running: OperationKind) -> WorkflowEffect {
    WorkflowEffect::ReportFailure {
        operation: request,
        message: format!(
            "the {} cannot start while the {} is still running",
            request.word(),
            running.word()
        ),
    }
}

impl Workflow {
    /// The next operation or prompt identity.
    fn take_id() -> u64 {
        take_identity()
    }

    /// Prepares the workflow for the installation `settings` describes, read from
    /// `basis`, on `platform`.
    #[must_use]
    pub fn new(settings: &Settings, platform: Platform, basis: &Path) -> Self {
        Self {
            installation: settings.clone(),
            platform,
            basis: basis.to_path_buf(),
            binding: None,
            pending: None,
            awaiting: None,
            acknowledged: None,
        }
    }

    /// Decides what `event` means, given the session it applies to, and returns
    /// the effects the caller must carry out, in order.
    ///
    /// Three things must all agree before anything happens: the session has to be
    /// the one this workflow was started with, the operation or prompt the event
    /// names has to be the one in flight, and the revision the in-flight step was
    /// prepared at has to still be current. An event that fails any of them is
    /// dropped without touching anything, because a late or foreign result must not
    /// discard edits, adopt another installation's bytes or repeat an effect. A
    /// refusal the workflow produces itself is always reported, so a user is never
    /// left looking at a button that appeared to do nothing.
    pub fn reduce(&mut self, session: &mut Session, event: WorkflowEvent) -> Vec<WorkflowEffect> {
        let mut effects = Vec::new();
        // One workflow serves one loaded session. Binding happens at the first
        // event, and a session built from a different load is then foreign here,
        // whatever its bytes, revision or paths coincide with.
        match self.binding {
            None => self.binding = Some(session.generation()),
            Some(bound) if bound != session.generation() => return effects,
            Some(_) => {}
        }

        match event {
            WorkflowEvent::SaveRequested { confirm_warnings } => {
                self.request(
                    session,
                    OperationKind::Save,
                    confirm_warnings,
                    None,
                    &mut effects,
                );
            }
            WorkflowEvent::LaunchRequested { confirm_warnings } => {
                self.request(
                    session,
                    OperationKind::Launch,
                    confirm_warnings,
                    None,
                    &mut effects,
                );
            }
            WorkflowEvent::CloseRequested { confirm_warnings } => {
                self.request(
                    session,
                    OperationKind::Close,
                    confirm_warnings,
                    None,
                    &mut effects,
                );
            }
            WorkflowEvent::RefreshRequested {
                target,
                confirm_warnings,
            } => {
                // A refresh is bound to the candidate it was asked for, and its save
                // step is governed by the preference the user holds now, like any
                // other operation's.
                if let Some(reason) = target.refusal() {
                    effects.push(reported(
                        OperationKind::Refresh,
                        format!("the refresh target is not usable, so nothing was read: {reason}"),
                    ));
                    return effects;
                }
                self.request(
                    session,
                    OperationKind::Refresh,
                    confirm_warnings,
                    Some(target),
                    &mut effects,
                );
            }
            WorkflowEvent::PromptAnswered { prompt, answer } => {
                self.answer(session, prompt, answer, &mut effects);
            }
            WorkflowEvent::SaveFinished { operation, outcome } => {
                self.save_finished(session, operation, outcome, &mut effects);
            }
            WorkflowEvent::LaunchFinished { operation, outcome } => {
                self.launch_finished(session, operation, outcome, &mut effects);
            }
            WorkflowEvent::RefreshFinished { operation, outcome } => {
                self.refresh_finished(session, operation, outcome, &mut effects);
            }
        }
        effects
    }

    /// Starts the operation `request` asks for, on the session it was given, with
    /// the warning preference that was current when the user asked.
    ///
    /// `refresh` names the candidate a re-read operates on. Pending edits are not
    /// resolved here: `advance` asks that question at the moment the step that
    /// cannot run over them is about to run, which is what keeps an edit made while
    /// some other question was open from slipping through.
    fn request(
        &mut self,
        session: &Session,
        request: Step,
        confirmation: bool,
        refresh: Option<RefreshTarget>,
        effects: &mut Vec<WorkflowEffect>,
    ) {
        if let Some(pending) = &self.pending {
            effects.push(refused_while_running(request, pending.request));
            return;
        }
        let operation = OperationId(Self::take_id());
        self.pending = Some(Pending {
            operation,
            request,
            session: session.generation(),
            config_path: session.config_path().to_path_buf(),
            confirmation,
            refresh,
            steps: vec![request],
            running: None,
            expected: None,
        });
        // A confirmation given to an earlier operation never covers this one.
        self.acknowledged = None;
        self.advance(session, effects);
    }

    /// Applies the user's answer to the prompt that is outstanding.
    fn answer(
        &mut self,
        session: &mut Session,
        prompt: PromptId,
        answer: PromptAnswer,
        effects: &mut Vec<WorkflowEffect>,
    ) {
        let Some(outstanding) = self.awaiting.filter(|awaiting| awaiting.prompt == prompt) else {
            // An answer for a prompt that was superseded, already answered or never
            // asked - a duplicate click, or a dialog left open over a newer
            // question - authorizes nothing and cancels nothing.
            return;
        };
        if !outstanding.question.allows(answer) {
            let request = self
                .pending
                .as_ref()
                .expect("an outstanding prompt always belongs to an operation")
                .request;
            effects.push(reported(
                request,
                format!(
                    "{} is not an answer to the {} prompt; it offers {}",
                    answer_word(answer),
                    outstanding.question.word(),
                    outstanding.question.choices()
                ),
            ));
            // The prompt stays outstanding: a wrong answer is not a cancellation.
            return;
        }
        self.awaiting = None;

        match answer {
            PromptAnswer::Cancel | PromptAnswer::Dismiss => {
                // Cancelling an unsaved-edits prompt and dismissing the warnings
                // both end the whole operation: no effect, and no report either,
                // because nothing failed.
                self.finish_operation();
            }
            PromptAnswer::Confirm => {
                self.acknowledged = Some(Acknowledgement {
                    operation: outstanding.operation,
                    // The revision the findings were shown at, not the revision the
                    // answer arrives at: an edit made in between means what the user
                    // was shown is no longer what would take effect.
                    revision: outstanding.revision,
                });
                self.advance(session, effects);
            }
            PromptAnswer::Save | PromptAnswer::Discard => {
                let follow_up = self
                    .pending
                    .as_ref()
                    .expect("an outstanding prompt always belongs to an operation")
                    .request;
                let steps = match answer {
                    PromptAnswer::Save => vec![OperationKind::Save, follow_up],
                    _ => {
                        session.reset();
                        vec![follow_up]
                    }
                };
                self.run_steps(steps);
                self.advance(session, effects);
            }
        }
    }

    /// Adopts the result of the save step.
    fn save_finished(
        &mut self,
        session: &mut Session,
        operation: OperationId,
        outcome: SaveOutcome,
        effects: &mut Vec<WorkflowEffect>,
    ) {
        let Some((revision, path, expected)) = self.owns_save(session, operation) else {
            return;
        };
        match outcome {
            SaveOutcome::Failed(failure) => {
                // Nothing was replaced, so nothing is clean: the edits stay, and
                // the launch or close this save was gated behind never happens.
                self.finish_operation();
                effects.push(reported(OperationKind::Save, failure.message));
            }
            SaveOutcome::Committed(committed) => {
                if revision != session.revision() {
                    self.finish_operation();
                    effects.push(reported(
                        OperationKind::Save,
                        unadopted(&path, "the list changed while this save was running"),
                    ));
                    return;
                }
                // The result has to be the document this operation prepared, byte
                // for byte. A valid configuration whose active list happens to match
                // - a different release wrote it, other fields differ - is not this
                // write, and adopting it would make unrelated bytes the baseline the
                // next save is prepared against.
                if expected.as_ref() != Some(&committed) {
                    self.finish_operation();
                    effects.push(reported(
                        OperationKind::Save,
                        unadopted(
                            &path,
                            "the result is not the document this operation prepared",
                        ),
                    ));
                    return;
                }
                match session.acknowledge_save(&committed) {
                    Ok(()) => {
                        self.end_step();
                        self.advance(session, effects);
                    }
                    Err(error) => {
                        self.finish_operation();
                        effects.push(reported(
                            OperationKind::Save,
                            OperationFailure::from_error(&error).message,
                        ));
                    }
                }
            }
        }
    }

    /// Adopts the result of the launch step.
    fn launch_finished(
        &mut self,
        session: &Session,
        operation: OperationId,
        outcome: LaunchOutcome,
        effects: &mut Vec<WorkflowEffect>,
    ) {
        if self
            .owns(session, operation, OperationKind::Launch)
            .is_none()
        {
            return;
        }
        match outcome {
            // The process exists or it does not, and no session state depends on
            // which, so an accepted spawn just releases the operation.
            LaunchOutcome::Spawned { .. } => {
                self.end_step();
                self.advance(session, effects);
            }
            LaunchOutcome::Failed(failure) => {
                // A spawn the operating system refused says nothing about the list:
                // a save that already landed stays landed. The failure is reported
                // and the operation ends.
                self.finish_operation();
                effects.push(reported(OperationKind::Launch, failure.message));
            }
        }
    }

    /// Adopts the result of the refresh step, which is the only point at which a
    /// re-read may replace the loaded installation.
    fn refresh_finished(
        &mut self,
        session: &Session,
        operation: OperationId,
        outcome: RefreshOutcome,
        effects: &mut Vec<WorkflowEffect>,
    ) {
        let Some(revision) = self.owns(session, operation, OperationKind::Refresh) else {
            return;
        };
        let Some(target) = self.refresh_target() else {
            return;
        };
        match outcome {
            // The session stays exactly as it was: a failed re-read redirects
            // nothing, and a save that this operation really performed before it
            // stays saved.
            RefreshOutcome::Failed(failure) => {
                self.finish_operation();
                effects.push(reported(OperationKind::Refresh, failure.message));
            }
            RefreshOutcome::Reloaded(replacement) => {
                if revision != session.revision() {
                    self.finish_operation();
                    effects.push(reported(
                        OperationKind::Refresh,
                        "the list changed while the refresh was running, so the re-read was not \
                         installed"
                            .to_string(),
                    ));
                    return;
                }
                // An operation id says this result was handed to this operation. It
                // does not say the result is about the installation the operation was
                // asked to read, so the correspondence is checked against the captured
                // candidate: its origin, the release it was validated against, and the
                // internal consistency of the configuration it brought back. A result
                // that fails any of these installs nothing and changes nothing, and the
                // old installation stays current.
                if let Some(reason) = rejected(&target, &replacement) {
                    self.finish_operation();
                    effects.push(reported(OperationKind::Refresh, reason));
                    return;
                }
                self.finish_operation();
                effects.push(WorkflowEffect::InstallRefresh {
                    operation,
                    target,
                    replacement,
                });
            }
        }
    }

    /// Runs the step that may run now, appending the effect it needs.
    ///
    /// At most one step is dispatched per call: a step that writes, spawns or
    /// re-reads has to report back before the next one is even considered, and an
    /// unanswered prompt stops the operation until it is answered. Both gates are
    /// re-evaluated on every entry, which is why an edit made while another prompt
    /// was open cannot take an operation past a precondition it no longer meets.
    fn advance(&mut self, session: &Session, effects: &mut Vec<WorkflowEffect>) {
        if self.awaiting.is_some() || self.running_step().is_some() {
            return;
        }
        let (Some(operation), Some(path)) = (self.operation_of_pending(), self.config_path_of())
        else {
            return;
        };
        let Some(step) = self.head_step() else {
            // Every step has run, so the operation is over.
            self.finish_operation();
            return;
        };

        if session.is_dirty()
            && let Some(unsaved) = UnsavedQuestion::for_step(step)
        {
            // The unsaved gate: launching, closing or re-reading never happens over
            // pending edits until the user has chosen for them, at this revision. A
            // warning acknowledgement is not that choice.
            let prompt = PromptId(Self::take_id());
            self.awaiting = Some(Awaiting {
                prompt,
                operation,
                question: Question::Unsaved(unsaved),
                revision: session.revision(),
            });
            effects.push(unsaved.prompt(prompt, operation));
            return;
        }

        if let Some(issues) = self.unconfirmed(step, session) {
            let prompt = PromptId(Self::take_id());
            self.awaiting = Some(Awaiting {
                prompt,
                operation,
                question: Question::Warnings,
                revision: session.revision(),
            });
            effects.push(WorkflowEffect::PromptWarnings {
                prompt,
                operation,
                issues,
            });
            return;
        }

        match step {
            OperationKind::Save => match self.prepared(session, operation, &path) {
                Ok((effect, expected)) => {
                    self.dispatch(step, session.revision(), Some(expected));
                    effects.push(effect);
                }
                // A list this build cannot encode is a failure of the save, not a
                // warning to confirm: nothing is written and the edits stay.
                Err(error) => {
                    self.finish_operation();
                    effects.push(reported(step, OperationFailure::from_error(&error).message));
                }
            },
            OperationKind::Launch => {
                match build_launch(&self.installation, self.platform, &self.basis) {
                    Ok(command) => {
                        self.dispatch(step, session.revision(), None);
                        effects.push(WorkflowEffect::Launch { operation, command });
                    }
                    Err(error) => {
                        self.finish_operation();
                        effects.push(reported(step, OperationFailure::from_error(&error).message));
                    }
                }
            }
            OperationKind::Refresh => {
                let Some(target) = self.refresh_target() else {
                    return;
                };
                self.dispatch(step, session.revision(), None);
                effects.push(WorkflowEffect::Refresh { operation, target });
            }
            // Closing waits for no result: the save it was gated behind already
            // reported, and a failure there means this step is never reached.
            OperationKind::Close => {
                self.finish_operation();
                effects.push(WorkflowEffect::Close);
            }
        }
    }

    /// The bytes to write and the exact document they form, or the reason this list
    /// cannot be saved at all.
    ///
    /// Preparation happens here, before any effect leaves the reducer, so an
    /// unencodable list is reported instead of half-written. The baseline travels
    /// with the effect because `commit_save` refuses bytes prepared against a
    /// different load, and the expected document stays with the operation so the
    /// completion can be recognized rather than merely looked plausible.
    fn prepared(
        &self,
        session: &Session,
        operation: OperationId,
        path: &Path,
    ) -> Result<(WorkflowEffect, ConfigSnapshot), Error> {
        let prepared = prepare_save(
            session.loaded_config(),
            session.working_entries(),
            session.catalog(),
        )?;
        let expected = prepared.expected_committed().clone();
        Ok((
            WorkflowEffect::Save {
                operation,
                path: path.to_path_buf(),
                baseline: session.loaded_config().clone(),
                prepared: Box::new(prepared),
            },
            expected,
        ))
    }

    /// The findings that have not been confirmed for the step about to run.
    ///
    /// Confirmation guards the steps that put content into effect: writing the list
    /// and starting the game. A re-read and a close write nothing, so they ask only
    /// about pending edits. The preference is the one this operation started with,
    /// so a toggle the user makes mid-operation cannot interrupt it, and the next
    /// operation captures what the user has now. Turning confirmation off removes
    /// the question, never the findings or the failure reports, and an `Error`
    /// severity finding is confirmed like any other instead of being treated as a
    /// reason to refuse the user's own list.
    fn unconfirmed(&self, step: Step, session: &Session) -> Option<Vec<Issue>> {
        if !matches!(step, OperationKind::Save | OperationKind::Launch) {
            return None;
        }
        if !self
            .pending
            .as_ref()
            .is_some_and(|pending| pending.confirmation)
        {
            return None;
        }
        if let (Some(acknowledged), Some(pending)) = (&self.acknowledged, &self.pending)
            && acknowledged.operation == pending.operation
            && acknowledged.revision == session.revision()
        {
            return None;
        }
        let issues = session.validate();
        (!issues.is_empty()).then_some(issues)
    }

    /// Replaces the steps left to run.
    fn run_steps(&mut self, steps: Vec<Step>) {
        if let Some(pending) = &mut self.pending {
            pending.steps = steps;
        }
    }

    /// Moves the head step into flight, recording the revision its effects were
    /// prepared at - the check a later completion is held to - and, for a write, the
    /// exact document it must return.
    fn dispatch(&mut self, step: Step, revision: u64, expected: Option<ConfigSnapshot>) {
        if let Some(pending) = &mut self.pending {
            pending.steps.remove(0);
            pending.running = Some((step, revision));
            pending.expected = expected;
        }
    }

    /// Clears the in-flight step so the operation may continue.
    fn end_step(&mut self) {
        if let Some(pending) = &mut self.pending {
            pending.running = None;
            pending.expected = None;
        }
    }

    /// Lets go of the operation entirely, along with its prompt and its
    /// acknowledgement. The session keeps whatever edits it holds.
    fn finish_operation(&mut self) {
        self.pending = None;
        self.awaiting = None;
        self.acknowledged = None;
    }

    /// The step whose completion is still expected, if any.
    fn running_step(&self) -> Option<Step> {
        self.pending
            .as_ref()
            .and_then(|pending| pending.running)
            .map(|(step, _)| step)
    }

    /// The next step to run, if the workflow owns an operation with steps left.
    fn head_step(&self) -> Option<Step> {
        self.pending
            .as_ref()
            .and_then(|pending| pending.steps.first())
            .copied()
    }

    /// The operation the workflow owns.
    fn operation_of_pending(&self) -> Option<OperationId> {
        Some(self.pending.as_ref()?.operation)
    }

    /// The file the bound installation reads and writes.
    fn config_path_of(&self) -> Option<PathBuf> {
        Some(self.pending.as_ref()?.config_path.clone())
    }

    /// The candidate this operation was asked to re-read.
    fn refresh_target(&self) -> Option<RefreshTarget> {
        self.pending.as_ref()?.refresh.clone()
    }

    /// The revision the named operation dispatched `step` at, when that step is the
    /// one in flight, the operation belongs to this session, and it is the operation
    /// this workflow owns. Anything else - another workflow's identity, another
    /// session, another step, or a completion after the operation ended - owns
    /// nothing here, so its result is dropped.
    fn owns(&self, session: &Session, operation: OperationId, step: Step) -> Option<u64> {
        let pending = self.pending.as_ref()?;
        if pending.operation != operation || !pending.belongs(session) {
            return None;
        }
        match pending.running {
            Some((running, revision)) if running == step => Some(revision),
            _ => None,
        }
    }

    /// The same check for a save, which additionally carries the exact document its
    /// completion must match.
    fn owns_save(
        &self,
        session: &Session,
        operation: OperationId,
    ) -> Option<(u64, PathBuf, Option<ConfigSnapshot>)> {
        let pending = self.pending.as_ref()?;
        let revision = self.owns(session, operation, OperationKind::Save)?;
        Some((
            revision,
            pending.config_path.clone(),
            pending.expected.clone(),
        ))
    }
}

/// Which step cannot run over pending edits at all.
impl UnsavedQuestion {
    /// The question this step cannot run without an answer to, if it has one.
    ///
    /// Saving needs no such question: it is what the user chose. Launching, closing
    /// and re-reading each resolve pending edits first.
    fn for_step(step: Step) -> Option<Self> {
        match step {
            OperationKind::Save => None,
            OperationKind::Launch => Some(Self::Launch),
            OperationKind::Close => Some(Self::Close),
            OperationKind::Refresh => Some(Self::Refresh),
        }
    }
}

/// Why this replacement may not be installed on that candidate, if it may not.
///
/// The producer of a refresh result is the scan and read that was given this very
/// candidate, and nothing here can re-enact that read to prove it happened - this
/// boundary does not re-scan, re-read or re-open anything. What it can and does
/// require is that the result says it belongs to the captured candidate, names the
/// release that candidate is validated against, and is internally consistent: a
/// result that fails those tests cannot have come from reading this candidate, and a
/// result that passes them is a result the producer built for this input. The
/// producer staying honest about where its catalog came from is the trusted boundary
/// of this design, and it is stated rather than pretended otherwise.
fn rejected(target: &RefreshTarget, replacement: &RefreshReplacement) -> Option<String> {
    if replacement.origin != *target {
        return Some(
            "the refresh result is not about the candidate this operation asked for".to_string(),
        );
    }
    if target.target_version() != Some(&replacement.target_version) {
        return Some("the refresh result names a different release than this target".to_string());
    }
    let consistent = match parse_config(&replacement.config.raw) {
        Ok(reloaded) => reloaded == replacement.config,
        Err(_) => false,
    };
    if !consistent {
        return Some("the refresh result is not the load of its own bytes".to_string());
    }
    None
}

/// What a save that will not be adopted says about the disk.
///
/// The write happened. Nothing in this build un-writes it, so the report states
/// what is on disk and what is still pending instead of describing a completed
/// filesystem operation as cancelled.
fn unadopted(path: &Path, reason: &str) -> String {
    format!(
        "{reason}, so it was not adopted: {} keeps the bytes that were written and the current \
         edits are still unsaved",
        path.display()
    )
}
