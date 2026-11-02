//! Read-only discovery of the mod folders a settings record points at.
//!
//! The scanner turns configured roots into owned [`ModMetadata`] rows plus
//! path-aware [`ScanDiagnostic`]s. It reads and never writes, so unplugged
//! drives, a missing optional Workshop folder and a half-installed mod stay
//! visible as records instead of being repaired behind the user's back. The
//! game's `Config` folder and the rewrite's settings file are never consulted:
//! a scan needs roots, a target release and nothing else.
//!
//! Nothing here builds a catalog: identity, source and per-folder metadata are
//! returned as data, and package-id duplication, source preference between
//! copies and natural-resolution policy stay with the catalog builder.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fmt::{self};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::domain::metadata::parse_about;
use crate::domain::types::{
    DiagnosticKind, DiagnosticSeverity, Error, GameVersion, MetadataDiagnostic, ModId, ModMetadata,
    ParsedMetadata, Source,
};
use crate::integrations::settings::Settings;

/// Folder holding a mod's `About.xml` and `PublishedFileId.txt`.
const ABOUT_FOLDER: &str = "About";

/// Metadata document inside [`ABOUT_FOLDER`]; a non-standard spelling of this
/// path is reported as a missing document rather than guessed at.
const ABOUT_FILE: &str = "About.xml";

/// Marker a SteamCMD download carries, naming the folder it was unpacked into.
const PUBLISHED_FILE_ID_FILE: &str = "PublishedFileId.txt";

/// Git checkout marker, either a folder or a worktree pointer file. A root's
/// own `.git` is repository metadata, never a mod candidate.
const GIT_MARKER: &str = ".git";

/// Subfolder of the content root holding official content.
const DATA_FOLDER: &str = "Data";

/// The setting a missing or unusable [`DATA_FOLDER`] is reported against.
const OFFICIAL_ROOT_FIELD: &str = "content_root/Data";

/// One problem outside a mod record: a root or a directory entry that cannot be
/// turned into a mod at all.
///
/// `path` and `operation` say which filesystem step produced the problem and
/// `message` carries the actual cause, so a UI can show the failure without
/// re-reading the filesystem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScanDiagnostic {
    pub severity: DiagnosticSeverity,
    pub path: PathBuf,
    pub operation: String,
    pub message: String,
}

impl ScanDiagnostic {
    #[must_use]
    pub fn new(
        severity: DiagnosticSeverity,
        path: impl Into<PathBuf>,
        operation: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity,
            path: path.into(),
            operation: operation.into(),
            message: message.into(),
        }
    }

    /// A problem the scan worked around; the result is still usable.
    #[must_use]
    pub fn warning(
        path: impl Into<PathBuf>,
        operation: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::new(DiagnosticSeverity::Warning, path, operation, message)
    }

    /// A path that could not be turned into a mod record at all.
    #[must_use]
    pub fn error(
        path: impl Into<PathBuf>,
        operation: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::new(DiagnosticSeverity::Error, path, operation, message)
    }
}

impl fmt::Display for ScanDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} while {} on {}",
            self.message,
            self.operation,
            self.path.display()
        )
    }
}

/// What one scan found: owned mod rows and the path-aware problems that are not
/// representable as a mod.
#[derive(Clone, Debug)]
pub struct ScanResult {
    /// One row per canonical mod directory, ordered by [`ModId`].
    pub mods: Vec<ModMetadata>,
    pub diagnostics: Vec<ScanDiagnostic>,
}

