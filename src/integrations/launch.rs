//! Starting the game: one platform command vector, then one process call.
//!
//! The boundary has two steps and no third one. [`build_launch`] turns one
//! settings record into the operating-system command that would start the game,
//! and [`spawn_launch`] hands that vector to the process API. Nothing here joins
//! a program and its arguments into a command line, so a configured path holding
//! spaces, quotes or non-ASCII text stays exactly one argument instead of
//! becoming shell syntax.
//!
//! A command is built for one [`Platform`] at a time, which is what lets a test
//! on any host ask what Windows, Linux or macOS would run. Two decisions do read
//! the filesystem, and both say so: the game root has to be the directory it
//! claims to be, and the default executable is the first candidate this platform
//! would use. A refusal names the setting or the candidate paths it looked at;
//! nothing is installed, downloaded, migrated or autodetected, and no real game
//! is ever started by this build's tests.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::domain::types::Error;
use crate::integrations::settings::Settings;

/// The 64-bit Windows game executable, the first Windows default.
const WINDOWS_64: &str = "RimWorldWin64.exe";
/// The 32-bit Windows game executable, tried when the 64-bit one is absent.
const WINDOWS_32: &str = "RimWorldWin.exe";
/// The 64-bit Linux game executable, the first Linux default.
const LINUX_64: &str = "RimWorldLinux64";
/// The Linux executable that predates the 64-bit name, tried second.
const LINUX_32: &str = "RimWorldLinux";
/// The macOS launcher this build asks to open the bundle. Every macOS install
/// has it at this path, and it is started as a program, not through a shell.
const MACOS_LAUNCHER: &str = "/usr/bin/open";
/// The extension that names a macOS application bundle directory.
const MACOS_BUNDLE: &str = "app";
/// The `open` flag that starts a new instance of the application even when one
/// is already running, so launching from the manager never wakes an old window.
const MACOS_NEW_INSTANCE: &str = "-n";

/// The operating system family a launch command is built for.
///
/// This is the target of the command, not the host: a test can build the macOS
/// command on Linux. Only [`Platform::current`] is about the running system.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    /// Windows: the executable lives inside the game root.
    Windows,
    /// macOS: the game root *is* the application bundle, and `open` starts it.
    MacOS,
    /// Linux: the executable lives inside the game root.
    Linux,
}

impl Platform {
    /// The platform this process runs on, or `None` on an operating system this
    /// build does not launch a game on.
    #[must_use]
    pub fn current() -> Option<Platform> {
        match std::env::consts::OS {
            "windows" => Some(Platform::Windows),
            "macos" => Some(Platform::MacOS),
            "linux" => Some(Platform::Linux),
            _ => None,
        }
    }

    /// How a diagnostic names this platform.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Platform::Windows => "the Windows launch",
            Platform::MacOS => "the macOS launch",
            Platform::Linux => "the Linux launch",
        }
    }
}

/// A program, its separate arguments and the directory to start it in.
///
/// Every field is data an operating system consumes as-is: `args` never holds
/// shell text, and `working_directory` is `None` where the start directory is
/// not this build's business. The fields are public because a caller may need to
/// show or hand over the vector it was given; [`build_launch`] is the only place
/// that decides what the game's vector is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchCommand {
    /// The executable to start, or the launcher program for a bundled start.
    pub program: PathBuf,
    /// The arguments, one element per argument, in order.
    pub args: Vec<OsString>,
    /// The directory the process starts in.
    pub working_directory: Option<PathBuf>,
}

