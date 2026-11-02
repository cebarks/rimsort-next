//! Owned, journaled publication beside the game's scanned Mods directory.
use crate::{
    application::acquisition::AcquisitionError,
    domain::{metadata::parse_about, types::GameVersion},
    integrations::workshop::PublishedFileId,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

#[cfg(test)]
#[path = "../../tests/support/install_recovery.rs"]
mod tests;

const MAX_STATE: u64 = 4 * 1024 * 1024;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Owner {
    root: PathBuf,
    token: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackupRecord {
    pub path: PathBuf,
    pub fingerprint: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallRecord {
    pub fingerprint: String,
    pub upstream_updated: Option<u64>,
    pub backup: Option<BackupRecord>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    records: BTreeMap<PublishedFileId, InstallRecord>,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
enum Phase {
    Prepared,
    MoveOld,
    PlaceNew,
    CommitManifest,
    Complete,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    owner: Owner,
    item: PublishedFileId,
    old: Option<InstallRecord>,
    new: InstallRecord,
    stage: PathBuf,
    backup: PathBuf,
    phase: Phase,
}
#[derive(Debug)]
pub struct Promotion {
    pub path: PathBuf,
    pub record: InstallRecord,
    pub warnings: Vec<String>,
}
pub struct Installer {
    root: PathBuf,
    home: PathBuf,
    owner: Owner,
    manifest: Manifest,
    warnings: Vec<String>,
    _lock: File,
}

impl Drop for Installer {
    fn drop(&mut self) {
        // Explicit unlock also releases duplicated descriptors inherited across fork/exec.
        let _ = self._lock.unlock();
    }
}

fn refuse(message: impl Into<String>) -> AcquisitionError {
    AcquisitionError::Recovery(message.into())
}

/// Refuse symbolic links in every existing component, not just the leaf.
pub(crate) fn no_links(path: &Path) -> Result<(), AcquisitionError> {
    if !path.is_absolute() {
        return Err(refuse("paths must be absolute"));
    }
    let mut cursor = PathBuf::new();
    for component in path.components() {
        if matches!(component, std::path::Component::ParentDir) {
            return Err(refuse("parent traversal is forbidden"));
        }
        cursor.push(component);
        match fs::symlink_metadata(&cursor) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(refuse(format!("link refused: {}", cursor.display())));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
fn present(path: &Path) -> Result<bool, AcquisitionError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}
fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, AcquisitionError> {
    no_links(path)?;
    if !fs::metadata(path)?.is_file() {
        return Err(refuse("state is not a regular file"));
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_STATE + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_STATE {
        return Err(refuse("state exceeds 4 MiB"));
    }
    serde_json::from_slice(&bytes).map_err(|e| refuse(format!("{}: {e}", path.display())))
}
fn atomic_json(path: &Path, value: &impl Serialize) -> Result<(), AcquisitionError> {
    no_links(path)?;
    let parent = path.parent().ok_or_else(|| refuse("state has no parent"))?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut file, value).map_err(|e| refuse(e.to_string()))?;
    file.flush()?;
    if file.as_file().metadata()?.len() > MAX_STATE {
        return Err(refuse("state exceeds 4 MiB"));
    }
    file.as_file().sync_all()?;
    file.persist(path)
        .map_err(|e| AcquisitionError::Io(e.error))?;
    sync_directory(parent)
}
fn sync_directory(path: &Path) -> Result<(), AcquisitionError> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}
impl Installer {
    /// Acquires the OS lock and recovers before allowing further mutation.
    pub fn open(mods_root: &Path) -> Result<Self, AcquisitionError> {
        no_links(mods_root)?;
        let root = mods_root.canonicalize()?;
        if !root.is_dir() {
            return Err(refuse("Mods root must be a directory"));
        }
        let name = root
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| refuse("unrepresentable Mods name"))?;
        let home = root
            .parent()
            .ok_or_else(|| refuse("Mods root has no parent"))?
            .join(format!(".rimsort-next-{name}"));
        no_links(&home)?;
        let created = match fs::create_dir(&home) {
            Ok(()) => true,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(e) => return Err(e.into()),
        };
        if created {
            let unique = tempfile::NamedTempFile::new_in(&home)?;
            let token = unique
                .path()
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or_else(|| refuse("owner identity"))?
                .to_string();
            atomic_json(
                &home.join("owner.json"),
                &Owner {
                    root: root.clone(),
                    token,
                },
            )?;
        }
        let owner: Owner = read_json(&home.join("owner.json"))?;
        if owner.root != root || owner.token.is_empty() {
            return Err(refuse("installation owner/root mismatch"));
        }
        let lock_path = home.join("lock");
        no_links(&lock_path)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)?;
        lock.try_lock()
            .map_err(|e| refuse(format!("installation is locked: {e}")))?;
        if created {
            fs::create_dir(home.join("staging"))?;
            fs::create_dir(home.join("backups"))?;
            atomic_json(&home.join("manifest.json"), &Manifest::default())?;
        }
        for path in [home.join("staging"), home.join("backups")] {
            no_links(&path)?;
            if !path.is_dir() {
                return Err(refuse("missing management directory"));
            }
        }
        let manifest = read_json(&home.join("manifest.json"))?;
        let mut installer = Self {
            root,
            home,
            owner,
            manifest,
            warnings: Vec::new(),
            _lock: lock,
        };
        installer.recover()?;
        Ok(installer)
    }
    pub fn take_warnings(&mut self) -> Vec<String> {
        std::mem::take(&mut self.warnings)
    }

    pub fn observation(
        &self,
        id: PublishedFileId,
    ) -> Result<Option<InstallRecord>, AcquisitionError> {
        match self.manifest.records.get(&id) {
            Some(record) if fingerprint(&self.target(id))? == record.fingerprint => {
                Ok(Some(record.clone()))
            }
            Some(_) => Err(refuse("installed content differs from ownership record")),
            None => Ok(None),
        }
    }
    fn target(&self, id: PublishedFileId) -> PathBuf {
        self.root.join(id.get().to_string())
    }
    fn save_manifest(&self) -> Result<(), AcquisitionError> {
        atomic_json(&self.home.join("manifest.json"), &self.manifest)
    }
    fn save_journal(&self, journal: &Journal) -> Result<(), AcquisitionError> {
        atomic_json(&self.home.join("journal.json"), journal)
    }
    fn phase(&self, journal: &mut Journal, phase: Phase) -> Result<(), AcquisitionError> {
        journal.phase = phase;
        self.save_journal(journal)
    }

    fn prepare(
        &mut self,
        id: PublishedFileId,
        source: &Path,
        version: &GameVersion,
        updated: Option<u64>,
        cancel: &AtomicBool,
    ) -> Result<Journal, AcquisitionError> {
        self.recover()?;
        if cancel.load(Ordering::Acquire) {
            return Err(AcquisitionError::Cancelled);
        }
        let old = self.manifest.records.get(&id).cloned();
        let target = self.target(id);
        match &old {
            Some(record) if fingerprint(&target)? != record.fingerprint => {
                return Err(refuse("local edits refuse update"));
            }
            None if present(&target)? => return Err(refuse("unmanaged destination collision")),
            _ => {}
        }
        let stage = tempfile::Builder::new()
            .prefix("stage-")
            .tempdir_in(self.home.join("staging"))?;
        copy_tree(source, stage.path(), cancel, 0)?;
        validate_about(stage.path(), version)?;
        let new_hash = fingerprint(stage.path())?;
        if fingerprint(source)? != new_hash {
            return Err(refuse("source changed during staging"));
        }
        if cancel.load(Ordering::Acquire) {
            return Err(AcquisitionError::Cancelled);
        }
        let name = stage
            .path()
            .file_name()
            .ok_or_else(|| refuse("stage identity"))?;
        let backup = self.home.join("backups").join(name);
        if present(&backup)? {
            return Err(refuse("backup collision"));
        }
        let new = InstallRecord {
            fingerprint: new_hash,
            upstream_updated: updated,
            backup: old.as_ref().map(|r| BackupRecord {
                path: backup.clone(),
                fingerprint: r.fingerprint.clone(),
            }),
        };
        let journal = Journal {
            owner: self.owner.clone(),
            item: id,
            old,
            new,
            stage: stage.path().to_path_buf(),
            backup,
            phase: Phase::Prepared,
        };
        self.save_journal(&journal)?;
        let _ = stage.keep();
        Ok(journal)
    }
    pub fn promote(
        &mut self,
        id: PublishedFileId,
        source: &Path,
        version: &GameVersion,
        updated: Option<u64>,
        cancel: &AtomicBool,
    ) -> Result<Promotion, AcquisitionError> {
        let journal = self.prepare(id, source, version, updated, cancel)?;
        if cancel.load(Ordering::Acquire) {
            self.recover()?;
            return Err(AcquisitionError::Cancelled);
        }
        self.publish(journal)
    }
    fn publish(&mut self, mut journal: Journal) -> Result<Promotion, AcquisitionError> {
        let target = self.target(journal.item);
        self.phase(&mut journal, Phase::MoveOld)?;
        if let Some(old) = &journal.old {
            if fingerprint(&target)? != old.fingerprint {
                return Err(refuse("target changed before rename"));
            }
            if present(&journal.backup)? {
                return Err(refuse("backup destination appeared during promotion"));
            }
            fs::rename(&target, &journal.backup)?;
            sync_directory(&self.root)?;
            sync_directory(&self.home.join("backups"))?;
            if fingerprint(&journal.backup)? != old.fingerprint {
                if !present(&target)? {
                    fs::rename(&journal.backup, &target)?;
                }
                return Err(refuse("target changed during rename; publication stopped"));
            }
        }
        self.phase(&mut journal, Phase::PlaceNew)?;
        if present(&target)? {
            return Err(refuse("destination appeared during promotion"));
        }
        if fingerprint(&journal.stage)? != journal.new.fingerprint {
            return Err(refuse("stage changed before publication"));
        }
        fs::rename(&journal.stage, &target)?;
        sync_directory(&self.root)?;
        sync_directory(&self.home.join("staging"))?;
        self.phase(&mut journal, Phase::CommitManifest)?;
        self.recover()?;
        Ok(Promotion {
            path: target,
            record: journal.new,
            warnings: self.take_warnings(),
        })
    }
    fn cleanup_old_backup(&self, old: Option<&InstallRecord>) -> Vec<String> {
        let Some(backup) = old.and_then(|r| r.backup.as_ref()) else {
            return Vec::new();
        };
        let clean = (|| {
            self.owned_child(&backup.path, "backups")?;
            if !present(&backup.path)? {
                return Ok(());
            }
            if fingerprint(&backup.path)? != backup.fingerprint {
                return Err(refuse("previous backup modified"));
            }
            fs::remove_dir_all(&backup.path)?;
            sync_directory(&self.home.join("backups"))
        })();
        clean
            .err()
            .map(|e| vec![format!("older backup retained: {e}")])
            .unwrap_or_default()
    }
    fn owned_child(&self, path: &Path, area: &str) -> Result<(), AcquisitionError> {
        if path.parent() != Some(self.home.join(area).as_path()) || path.file_name().is_none() {
            return Err(refuse("journal path is outside owned area"));
        }
        no_links(path)
    }
    fn verify_journal(&self, j: &Journal) -> Result<(), AcquisitionError> {
        if j.owner != self.owner {
            return Err(refuse("journal ownership mismatch"));
        }
        self.owned_child(&j.stage, "staging")?;
        self.owned_child(&j.backup, "backups")?;
        let expected = j.old.as_ref().map(|old| BackupRecord {
            path: j.backup.clone(),
            fingerprint: old.fingerprint.clone(),
        });
        if j.new.backup != expected {
            return Err(refuse("journal backup record mismatch"));
        }
        if let Some(old) = &j.old
            && let Some(backup) = &old.backup
        {
            self.owned_child(&backup.path, "backups")?;
        }
        Ok(())
    }
    /// Idempotent actual-layout reconciliation; every mismatch preserves the journal and all copies.
    pub fn recover(&mut self) -> Result<(), AcquisitionError> {
        let path = self.home.join("journal.json");
        if !present(&path)? {
            return Ok(());
        }
        let mut j: Journal = read_json(&path)?;
        self.verify_journal(&j)?;
        let disk: Manifest = read_json(&self.home.join("manifest.json"))?;
        let record = disk.records.get(&j.item);
        if record != j.old.as_ref() && record != Some(&j.new) {
            return Err(refuse("unexpected manifest record"));
        }
        self.manifest = disk;
        let target = self.target(j.item);
        let t = optional_fingerprint(&target)?;
        let s = optional_fingerprint(&j.stage)?;
        let b = optional_fingerprint(&j.backup)?;
        if s.as_ref().is_some_and(|h| h != &j.new.fingerprint) {
            return Err(refuse("stage fingerprint mismatch"));
        }
        let old_hash = j.old.as_ref().map(|r| &r.fingerprint);
        if b.as_ref().is_some_and(|h| Some(h) != old_hash) {
            return Err(refuse("backup fingerprint mismatch"));
        }
        let old_record = self.manifest.records.get(&j.item) == j.old.as_ref();
        let published =
            t.as_ref() == Some(&j.new.fingerprint) && s.is_none() && b.as_ref() == old_hash;
        let rollback = old_record && b.is_none() && t.as_ref() == old_hash;
        let restore = old_record
            && t.is_none()
            && old_hash.is_some()
            && b.as_ref() == old_hash
            && s.is_some();
        if published {
            self.phase(&mut j, Phase::CommitManifest)?;
            let previous = self.manifest.records.insert(j.item, j.new.clone());
            if let Err(error) = self.save_manifest() {
                match previous {
                    Some(record) => {
                        self.manifest.records.insert(j.item, record);
                    }
                    None => {
                        self.manifest.records.remove(&j.item);
                    }
                }
                return Err(error);
            }
        } else if rollback || restore {
            if restore {
                fs::rename(&j.backup, &target)?;
                sync_directory(&self.root)?;
                sync_directory(&self.home.join("backups"))?;
            }
            if s.is_some() {
                fs::remove_dir_all(&j.stage)?;
                sync_directory(&self.home.join("staging"))?;
            }
        } else {
            return Err(refuse(
                "unrecognized installation layout; all copies retained",
            ));
        }
        self.phase(&mut j, Phase::Complete)?;
        if published {
            self.warnings
                .extend(self.cleanup_old_backup(j.old.as_ref()));
        }
        fs::remove_file(&path)?;
        sync_directory(&self.home)
    }
}
fn optional_fingerprint(path: &Path) -> Result<Option<String>, AcquisitionError> {
    if present(path)? {
        fingerprint(path).map(Some)
    } else {
        Ok(None)
    }
}

/// Sorted length-delimited UTF-8 paths, type tags and streamed file-content digests.
pub fn fingerprint(root: &Path) -> Result<String, AcquisitionError> {
    no_links(root)?;
    let mut digest = Sha256::new();
    hash_tree(root, root, &mut digest, 0)?;
    Ok(digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}
fn entries(path: &Path, depth: usize) -> Result<Vec<fs::DirEntry>, AcquisitionError> {
    if depth > 128 {
        return Err(refuse("content tree depth exceeds 128"));
    }
    no_links(path)?;
    if !fs::symlink_metadata(path)?.is_dir() {
        return Err(refuse("content root must be a directory"));
    }
    let mut entries = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    Ok(entries)
}
fn regular(entry: &fs::DirEntry) -> Result<fs::FileType, AcquisitionError> {
    let kind = entry.file_type()?;
    if !kind.is_dir() && !kind.is_file() {
        return Err(refuse("links and special files are refused"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if kind.is_file() && entry.metadata()?.nlink() != 1 {
            return Err(refuse("hard-linked content refused"));
        }
    }
    if entry.file_name().to_str().is_none() {
        return Err(refuse("non-UTF8 content path refused"));
    }
    Ok(kind)
}
fn hash_tree(
    root: &Path,
    path: &Path,
    digest: &mut Sha256,
    depth: usize,
) -> Result<(), AcquisitionError> {
    for entry in entries(path, depth)? {
        let kind = regular(&entry)?;
        let path = entry.path();
        let rel = path.strip_prefix(root).map_err(|e| refuse(e.to_string()))?;
        let name = rel.to_str().ok_or_else(|| refuse("unrepresentable path"))?;
        digest.update([u8::from(kind.is_file())]);
        digest.update((name.len() as u64).to_be_bytes());
        digest.update(name.as_bytes());
        if kind.is_dir() {
            hash_tree(root, &path, digest, depth + 1)?;
        } else {
            let mut file = File::open(path)?;
            let mut content = Sha256::new();
            let mut buffer = [0; 64 * 1024];
            let mut length = 0u64;
            loop {
                let count = file.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                length += count as u64;
                content.update(&buffer[..count]);
            }
            digest.update(length.to_be_bytes());
            digest.update(content.finalize());
        }
    }
    Ok(())
}
fn copy_tree(
    source: &Path,
    destination: &Path,
    cancel: &AtomicBool,
    depth: usize,
) -> Result<(), AcquisitionError> {
    for entry in entries(source, depth)? {
        if cancel.load(Ordering::Acquire) {
            return Err(AcquisitionError::Cancelled);
        }
        let kind = regular(&entry)?;
        let dest = destination.join(entry.file_name());
        if kind.is_dir() {
            fs::create_dir(&dest)?;
            copy_tree(&entry.path(), &dest, cancel, depth + 1)?;
        } else {
            let mut input = File::open(entry.path())?;
            let mut output = OpenOptions::new().write(true).create_new(true).open(dest)?;
            let mut buffer = [0; 64 * 1024];
            loop {
                if cancel.load(Ordering::Acquire) {
                    return Err(AcquisitionError::Cancelled);
                }
                let count = input.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                output.write_all(&buffer[..count])?;
            }
            output.sync_all()?;
        }
    }
    sync_directory(destination)
}
fn validate_about(root: &Path, version: &GameVersion) -> Result<(), AcquisitionError> {
    let about = root.join("About/About.xml");
    no_links(&about)?;
    let mut bytes = Vec::new();
    File::open(&about)?
        .take(MAX_STATE + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_STATE {
        return Err(refuse("About.xml exceeds 4 MiB"));
    }
    let mut reader = quick_xml::Reader::from_reader(bytes.as_slice());
    let mut depth = 0;
    loop {
        match reader.read_event().map_err(|e| refuse(e.to_string()))? {
            quick_xml::events::Event::Start(_) => {
                depth += 1;
                if depth > 128 {
                    return Err(refuse("About.xml depth exceeds 128"));
                }
            }
            quick_xml::events::Event::End(_) => depth -= 1,
            quick_xml::events::Event::Eof => break,
            _ => {}
        }
    }
    let parsed = parse_about(&bytes, version).map_err(|e| refuse(e.to_string()))?;
    if !parsed.is_valid()
        || parsed.package_id.is_none()
        || parsed.steam_app_id.is_some_and(|id| id != 294100)
    {
        return Err(refuse("invalid RimWorld About.xml metadata"));
    }
    Ok(())
}