/// Discovers the installed mod folders addressed by `settings`.
///
/// `game_root`, `content_root`, `local_mods_root` and the official
/// `content_root/Data` folder they imply are required, as is `target_version`;
/// anything missing, unreadable or not a directory is an error rather than an
/// empty scan, because a scan of nothing looks identical to an install with no
/// mods and would be usable as a replacement. `workshop_root` is optional: an
/// absent folder leaves the rest of the scan usable with a path-aware
/// diagnostic.
///
/// Official content comes from `content_root/Data`. Each root contributes only
/// its direct subdirectories, canonicalised and deduplicated so that a symlinked
/// alias of a folder or of a whole root collapses onto the one row it names and
/// is classified once. A folder whose `About.xml` cannot be read stays visible
/// as an invalid row; a path that cannot be resolved at all is reported as a
/// diagnostic, since there is no canonical identity to attach it to. Returned
/// diagnostics are ordered independently of how the filesystem enumerated them.
pub fn scan_install(settings: &Settings) -> Result<ScanResult, Error> {
    match scan_with(settings, &mut always_continue) {
        Ok(result) => Ok(result),
        Err(Stopped::Failed(error)) => Err(error),
        // `Infallible` holds no value and `always_continue` answers "keep going" every
        // time, so a scan nobody asked to stop cannot report a stop. The arm is exhausted
        // by the type rather than by inventing a filesystem failure that never happened.
        Err(Stopped::Cancelled(never)) => match never {},
    }
}

/// What stopped a scan that was allowed to be interrupted.
///
/// Only the job boundary that passes a cancellation check sees this: a caller that asks
/// a scan to stop gets the scan's own failure or nothing at all, never a partial list
/// dressed up as a result.
#[derive(Debug)]
pub(crate) enum ScanStop {
    /// The caller's check said stop before the whole installation was read.
    Cancelled,
    /// The scan failed on its own, in the words [`scan_install`] reports it in.
    Failed(Error),
}

/// [`scan_install`] with the cooperative stop boundary a background scan needs.
///
/// `cancelled` is asked before a root is opened, once per directory entry, and once per
/// mod folder whose metadata is about to be read, so a scan stops within one folder of
/// being asked to rather than only after reading the whole installation. Nothing else
/// changes: the same roots, the same canonicalisation, the same rows and the same
/// ordered diagnostics a plain scan returns. A read already issued to the filesystem
/// still runs to completion; that is the granularity of the boundary, not a gap in it.
pub(crate) fn scan_install_checked(
    settings: &Settings,
    cancelled: &dyn Fn() -> bool,
) -> Result<ScanResult, ScanStop> {
    match scan_with(settings, &mut || if cancelled() { Err(()) } else { Ok(()) }) {
        Ok(result) => Ok(result),
        Err(Stopped::Cancelled(())) => Err(ScanStop::Cancelled),
        Err(Stopped::Failed(error)) => Err(ScanStop::Failed(error)),
    }
}

/// Why one pass of the shared scan body stopped.
///
/// `C` is what the caller's check hands back when it says stop: [`std::convert::Infallible`]
/// for a scan that cannot be asked, which is what makes that caller's cancellation arm
/// exhaustive instead of a place a fake failure has to be written.
enum Stopped<C> {
    /// The caller asked this scan to stop, carrying the caller's own stop value through.
    Cancelled(C),
    /// A step that makes the scan unusable, in the words the scan has always used.
    Failed(Error),
}

/// The check a scan that cannot be stopped runs.
fn always_continue() -> Result<(), std::convert::Infallible> {
    Ok(())
}

/// Runs one boundary check, carrying the caller's stop value through unchanged.
fn stop_at<C>(cancelled: &mut dyn FnMut() -> Result<(), C>) -> Result<(), Stopped<C>> {
    match cancelled() {
        Ok(()) => Ok(()),
        Err(stop) => Err(Stopped::Cancelled(stop)),
    }
}