/// Builds the command that starts the game described by `settings`.
///
/// `executable_override` always wins over a default, on every platform, and is
/// used verbatim: it is an explicit path, not a name to look up. Otherwise the
/// platform's own candidates are tried in order, and a launch that finds none is
/// a refusal naming what was tried rather than a guess. The game root is the
/// working directory on Windows and Linux; on macOS the bundle *is* the game
/// root (or the override names another bundle) and `content_root` is never
/// consulted, because it holds the game's data, not an application to open.
///
/// `basis` is the one directory every relative configured path is read from: the
/// manager's working directory, captured once where the application starts. A
/// relative game root or override is resolved against it before anything is
/// checked, so the file whose metadata was validated is the file that is named in
/// the command, and the command's program and working directory are both absolute.
/// Resolving a relative path against the child's own directory instead would look
/// at one file and start another.
///
/// Path *syntax* is decided by the host this runs on, not by the platform a command
/// is being built for: `Platform` selects which executables and which launcher are
/// used, while `std::path` decides what a configured string means. That is why the
/// Windows drive-relative rule below can only be exercised on a Windows host, and
/// why building a Windows command here from synthetic paths is evidence about
/// candidate order and argument vectors, not about Windows path parsing.
pub fn build_launch(
    settings: &Settings,
    platform: Platform,
    basis: &Path,
) -> Result<LaunchCommand, Error> {
    match platform {
        Platform::Windows => {
            executable_in_game_root(settings, platform, &[WINDOWS_64, WINDOWS_32], basis)
        }
        Platform::Linux => {
            executable_in_game_root(settings, platform, &[LINUX_64, LINUX_32], basis)
        }
        Platform::MacOS => open_bundle(settings, basis),
    }
}

