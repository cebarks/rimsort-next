//! Atomic, schema-versioned named checkpoints. Names are display text, never paths.
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
#[error("profile library: {0}")]
pub struct ProfileError(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Revision {
    pub tokens: Vec<String>,
    /// Seconds since the Unix epoch (UTC).
    pub timestamp: u64,
    pub reason: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub id: u64,
    pub name: String,
    pub revisions: Vec<Revision>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema: u32,
    next_id: u64,
    profiles: Vec<Profile>,
}

#[derive(Debug)]
pub struct ProfileLibrary {
    path: PathBuf,
    document: Document,
    baseline: Option<Vec<u8>>,
}

fn error(error: impl std::fmt::Display) -> ProfileError {
    ProfileError(error.to_string())
}
fn revision(tokens: &[String], reason: &str) -> Result<Revision, ProfileError> {
    Ok(Revision {
        tokens: tokens.to_vec(),
        timestamp: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(error)?
            .as_secs(),
        reason: reason.into(),
    })
}
fn name(value: &str) -> Result<String, ProfileError> {
    if value.chars().any(char::is_control) || value.trim().is_empty() {
        return Err(error(
            "names must be nonempty and contain no control characters",
        ));
    }
    Ok(value.trim().into())
}
fn validate(doc: &Document) -> Result<(), ProfileError> {
    if doc.schema != 1 {
        return Err(error(format!(
            "unsupported schema {} (expected 1)",
            doc.schema
        )));
    }
    if doc.profiles.is_empty() {
        return Err(error("at least one profile is required"));
    }
    let mut ids = HashSet::new();
    let mut names = HashSet::new();
    for p in &doc.profiles {
        if p.id == 0 || p.id >= doc.next_id || !ids.insert(p.id) {
            return Err(error("invalid or repeated profile identity"));
        }
        if name(&p.name)? != p.name || !names.insert(p.name.to_lowercase()) {
            return Err(error("invalid or duplicate profile name"));
        }
        if p.revisions.is_empty()
            || p.revisions.len() > 50
            || p.revisions.windows(2).any(|w| w[0].tokens == w[1].tokens)
        {
            return Err(error("invalid revision history"));
        }
    }
    Ok(())
}
impl ProfileLibrary {
    pub fn open(path: &Path, initial: &[String]) -> Result<Self, ProfileError> {
        match fs::File::open(path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(MAX_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map_err(error)?;
                if bytes.len() as u64 > MAX_BYTES {
                    return Err(error("document exceeds 32 MiB; left untouched"));
                }
                let document: Document = serde_json::from_slice(&bytes).map_err(error)?;
                validate(&document)?;
                Ok(Self {
                    path: path.into(),
                    document,
                    baseline: Some(bytes),
                })
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let document = Document {
                    schema: 1,
                    next_id: 2,
                    profiles: vec![Profile {
                        id: 1,
                        name: "Default".into(),
                        revisions: vec![revision(initial, "Initial game baseline")?],
                    }],
                };
                let mut library = Self {
                    path: path.into(),
                    document,
                    baseline: None,
                };
                library.baseline = Some(library.persist(&library.document)?);
                Ok(library)
            }
            Err(e) => Err(error(e)),
        }
    }
    pub fn profiles(&self) -> &[Profile] {
        &self.document.profiles
    }
    fn persist(&self, candidate: &Document) -> Result<Vec<u8>, ProfileError> {
        validate(candidate)?;
        let bytes = serde_json::to_vec_pretty(candidate).map_err(error)?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(error("document exceeds 32 MiB; nothing written"));
        }
        let parent = self
            .path
            .parent()
            .ok_or_else(|| error("missing parent directory"))?;
        fs::create_dir_all(parent).map_err(error)?;
        // A stable sidecar inode serializes compare + rename across app instances.
        // It is never removed: unlinking a lock file would let waiters lock different inodes.
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.path.with_extension("json.lock"))
            .map_err(error)?;
        lock.try_lock().map_err(|failure| match failure {
            fs::TryLockError::WouldBlock => {
                error("library is busy in another operation; retry later")
            }
            fs::TryLockError::Error(failure) => error(failure),
        })?;
        let current = match fs::File::open(&self.path) {
            Ok(file) => {
                let mut current = Vec::new();
                file.take(MAX_BYTES + 1)
                    .read_to_end(&mut current)
                    .map_err(error)?;
                Some(current)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(error(e)),
        };
        if current != self.baseline {
            return Err(error(
                "profiles.json changed since it was loaded; reopen the library before saving (no changes written)",
            ));
        }
        let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(error)?;
        temp.write_all(&bytes).map_err(error)?;
        temp.as_file().sync_all().map_err(error)?;
        temp.persist(&self.path).map_err(error)?;
        Ok(bytes)
    }
    fn change<T>(
        &mut self,
        change: impl FnOnce(&mut Document) -> Result<T, ProfileError>,
    ) -> Result<T, ProfileError> {
        let mut candidate = self.document.clone();
        let result = change(&mut candidate)?;
        let bytes = self.persist(&candidate)?;
        self.document = candidate;
        self.baseline = Some(bytes);
        Ok(result)
    }
    pub fn create(&mut self, display: &str, tokens: &[String]) -> Result<u64, ProfileError> {
        let name = name(display)?;
        self.change(|doc| {
            let id = doc.next_id;
            doc.next_id = id
                .checked_add(1)
                .ok_or_else(|| error("profile identities exhausted"))?;
            doc.profiles.push(Profile {
                id,
                name,
                revisions: vec![revision(tokens, "Created checkpoint")?],
            });
            Ok(id)
        })
    }
    pub fn save_revision(
        &mut self,
        id: u64,
        tokens: &[String],
        reason: &str,
    ) -> Result<(), ProfileError> {
        let profile = self
            .document
            .profiles
            .iter()
            .find(|p| p.id == id)
            .ok_or_else(|| error("unknown profile"))?;
        if profile.revisions[0].tokens == tokens {
            return self.change(|_| Ok(()));
        }
        self.change(|doc| {
            let profile = profile_mut(doc, id)?;
            profile.revisions.insert(0, revision(tokens, reason)?);
            profile.revisions.truncate(50);
            Ok(())
        })
    }
    pub fn rename(&mut self, id: u64, display: &str) -> Result<(), ProfileError> {
        let name = name(display)?;
        self.change(|doc| {
            profile_mut(doc, id)?.name = name;
            Ok(())
        })
    }
    pub fn duplicate(&mut self, id: u64, display: &str) -> Result<u64, ProfileError> {
        let tokens = self
            .document
            .profiles
            .iter()
            .find(|p| p.id == id)
            .ok_or_else(|| error("unknown profile"))?
            .revisions[0]
            .tokens
            .clone();
        self.create(display, &tokens)
    }
    pub fn delete(&mut self, id: u64) -> Result<(), ProfileError> {
        self.change(|doc| {
            profile_mut(doc, id)?;
            doc.profiles.retain(|p| p.id != id);
            Ok(())
        })
    }
}
fn profile_mut(doc: &mut Document, id: u64) -> Result<&mut Profile, ProfileError> {
    doc.profiles
        .iter_mut()
        .find(|p| p.id == id)
        .ok_or_else(|| error("unknown profile"))
}