/// The whole read, over one cancellation boundary.
fn scan_with<C>(
    settings: &Settings,
    cancelled: &mut dyn FnMut() -> Result<(), C>,
) -> Result<ScanResult, Stopped<C>> {
    // The game root is validated up front even though the mod list is read from
    // the content root: a wrong install path is a settings mistake the user has
    // to see before a scan result is worth anything.
    required_root("game_root", settings.game_root.as_deref()).map_err(Stopped::Failed)?;
    let content_root =
        required_root("content_root", settings.content_root.as_deref()).map_err(Stopped::Failed)?;
    let local_root = required_root("local_mods_root", settings.local_mods_root.as_deref())
        .map_err(Stopped::Failed)?;
    let target_version = settings
        .target_version
        .as_ref()
        .ok_or_else(|| not_configured("target_version"))
        .map_err(Stopped::Failed)?;

    let mut discovery = Discovery::default();
    let workshop_root = optional_workshop_root(settings.workshop_root.as_deref(), &mut discovery)
        .map_err(Stopped::Failed)?;
    discovery.scan_official_root(&content_root, cancelled)?;
    discovery.scan_root(&local_root, RootKind::Local, cancelled)?;
    if let Some(workshop_root) = workshop_root {
        discovery.scan_root(&workshop_root, RootKind::Workshop, cancelled)?;
    }

    let mut mods = Vec::with_capacity(discovery.candidates.len());
    for (directory, source) in &discovery.candidates {
        stop_at(cancelled)?;
        mods.push(ModMetadata::new(
            ModId::new(directory.clone()),
            *source,
            read_metadata(directory, target_version),
        ));
    }

    // Entry failures surface while the filesystem enumerates a root, so the
    // whole diagnostic list is ordered here rather than left in whatever order
    // `read_dir` happened to return.
    let mut diagnostics = discovery.diagnostics;
    diagnostics.sort_by(diagnostic_order);

    Ok(ScanResult { mods, diagnostics })
}

/// Which root a mod directory was enumerated from: the root decides the source
/// unless a local folder's own markers say otherwise.
#[derive(Clone, Copy, Debug)]
enum RootKind {
    Official,
    Local,
    Workshop,
}

/// One scan's found-so-far state: canonical mod directories with the source
/// chosen for them, plus the path-aware problems met on the way.
#[derive(Default)]
struct Discovery {
    /// Keyed by canonical directory, so iteration yields rows in id order no
    /// matter how the filesystem enumerated them.
    candidates: BTreeMap<PathBuf, Source>,
    diagnostics: Vec<ScanDiagnostic>,
}

impl Discovery {
    /// Adds the mod folders directly inside a root. Official and Workshop
    /// content takes its source from the root, so their markers are never read.
    /// The root itself and every entry met in it is one cancellation boundary.
    fn scan_root<C>(
        &mut self,
        root: &Path,
        kind: RootKind,
        cancelled: &mut dyn FnMut() -> Result<(), C>,
    ) -> Result<(), Stopped<C>> {
        stop_at(cancelled)?;
        for directory in self.mod_directories(root, cancelled)? {
            if matches!(kind, RootKind::Local) && self.candidates.contains_key(&directory) {
                // The official root is scanned first and already owns this
                // canonical folder, so reading its local markers could only
                // produce a source that cannot win, plus diagnostics about a
                // classification that is never used.
                continue;
            }
            let source = match kind {
                RootKind::Official => Source::Official,
                RootKind::Workshop => Source::Workshop,
                RootKind::Local => self.classify_local(&directory),
            };
            self.add(directory, source);
        }
        Ok(())
    }

    /// Official content lives in `content_root/Data`, which is a required root:
    /// with no readable `Data` folder there are no official records to return,
    /// and a scan that quietly skipped it would look exactly like an
    /// installation with no official content. Nothing is created here, so a
    /// wrong content root stays a settings mistake the caller has to see.
    fn scan_official_root<C>(
        &mut self,
        content_root: &Path,
        cancelled: &mut dyn FnMut() -> Result<(), C>,
    ) -> Result<(), Stopped<C>> {
        let data_root = content_root.join(DATA_FOLDER);
        let data_root = directory_root(OFFICIAL_ROOT_FIELD, &data_root).map_err(Stopped::Failed)?;
        self.scan_root(&data_root, RootKind::Official, cancelled)
    }