/// Starts `command` and returns immediately.
///
/// This is the only process call: the game is spawned, never waited for, so a
/// caller on a UI thread is never blocked by it, and nothing here kills it. The
/// returned [`std::process::Child`] is the caller's own handle - the adapter
/// stores no process state, so a second call can only come from a caller that
/// decided to make one.
///
/// What dropping that handle does, and does not, do is standard-library behavior
/// and is stated here rather than assumed: `Child::drop` closes the handles it
/// owns and leaves the process running, so the game outlives the handle, while
/// the application job host retains direct children for nonblocking observation
/// and reaping. Bundle launchers are tracked separately and never prove game exit.
/// No claim is made about session or controlling-terminal detachment beyond that. Whether the operating system accepted the
/// start is the whole of what success means: it is not a claim that the game
/// loaded.
///
/// The child's standard handles are sent to the null device, so the game can
/// never hold open the output pipe of the process that started it; RimWorld
/// writes its own log file.
pub fn spawn_launch(command: &LaunchCommand) -> Result<std::process::Child, Error> {
    let mut builder = Command::new(&command.program);
    builder
        .args(&command.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(directory) = &command.working_directory {
        builder.current_dir(directory);
    }
    builder
        .spawn()
        .map_err(|source| failed(&command.program, source))
}

/// The Windows and Linux shape: an executable inside the game root, started from
/// the game root, with no arguments of any kind.
///
/// The defaults are chosen in the given order. A candidate only counts when it is
/// a file, and for the Linux defaults on a host that exposes Unix permission
/// modes, when some execute bit is set: asking a host without those modes is
/// asked here, because it has no mode to read.
fn executable_in_game_root(
    settings: &Settings,
    platform: Platform,
    defaults: &[&str],
    basis: &Path,
) -> Result<LaunchCommand, Error> {
    let context = platform.label();
    let game_root = settings
        .game_root
        .as_ref()
        .ok_or_else(|| invalid(context, "game_root is not configured".to_string()))
        .and_then(|root| resolved(root, basis, context))?;
    require_directory(&game_root, context, "game_root")?;

    let program = match &settings.executable_override {
        Some(explicit) => {
            let explicit = resolved(explicit, basis, context)?;
            require_file(&explicit, context, "executable_override")?;
            explicit
        }
        None => first_default(&game_root, defaults, platform, context)?,
    };

    Ok(LaunchCommand {
        program,
        args: Vec::new(),
        working_directory: Some(game_root),
    })
}

/// The macOS shape: `open -n <bundle>`, with the bundle as its own argument.
///
/// `content_root` is not part of this decision at all: it is the game's resource
/// directory, and opening it would start nothing. Neither is a bundle discovered
/// by looking around; the configured value is the configured value.
fn open_bundle(settings: &Settings, basis: &Path) -> Result<LaunchCommand, Error> {
    let context = Platform::MacOS.label();
    let (bundle, setting) = match (&settings.executable_override, &settings.game_root) {
        (Some(explicit), _) => (
            resolved(explicit, basis, context)?,
            "executable_override".to_string(),
        ),
        (None, Some(game_root)) => (
            resolved(game_root, basis, context)?,
            "game_root".to_string(),
        ),
        (None, None) => {
            return Err(invalid(
                context,
                "neither executable_override nor game_root names the application bundle to open"
                    .to_string(),
            ));
        }
    };
    require_directory(&bundle, context, &setting)?;
    require_bundle(&bundle, context, &setting)?;

    Ok(LaunchCommand {
        program: PathBuf::from(MACOS_LAUNCHER),
        args: vec![OsString::from(MACOS_NEW_INSTANCE), bundle.into_os_string()],
        working_directory: None,
    })
}

/// The first default executable under `game_root`, or a refusal listing them all.
fn first_default(
    game_root: &Path,
    defaults: &[&str],
    platform: Platform,
    context: &str,
) -> Result<PathBuf, Error> {
    // Permission modes belong to the platform being launched on, not the host
    // that is doing the asking, so they are only consulted for Linux defaults -
    // and only where the host has modes to read at all.
    let check_modes = platform == Platform::Linux;
    let mut tried = Vec::with_capacity(defaults.len());
    for name in defaults {
        let candidate = game_root.join(name);
        tried.push(candidate.display().to_string());
        let Ok(found) = candidate.metadata() else {
            continue;
        };
        if found.is_file() && (!check_modes || host_can_execute(&found)) {
            return Ok(candidate);
        }
    }
    Err(invalid(
        context,
        format!(
            "no game executable under {}; tried {}",
            game_root.display(),
            tried.join(", ")
        ),
    ))
}

/// Whether this host would let `found` run, from its permission bits.
#[cfg(unix)]
fn host_can_execute(found: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    found.permissions().mode() & 0o111 != 0
}

/// Whether this host would let `found` run.
#[cfg(not(unix))]
fn host_can_execute(_found: &fs::Metadata) -> bool {
    // A Windows host exposes no Unix mode bits, so presence is all that can be
    // said about a candidate there; a Linux host running these tests does check.
    true
}

/// The one directory relative configured paths are read from.
///
/// An absolute path is used exactly as configured; anything else belongs under
/// `basis`, because that is where the manager was standing when it read these
/// settings. The basis itself has to be absolute, or "relative to the basis" would
/// mean whatever directory the process happens to be in when the game starts. The
/// result is insisted on being absolute before any metadata is read or any command
/// vector is built, because a program or working directory whose meaning depends on
/// the child's own start directory is the bug this boundary exists to prevent.
fn resolved(path: &Path, basis: &Path, context: &str) -> Result<PathBuf, Error> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    if !basis.is_absolute() {
        return Err(unresolvable(basis, path, context));
    }
    let resolved = under_basis(path, basis, context)?;
    if !resolved.is_absolute() {
        return Err(invalid(
            context,
            format!(
                "{} did not resolve to an absolute path under the captured basis {}",
                path.display(),
                basis.display()
            ),
        ));
    }
    Ok(resolved)
}

/// Refuses to guess where a relative path starts from.
fn unresolvable(basis: &Path, path: &Path, context: &str) -> Error {
    invalid(
        context,
        format!(
            "the launch basis {} is not an absolute directory, so the relative path {} cannot be \
             resolved",
            basis.display(),
            path.display()
        ),
    )
}

