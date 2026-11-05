//! Bounded, atomic acquisition preferences and historical receipts, separate from game settings.
use crate::application::acquisition::{AcquisitionBackend, AcquisitionReport};
use crate::integrations::workshop::WorkshopItem;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub(crate) mod budget;

const LIMIT: u64 = 4 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Preferences {
    pub schema_version: u32,
    pub executable: PathBuf,
    pub timeout_seconds: u64,
}
impl Preferences {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err("Unsupported acquisition preferences schema".into());
        }
        if !self.executable.is_absolute() || !self.executable.is_file() {
            return Err(
                "SteamCMD must be an absolute existing executable file; install it manually".into(),
            );
        }
        if !(1..=86400).contains(&self.timeout_seconds) {
            return Err("Timeout must be 1–86400 seconds (default 900)".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SteamClientPreferences {
    pub schema_version: u32,
    pub helper_override: Option<PathBuf>,
    pub runtime_dir: Option<PathBuf>,
    pub timeout_seconds: u64,
}
impl Default for SteamClientPreferences {
    fn default() -> Self {
        Self {
            schema_version: 1,
            helper_override: None,
            runtime_dir: None,
            timeout_seconds: 900,
        }
    }
}
impl SteamClientPreferences {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1 || !(1..=86400).contains(&self.timeout_seconds) {
            return Err("Native preferences require schema 1 and timeout 1–86400 seconds".into());
        }
        if self
            .helper_override
            .as_ref()
            .is_some_and(|p| !p.is_absolute() || !p.is_file())
        {
            return Err("Helper override must be an absolute existing file; no fallback".into());
        }
        if self
            .runtime_dir
            .as_ref()
            .is_some_and(|p| !p.is_absolute() || !p.is_dir())
        {
            return Err(
                "Runtime override must be an absolute existing directory; no fallback".into(),
            );
        }
        Ok(())
    }
    pub fn config(&self, main: &Path) -> Result<super::steam_client::SteamClientConfig, String> {
        self.validate()?;
        Ok(super::steam_client::SteamClientConfig {
            helper_path: match &self.helper_override {
                Some(p) => p.clone(),
                None => super::steam_client::sibling_helper(main).map_err(|e| e.to_string())?,
            },
            runtime_dir: self.runtime_dir.clone(),
            total_deadline: std::time::Duration::from_secs(self.timeout_seconds),
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FinalReport {
    pub schema_version: u32,
    pub request_id: String,
    pub source_generation: u64,
    pub source_revision: u64,
    pub backend: AcquisitionBackend,
    pub mods_root: PathBuf,
    pub executable: PathBuf,
    pub timeout_seconds: u64,
    pub target_version: String,
    pub requested_at: Option<u64>,
    pub returned_at: Option<u64>,
    pub candidates: Vec<WorkshopItem>,
    pub preview_diagnostics: String,
    pub report: AcquisitionReport,
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<crate::application::native_acquisition::NativeReport>,
}

pub fn read<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, String> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    let mut bytes = Vec::new();
    file.take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > LIMIT {
        return Err(format!("{} exceeds 4 MiB", path.display()));
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| format!("{}: {e}", path.display()))
}

pub fn write(path: &Path, value: &impl Serialize) -> Result<(), String> {
    struct Bounded(Vec<u8>);
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.0.len().saturating_add(bytes.len()) > LIMIT as usize {
                return Err(std::io::Error::other("acquisition state exceeds 4 MiB"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut bytes = Bounded(Vec::new());
    serde_json::to_writer(&mut bytes, value).map_err(|e| e.to_string())?;
    let parent = path.parent().ok_or("state has no parent directory")?;
    if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err("Refusing linked acquisition state".into());
    }
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    file.write_all(&bytes.0).map_err(|e| e.to_string())?;
    file.as_file().sync_all().map_err(|e| e.to_string())?;
    file.persist(path).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    File::open(parent)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())?;
    Ok(())
}