    /// Direct subdirectories of `root`, canonicalised, sorted and deduplicated
    /// so an alias is classified once whatever number of links name it. Plain
    /// files and the `.git` metadata folder are not mod candidates; a symlink is
    /// resolved only far enough to name the directory it points at, never
    /// descended into. Each entry met is one cancellation boundary, so a walk through a
    /// large or slow folder can be stopped without losing the entries it already
    /// resolved.
    fn mod_directories<C>(
        &mut self,
        root: &Path,
        cancelled: &mut dyn FnMut() -> Result<(), C>,
    ) -> Result<Vec<PathBuf>, Stopped<C>> {
        let entries = fs::read_dir(root).map_err(|error| Stopped::Failed(io_error(root, error)))?;
        let mut directories = Vec::new();
        for entry in entries {
            stop_at(cancelled)?;
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    self.report(root, "enumerate root entries", &error);
                    continue;
                }
            };
            if entry.file_name() == OsStr::new(GIT_MARKER) {
                continue;
            }
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(error) => {
                    self.report(&path, "inspect mod candidate", &error);
                    continue;
                }
            };
            if file_type.is_file() {
                continue;
            }
            match path.canonicalize() {
                // A link to a file is not a mod either, and an alias of a mod
                // folder has already collapsed onto that folder here.
                Ok(canonical) => {
                    if canonical.is_dir() {
                        directories.push(canonical);
                    }
                }
                Err(error) => self.report(&path, "resolve mod directory", &error),
            }
        }
        directories.sort();
        directories.dedup();
        Ok(directories)
    }

    /// Records a mod directory once, keeping the highest-preference source when
    /// two roots or two links name the same folder.
    fn add(&mut self, directory: PathBuf, source: Source) {
        use std::collections::btree_map::Entry;
        match self.candidates.entry(directory) {
            Entry::Occupied(mut entry) => {
                if source_rank(source) < source_rank(*entry.get()) {
                    entry.insert(source);
                }
            }
            Entry::Vacant(entry) => {
                entry.insert(source);
            }
        }
    }

    /// Classifies one folder of a local mods root. A `PublishedFileId.txt`
    /// naming the folder is a SteamCMD download even when the folder also
    /// carries a `.git` marker, because SteamCMD has been observed to leave one
    /// behind; otherwise the folder's own `.git` marker wins, then a marker whose
    /// presence is confirmed, then a hand-managed local mod. Parent folders are
    /// never consulted, so an outer repository does not reclassify everything
    /// under it.
    ///
    /// A marker that cannot be read keeps the classification it proved: its id is
    /// unknown, so no matching id can be claimed, but its presence still says the
    /// folder was unpacked by a download tool. Only a marker whose presence
    /// cannot be confirmed at all is treated as absent.
    fn classify_local(&mut self, directory: &Path) -> Source {
        let marker_path = directory.join(ABOUT_FOLDER).join(PUBLISHED_FILE_ID_FILE);
        let folder_name = directory.file_name().unwrap_or(OsStr::new(""));
        let marker = read_published_file_id(&marker_path);
        match &marker {
            Marker::Unreadable(reason) | Marker::Unconfirmed(reason) => {
                self.report_message(&marker_path, "read PublishedFileId marker", reason.clone());
            }
            Marker::Absent | Marker::Readable(_) => {}
        }
        if matches!(&marker, Marker::Readable(id) if id.as_str() == folder_name) {
            // A marker naming the folder wins even when the folder also carries
            // a `.git` marker.
            return Source::SteamCmd;
        }
        if self.has_git_marker(directory) {
            return Source::Git;
        }
        if matches!(marker, Marker::Readable(_) | Marker::Unreadable(_)) {
            return Source::SteamCmd;
        }
        Source::Local
    }

    fn has_git_marker(&mut self, directory: &Path) -> bool {
        let marker = directory.join(GIT_MARKER);
        // Following the marker keeps a dangling `.git` link from being mistaken
        // for a checkout, while a folder and a worktree pointer file both count.
        match fs::metadata(&marker) {
            Ok(_) => true,
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => {
                self.report(&marker, "read .git marker", &error);
                false
            }
        }
    }

    /// Reports an entry-level failure with the path that produced it, so one
    /// unreadable child never removes its siblings from the result.
    fn report(&mut self, path: &Path, operation: &str, error: &io::Error) {
        self.report_message(path, operation, error.to_string());
    }

    /// Reports an entry-level failure whose cause is already spelled out, which
    /// is how a partially read marker keeps its actual reason.
    fn report_message(&mut self, path: &Path, operation: &str, message: String) {
        self.diagnostics
            .push(ScanDiagnostic::error(path, operation, message));
    }
}