/// Puts a non-absolute configured path under the captured basis.
///
/// Every path handed here is already known to be neither absolute nor rooted, so on
/// a host without drive prefixes there is exactly one shape: an ordinary relative
/// path, joined under the basis.
#[cfg(not(windows))]
fn under_basis(path: &Path, basis: &Path, _context: &str) -> Result<PathBuf, Error> {
    Ok(basis.join(path))
}

/// Puts a non-absolute configured path under the captured basis on Windows.
///
/// Windows has one more shape than the join operator admits: `C:install` carries a
/// drive prefix but no root, and `Path::join` *replaces* the base for such a path,
/// so joining it under `C:\manager` would keep it drive-relative and leave the game
/// resolving against whatever directory drive C happens to be in. That per-drive
/// current directory is hidden state this build neither owns nor reads, so the owner
/// rule is applied instead: the shorthand is resolved under the captured basis when
/// it names the basis's own drive - which is unambiguous - and anything else is
/// refused with the full absolute path asked for. Nothing is reinterpreted as another
/// drive's path, and no live per-drive state is consulted.
///
/// The drive a parsed Windows prefix names, if it names one at all.
///
/// The classification reads [`Prefix`]'s typed variants and no rendering of the
/// prefix text: `Disk` and `VerbatimDisk` carry a drive byte, while a UNC share, a
/// verbatim non-disk prefix and a device namespace do not. Those answer `None`, so
/// they are never treated as supplying or matching a drive, and are never guessed
/// from the leading separator that several kinds of prefix share.
#[cfg(any(windows, test))]
fn prefix_drive(kind: std::path::Prefix<'_>) -> Option<u8> {
    use std::path::Prefix::{DeviceNS, Disk, UNC, Verbatim, VerbatimDisk, VerbatimUNC};
    match kind {
        Disk(drive) | VerbatimDisk(drive) => Some(drive),
        UNC(..) | VerbatimUNC(..) | Verbatim(..) | DeviceNS(..) => None,
    }
}

/// The drive a path is anchored to, when its first component is a drive prefix.
///
/// A prefix component is produced only by a host that has drive prefixes, so this
/// answers `None` for every path on a Unix host; the typed classification above is
/// the part that holds identically on both.
#[cfg(any(windows, test))]
fn path_drive(path: &Path) -> Option<u8> {
    match path.components().next()? {
        std::path::Component::Prefix(prefix) => prefix_drive(prefix.kind()),
        _ => None,
    }
}

/// The Windows resolution, written once so the standard-library API it uses is
/// type-checked by every host that builds these tests, not only by a Windows
/// compiler.
///
/// Windows has one shape the join operator does not admit: `C:install` carries a
/// drive prefix but no root, and `Path::join` *replaces* the base for such a path, so
/// joining it under `C:\manager` would keep it drive-relative and leave the game
/// resolving against whatever directory drive C happens to be in. That per-drive
/// current directory is hidden state this build neither owns nor reads, so the owner
/// rule is applied instead: the shorthand resolves under the captured basis when it
/// names that basis's own drive - which is unambiguous - and anything else is refused
/// with the full absolute path asked for. Nothing is reinterpreted as another drive's
/// path, and no live per-drive state is consulted.
///
/// A path that arrives here with a prefix that names no drive (a device namespace,
/// for instance) is not read as a drive: it goes to the host's own join, and the
/// absolute check in [`resolved`] has the last word.
#[cfg(any(windows, test))]
fn resolve_on_windows(path: &Path, basis: &Path, context: &str) -> Result<PathBuf, Error> {
    let mut components = path.components();
    let drive = match components.next() {
        Some(std::path::Component::Prefix(prefix)) => prefix_drive(prefix.kind()),
        _ => None,
    };
    let Some(drive) = drive else {
        // No drive named: an ordinary relative or root-only path, answered by the
        // host's own join and then held to the absolute invariant.
        return Ok(basis.join(path));
    };
    let Some(basis_drive) = path_drive(basis) else {
        return Err(cross_drive(path, drive, basis, context));
    };
    if !basis_drive.eq_ignore_ascii_case(&drive) {
        return Err(cross_drive(path, drive, basis, context));
    }
    // The remaining components are appended one by one, so every name keeps its own
    // text: nothing is rebuilt from a lossy rendering of the whole path.
    let mut resolved = basis.to_path_buf();
    for component in components {
        resolved.push(component.as_os_str());
    }
    Ok(resolved)
}

/// Puts a non-absolute configured path under the captured basis on a Windows host.
///
/// The decision itself lives in [`resolve_on_windows`], so the same body that a
/// Windows host runs is the body these tests type-check everywhere else.
#[cfg(windows)]
fn under_basis(path: &Path, basis: &Path, context: &str) -> Result<PathBuf, Error> {
    resolve_on_windows(path, basis, context)
}

/// The refusal for a drive the captured basis does not name.
#[cfg(any(windows, test))]
fn cross_drive(path: &Path, drive: u8, basis: &Path, context: &str) -> Error {
    // Only the message renders the remainder as text, and only to say what to write;
    // the decision above never depends on a rendered path.
    let rest = path
        .components()
        .skip(1)
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<String>>()
        .join("\\");
    let drive = char::from(drive.to_ascii_uppercase());
    let absolute = format!("{drive}:\\{rest}");
    invalid(
        context,
        format!(
            "the configured path {} names drive {drive}, which the captured basis {} does not name; configure the full absolute path {absolute} instead",
            path.display(),
            basis.display()
        ),
    )
}

/// Insists that a selected macOS path represents an explicit application bundle.
///
/// This is the naming rule the owner chose - `game_root` is the configured `.app`
/// bundle, and `executable_override` may name another one - and nothing more: no
/// bundle is discovered, no `Info.plist` is read, and no claim is made that a
/// directory so named is a complete or launchable installation.
fn require_bundle(path: &Path, context: &str, setting: &str) -> Result<(), Error> {
    let named_a_bundle = path
        .extension()
        .is_some_and(|suffix| suffix == MACOS_BUNDLE);
    if !named_a_bundle {
        return Err(invalid(
            context,
            format!(
                "{setting} {} is not an .app application bundle",
                path.display()
            ),
        ));
    }
    Ok(())
}

/// Insists a configured path is an existing directory, naming the setting.
fn require_directory(path: &Path, context: &str, setting: &str) -> Result<(), Error> {
    let found = path.metadata().map_err(|source| failed(path, source))?;
    if !found.is_dir() {
        return Err(invalid(
            context,
            format!("{setting} {} is not a directory", path.display()),
        ));
    }
    Ok(())
}

/// Insists an explicitly configured executable is an existing file, naming it.
fn require_file(path: &Path, context: &str, setting: &str) -> Result<(), Error> {
    let found = path.metadata().map_err(|source| failed(path, source))?;
    if !found.is_file() {
        return Err(invalid(
            context,
            format!("{setting} {} is not a file", path.display()),
        ));
    }
    Ok(())
}

/// Reports a configured value this build cannot use, with the setting named.
fn invalid(context: &str, message: String) -> Error {
    Error::InvalidInput {
        context: context.to_string(),
        message,
    }
}