/// Total order for the diagnostics a scan returns: path, then operation, then
/// severity, then message. Entry failures are met during an unsorted
/// enumeration, so ordering them at the boundary is what makes the whole result
/// independent of how the filesystem chose to list a folder.
fn diagnostic_order(left: &ScanDiagnostic, right: &ScanDiagnostic) -> Ordering {
    left.path
        .cmp(&right.path)
        .then_with(|| left.operation.cmp(&right.operation))
        .then_with(|| severity_rank(left.severity).cmp(&severity_rank(right.severity)))
        .then_with(|| left.message.cmp(&right.message))
}

/// Errors before warnings: where one path and operation produced both, the
/// failure should not sit behind a warning the reader has to scan past.
fn severity_rank(severity: DiagnosticSeverity) -> u8 {
    match severity {
        DiagnosticSeverity::Error => 0,
        DiagnosticSeverity::Warning => 1,
    }
}

/// Preference order for one canonical folder found under several roots:
/// official content, then anything derived from a local root, then Workshop.
fn source_rank(source: Source) -> u8 {
    match source {
        Source::Official => 0,
        Source::Local | Source::SteamCmd | Source::Git => 1,
        Source::Workshop => 2,
    }
}

/// A required root, canonicalised so aliases and overlapping roots collapse onto
/// the same mod directories.
fn required_root(field: &str, configured: Option<&Path>) -> Result<PathBuf, Error> {
    let path = configured.ok_or_else(|| not_configured(field))?;
    directory_root(field, path)
}

/// A path that has to exist and be a directory. The filesystem checks are
/// fallible on purpose: a path that cannot be resolved or read keeps its actual
/// `std::io` cause and the path it happened to, while something that really is
/// not a directory is the settings mistake the caller named.
fn directory_root(field: &str, path: &Path) -> Result<PathBuf, Error> {
    let canonical = path.canonicalize().map_err(|error| io_error(path, error))?;
    let metadata = fs::metadata(&canonical).map_err(|error| io_error(&canonical, error))?;
    if metadata.is_dir() {
        return Ok(canonical);
    }
    Err(not_a_directory(field, path, &canonical))
}

/// The Workshop root is optional: unset means nothing to scan and says so with
/// no diagnostic, an absent folder keeps the scan usable with one, and a
/// configured path that is not a directory is a mistake rather than an empty
/// Workshop.
fn optional_workshop_root(
    configured: Option<&Path>,
    discovery: &mut Discovery,
) -> Result<Option<PathBuf>, Error> {
    let Some(path) = configured else {
        return Ok(None);
    };
    match path.canonicalize() {
        Ok(canonical) if canonical.is_dir() => Ok(Some(canonical)),
        Ok(canonical) => Err(not_a_directory("workshop_root", path, &canonical)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            discovery.diagnostics.push(ScanDiagnostic::warning(
                path.to_path_buf(),
                "read workshop mod root",
                format!("the configured Workshop folder is not there: {error}"),
            ));
            Ok(None)
        }
        Err(error) => Err(io_error(path, error)),
    }
}

fn not_configured(field: &str) -> Error {
    Error::InvalidInput {
        context: format!("mod scan: {field}"),
        message: "the setting has no value, so there is nothing to scan".to_string(),
    }
}

fn not_a_directory(field: &str, path: &Path, canonical: &Path) -> Error {
    Error::InvalidInput {
        context: format!("mod scan: {field}"),
        message: format!(
            "{} is not a directory (it resolves to {})",
            path.display(),
            canonical.display()
        ),
    }
}

fn io_error(path: &Path, source: io::Error) -> Error {
    Error::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// What reading a local folder's `About/PublishedFileId.txt` established, with
/// presence kept apart from content.
///
/// A `NotFound` read is absence, including a link whose target is gone, matching
/// the follow-the-link `exists()` check the reference classification uses. Any
/// other read failure is confirmed by a second metadata check before it counts
/// as a marker that is merely unreadable, so a racing delete or a path under a
/// non-directory (an `About` that is a file) cannot invent a download.
enum Marker {
    /// Nothing is filed under that name.
    Absent,
    /// The marker is there and readable, holding this trimmed id.
    Readable(String),
    /// The marker is there but is not a readable PublishedFileId document;
    /// `reason` is the actual filesystem or decoding cause.
    Unreadable(String),
    /// A read failed and no follow-up check could confirm the marker, so neither
    /// its presence nor its id is known; `reason` carries both causes.
    Unconfirmed(String),
}

/// Reads `About/PublishedFileId.txt` without guessing at anything it did not
/// say. A marker that is present but unreadable keeps the failure visible as a
/// reason instead of becoming an invented id.
fn read_published_file_id(path: &Path) -> Marker {
    match fs::read(path) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => Marker::Readable(text.trim().to_string()),
            Err(error) => Marker::Unreadable(format!("PublishedFileId.txt is not text: {error}")),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => Marker::Absent,
        Err(error) => match fs::symlink_metadata(path) {
            Ok(_) => Marker::Unreadable(error.to_string()),
            Err(confirm) => Marker::Unconfirmed(format!(
                "{error}; the marker could not be confirmed: {confirm}"
            )),
        },
    }
}

/// Reads a mod folder's `About/About.xml`. A folder whose document is missing,
/// unreadable or unusable still becomes a record, carrying the reason as an
/// error-severity diagnostic.
fn read_metadata(directory: &Path, target_version: &GameVersion) -> ParsedMetadata {
    let path = directory.join(ABOUT_FOLDER).join(ABOUT_FILE);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return unreadable(format!("{} is missing", path.display()));
        }
        Err(error) => {
            return unreadable(format!("{} could not be read: {error}", path.display()));
        }
    };
    parse_about(&bytes, target_version).unwrap_or_else(|error| unreadable(error.to_string()))
}

fn unreadable(reason: String) -> ParsedMetadata {
    ParsedMetadata {
        diagnostics: vec![MetadataDiagnostic::new(
            DiagnosticKind::UnreadableAbout { reason },
            format!("{ABOUT_FOLDER}/{ABOUT_FILE}"),
        )],
        ..ParsedMetadata::default()
    }
}

#[cfg(test)]
mod checked_scan_tests {
    //! The cooperative boundary of [`super::scan_install_checked`], proved by counting the
    //! checks the walk itself makes. A jobs-level case can only ask for a stop before or
    //! after a read, which is not the same claim, so the stop-inside-the-walk evidence
    //! lives here, at the boundary that takes it.
    use super::{ScanStop, scan_install, scan_install_checked};
    use crate::domain::types::GameVersion;
    use crate::integrations::settings::Settings;
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tempfile::TempDir;