/// Reports a filesystem failure on a configured path.
fn failed(path: &Path, source: io::Error) -> Error {
    Error::Io {
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::{Path, path_drive, resolved};

    fn resolve(path: &str, basis: &str) -> Result<std::path::PathBuf, crate::domain::types::Error> {
        resolved(Path::new(path), Path::new(basis), "the Windows launch")
    }

    /// The text of a resolved path, for assertions that name the whole form.
    fn text(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn a_drive_relative_path_on_the_basis_drive_resolves_under_the_basis() {
        // `C:install` under `C:\manager`: the same drive, so the shorthand has one
        // unambiguous meaning and std's own join would have kept it relative.
        let resolved_path = resolve(r"C:install", r"C:\manager").expect("same drive resolves");
        assert_eq!(text(&resolved_path), r"C:\manager\install");
        assert!(
            resolved_path.is_absolute(),
            "and the result is absolute, so neither the program nor the child's start \
             directory depends on a current directory"
        );
    }

    #[test]
    fn a_drive_relative_override_resolves_the_same_way() {
        let resolved_path =
            resolve(r"C:bin\launcher.exe", r"C:\manager").expect("same drive resolves");
        assert_eq!(text(&resolved_path), r"C:\manager\bin\launcher.exe");
        assert!(resolved_path.is_absolute());
    }

    #[test]
    fn the_basis_drive_is_matched_without_regard_to_case() {
        let resolved_path = resolve(r"c:install", r"C:\manager").expect("same drive, any case");
        assert_eq!(text(&resolved_path), r"C:\manager\install");
    }

    #[test]
    fn a_path_on_another_drive_is_refused_and_asks_for_the_absolute_form() {
        // The owner rule: no live per-drive current directory is consulted, and the
        // path is never reinterpreted as belonging to the basis's drive.
        let error = resolve(r"D:install", r"C:\manager").expect_err("cross drive has no basis");
        let message = error.to_string();
        assert!(
            message.contains(r"D:\install"),
            "the refusal asks for the full path on that drive: {message}"
        );
        assert!(
            !message.contains(r"C:\manager\install") && !message.contains(r"C:\manager\D:install"),
            "and does not quietly resolve one drive under another: {message}"
        );
    }

    #[test]
    fn a_basis_that_names_no_drive_cannot_supply_one() {
        let error = resolve(r"C:install", r"\\server\share").expect_err("no drive in the basis");
        assert!(
            error.to_string().contains(r"C:\install"),
            "the refusal still asks for the absolute form: {error}"
        );
    }

    #[test]
    fn rooted_and_unc_paths_are_used_exactly_as_configured() {
        for configured in [r"C:\Games\RimWorld", r"\\server\share\RimWorld"] {
            let resolved_path =
                resolve(configured, r"C:\manager").expect("an absolute path is its own answer");
            assert_eq!(text(&resolved_path), configured);
        }
    }

    #[test]
    fn an_ordinary_relative_path_still_joins_under_the_basis() {
        let resolved_path = resolve(r"install", r"C:\manager").expect("ordinary relative resolves");
        assert_eq!(text(&resolved_path), r"C:\manager\install");
    }

    #[test]
    fn a_verbatim_drive_basis_supplies_the_same_drive() {
        // `\\?\C:\manager` and `C:\manager` name the same drive in two different
        // forms, so the shorthand resolves under either basis.
        let resolved_path =
            resolve(r"C:install", r"\\?\C:\manager").expect("verbatim drive basis matches");
        assert_eq!(text(&resolved_path), r"\\?\C:\manager\install");
        assert!(resolved_path.is_absolute(), "and the result is absolute");
    }

    #[test]
    fn a_unc_basis_supplies_no_drive_and_is_refused() {
        // A share has no drive to match, even though its text starts with the same
        // two separators a verbatim prefix does, so the drive-relative path is
        // refused rather than hung under the share.
        let error = resolve(r"C:install", r"\\server\share").expect_err("a share is not a drive");
        let message = error.to_string();
        assert!(
            message.contains(r"C:\install"),
            "the refusal asks for the absolute form: {message}"
        );
        assert!(
            !message.contains(r"\\server\share\install"),
            "and nothing is built under the share: {message}"
        );
    }

    #[test]
    fn a_root_only_path_keeps_the_drive_of_the_captured_basis() {
        // `\install` has a root but no prefix, and std documents that joining such a
        // path replaces everything except the prefix of the path it is joined onto:
        // under the captured `C:\manager` basis it answers `C:\install`. The drive
        // therefore comes from the one captured basis, not from whichever directory
        // some drive happens to be in, and the absolute invariant is satisfied rather
        // than tripped. An earlier version of this case expected a refusal, which was
        // wrong: the invariant is checked after resolution, once the basis has
        // supplied the drive.
        let resolved_path =
            resolve(r"\install", r"C:\manager").expect("the captured basis supplies the drive");
        assert_eq!(text(&resolved_path), r"C:\install");
        assert!(
            resolved_path.is_absolute(),
            "a rooted path under an absolute basis stays absolute"
        );
        assert_eq!(
            path_drive(&resolved_path),
            Some(b'C'),
            "and the drive it carries is the basis's own"
        );
    }

    #[test]
    fn a_relative_basis_is_refused_rather_than_guessed() {
        let error = resolve(r"install", r"C:manager").expect_err("the basis itself is relative");
        assert!(
            error.to_string().contains("not an absolute directory"),
            "the refusal says why: {error}"
        );
    }
}

#[cfg(test)]
mod typed_prefix_tests {
    //! The typed prefix classification and the no-drive route of the Windows
    //! resolver, exercised on every host that builds these tests. A path with a real
    //! Windows prefix is only produced by a Windows host, so the drive cases live in
    //! the guarded module above; what is provable here is that the classification
    //! reads `Prefix`'s variants rather than a rendered first character, and that the
    //! shared body compiles and behaves for everything without a prefix.
    use super::{Path, PathBuf, path_drive, resolve_on_windows};
    use std::ffi::OsStr;
    use std::path::Prefix;

    #[test]
    fn a_disk_prefix_names_a_drive_in_either_spelling() {
        assert_eq!(super::prefix_drive(Prefix::Disk(b'C')), Some(b'C'));
        assert_eq!(super::prefix_drive(Prefix::VerbatimDisk(b'd')), Some(b'd'));
    }

    #[test]
    fn a_share_verbatim_path_and_device_namespace_name_no_drive() {
        let share = OsStr::new("server");
        assert_eq!(super::prefix_drive(Prefix::UNC(share, share)), None);
        assert_eq!(super::prefix_drive(Prefix::VerbatimUNC(share, share)), None);
        assert_eq!(super::prefix_drive(Prefix::Verbatim(share)), None);
        assert_eq!(super::prefix_drive(Prefix::DeviceNS(share)), None);
    }

    #[test]
    fn a_verbatim_drive_basis_and_a_unc_basis_are_told_apart() {
        // Both spellings begin with the same separators, so only the parsed kind can
        // decide which of them can supply a drive.
        assert_eq!(super::prefix_drive(Prefix::VerbatimDisk(b'C')), Some(b'C'));
        assert_eq!(
            super::prefix_drive(Prefix::UNC(OsStr::new("server"), OsStr::new("share"))),
            None
        );
    }

    #[test]
    fn a_share_path_reports_no_drive_on_every_host() {
        // A share never supplies a drive: parsed as a UNC prefix on Windows, and as an
        // ordinary relative name on a Unix host, which is why this holds both ways.
        assert_eq!(path_drive(Path::new(r"\\server\share")), None);
    }

    #[cfg(not(windows))]
    #[test]
    fn drive_shaped_text_is_only_a_relative_name_on_this_host() {
        // Recorded rather than hidden: on a Unix host neither form carries a prefix, so
        // the drive route in the shared body cannot be reached here at all. The
        // Windows-guarded cases above are what exercise it, and only a Windows host
        // compiles and runs them.
        assert_eq!(path_drive(Path::new(r"C:\manager")), None);
        assert_eq!(path_drive(Path::new("C:install")), None);
    }

    #[test]
    fn a_path_without_a_drive_is_joined_under_the_absolute_basis() {
        let resolved_path = resolve_on_windows(
            Path::new("install"),
            Path::new("/manager"),
            "the Windows launch",
        )
        .expect("an ordinary relative path resolves under an absolute basis");
        assert_eq!(resolved_path, PathBuf::from("/manager/install"));
    }
}