    /// An install with two official folders, two local folders and an absent Workshop
    /// root: enough boundaries that a stop lands in the middle of the walk rather than at
    /// its edges.
    fn install() -> (TempDir, Settings) {
        let temp = TempDir::new().expect("a temporary install");
        let game_root = temp.path().join("game");
        let content_root = game_root.join("Mods");
        let data_root = content_root.join("Data");
        let local_root = temp.path().join("Local Mods");
        for root in [&game_root, &data_root, &local_root] {
            fs::create_dir_all(root).expect("the install layout must be creatable");
        }
        for (root, name, package_id) in [
            (&data_root, "core", "base.one"),
            (&data_root, "royalty", "base.two"),
            (&local_root, "one", "first.mod"),
            (&local_root, "two", "second.mod"),
        ] {
            let about = root.join(name).join("About");
            fs::create_dir_all(&about).expect("a mod folder must be creatable");
            fs::write(
                about.join("About.xml"),
                format!(
                    "<ModMetaData><packageId>{package_id}</packageId><name>{name}</name>\
                     <supportedVersions><li>1.5</li></supportedVersions></ModMetaData>"
                ),
            )
            .expect("a mod's About.xml must be writable");
        }
        let settings = Settings {
            schema_version: 1,
            game_root: Some(game_root),
            content_root: Some(content_root),
            config_path: Some(temp.path().join("ModsConfig.xml")),
            local_mods_root: Some(local_root),
            workshop_root: Some(Path::new("/definitely/not/here").to_path_buf()),
            target_version: Some(GameVersion::parse("1.5.4171").expect("the target must parse")),
            executable_override: None,
            confirm_warnings: true,
        };
        (temp, settings)
    }

    /// A check that counts the times it was asked, and says stop from the `stop_after`th.
    struct Counter {
        asked: AtomicUsize,
        stop_after: usize,
    }

    impl Counter {
        fn never() -> Self {
            Self {
                asked: AtomicUsize::new(0),
                stop_after: usize::MAX,
            }
        }

        fn stopping_at(stop_after: usize) -> Self {
            Self {
                asked: AtomicUsize::new(0),
                stop_after,
            }
        }

        fn asked(&self) -> usize {
            self.asked.load(Ordering::Relaxed)
        }

        fn check(&self) -> bool {
            let count = self.asked.fetch_add(1, Ordering::Relaxed) + 1;
            count >= self.stop_after
        }
    }

    #[test]
    fn the_walk_asks_its_boundary_check_at_every_root_folder_and_candidate() {
        let (_temp, settings) = install();
        let counter = Counter::never();

        let checked = scan_install_checked(&settings, &|| counter.check())
            .expect("a check that never says stop must read the whole installation");
        let public = scan_install(&settings).expect("the same install must scan unchanged");

        // Two roots opened, four entries met, four folders read: ten boundaries, and a
        // count this high is only reachable by asking inside the walk.
        assert!(
            counter.asked() >= 10,
            "the walk must ask at each root, entry and candidate, asked {} times",
            counter.asked()
        );
        assert_eq!(checked.mods.len(), public.mods.len());
        assert_eq!(checked.diagnostics, public.diagnostics);
    }

    #[test]
    fn a_boundary_check_raised_mid_walk_stops_the_scan_at_that_boundary() {
        let (_temp, settings) = install();
        let counter = Counter::stopping_at(6);

        let stopped = scan_install_checked(&settings, &|| counter.check());

        assert!(
            matches!(stopped, Err(ScanStop::Cancelled)),
            "a scan asked to stop mid-walk must report the stop, not a result: {stopped:?}"
        );
        assert_eq!(
            counter.asked(),
            6,
            "the walk stopped at the boundary that said stop instead of reading to the end"
        );
    }

    #[test]
    fn a_check_that_never_says_stop_reads_exactly_the_unchanged_public_scan() {
        let (_temp, settings) = install();
        let counter = Counter::never();

        let checked = scan_install_checked(&settings, &|| counter.check()).expect("no stop asked");
        let public = scan_install(&settings).expect("the same install must scan");

        assert_eq!(checked.mods, public.mods, "the same rows in the same order");
        assert_eq!(
            checked.diagnostics, public.diagnostics,
            "and the same problems"
        );
    }
}
