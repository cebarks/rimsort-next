//! Compatible local list files. Imports never choose a fallback active list.
use crate::integrations::{config::parse_config, xml};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModListDocument {
    #[serde(rename = "activeMods")]
    pub tokens: Vec<String>,
    #[serde(rename = "version")]
    pub game_version: Option<String>,
    #[serde(rename = "knownExpansions", default)]
    pub known_expansions: Vec<String>,
}
#[derive(Debug, thiserror::Error)]
#[error("list file: {0}")]
pub struct ListError(pub String);
fn error(error: impl std::fmt::Display) -> ListError {
    ListError(error.to_string())
}

pub fn parse_list(bytes: &[u8]) -> Result<ModListDocument, ListError> {
    if bytes.len() > 32 * 1024 * 1024 {
        return Err(error("document exceeds 32 MiB"));
    }
    let content = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    if content.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'{') {
        return serde_json::from_slice(content).map_err(error);
    }
    let config = parse_config(bytes).map_err(error)?;
    let root = xml::parse_document(bytes).map_err(error)?;
    if root.children_named("knownExpansions").count() > 1 {
        return Err(error("duplicate knownExpansions"));
    }
    let mut expansions = Vec::new();
    if let Some(container) = root.child("knownExpansions") {
        if !container.text().is_empty() {
            return Err(error("text outside expansion entries"));
        }
        for child in container.children() {
            if !child.has_name("li") || !child.children().is_empty() {
                return Err(error("invalid knownExpansions entry"));
            }
            expansions.push(child.text().to_string());
        }
    }
    Ok(ModListDocument {
        tokens: config.active_tokens,
        game_version: config.version,
        known_expansions: expansions,
    })
}
pub fn read_list(path: &Path) -> Result<ModListDocument, ListError> {
    let file = fs::File::open(path).map_err(error)?;
    let mut bytes = Vec::new();
    file.take(32 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(error)?;
    parse_list(&bytes)
}
pub fn export_json(doc: &ModListDocument) -> Result<Vec<u8>, ListError> {
    serde_json::to_vec_pretty(doc).map_err(error)
}
pub fn export_xml(doc: &ModListDocument) -> Result<Vec<u8>, ListError> {
    let mut text = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<ModsConfigData>\n");
    if let Some(version) = &doc.game_version {
        text.push_str(&format!(
            "  <version>{}</version>\n",
            quick_xml::escape::escape(version)
        ));
    }
    for (name, tokens) in [
        ("activeMods", &doc.tokens),
        ("knownExpansions", &doc.known_expansions),
    ] {
        text.push_str(&format!("  <{name}>\n"));
        for token in tokens {
            text.push_str(&format!(
                "    <li>{}</li>\n",
                quick_xml::escape::escape(token)
            ));
        }
        text.push_str(&format!("  </{name}>\n"));
    }
    text.push_str("</ModsConfigData>\n");
    // The same strict reader catches unencodable XML characters before anything is written.
    parse_list(text.as_bytes())?;
    Ok(text.into_bytes())
}
#[must_use]
pub fn append_unique(current: &[String], incoming: &[String]) -> Vec<String> {
    let mut result = current.to_vec();
    let mut seen: HashSet<String> = current.iter().map(|t| t.trim().to_lowercase()).collect();
    for token in incoming {
        if seen.insert(token.trim().to_lowercase()) {
            result.push(token.clone());
        }
    }
    result
}
// Resolve parents even when the leaf is absent. Never re-follow the caller's
// directory-link spelling when creating the temporary file or persisting it.
fn export_destination(path: &Path) -> Result<(PathBuf, bool), ListError> {
    let name = path.file_name().ok_or_else(|| error("missing file name"))?;
    #[cfg(windows)]
    {
        let name = name
            .to_str()
            .ok_or_else(|| error("ambiguous Windows file name"))?;
        let stem = name
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        if name.ends_with(['.', ' '])
            || name
                .chars()
                .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
            || matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ((stem.starts_with("COM") || stem.starts_with("LPT"))
                && stem.len() == 4
                && stem.as_bytes()[3].is_ascii_digit())
        {
            return Err(error("ambiguous Windows file name"));
        }
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let destination = fs::canonicalize(parent).map_err(error)?.join(name);
    let exists = match fs::symlink_metadata(&destination) {
        Ok(metadata) if metadata.is_file() => true,
        Ok(_) => {
            return Err(error(
                "export identities must be regular files, not links or directories",
            ));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => return Err(error(e)),
    };
    Ok((destination, exists))
}

fn same_export_name(first: &Path, second: &Path) -> Result<bool, ListError> {
    let first = first
        .file_name()
        .ok_or_else(|| error("missing file name"))?;
    let second = second
        .file_name()
        .ok_or_else(|| error("missing file name"))?;
    #[cfg(any(windows, target_os = "macos"))]
    {
        // Missing files have no OS identity to compare. Do not guess at Unicode
        // case/normalization aliases on case-insensitive filesystems.
        let first = first.to_str().filter(|s| s.is_ascii());
        let second = second.to_str().filter(|s| s.is_ascii());
        match (first, second) {
            (Some(first), Some(second)) => {
                if cfg!(windows) && (first.contains('~') || second.contains('~')) {
                    return Err(error("cannot prove distinct missing Windows short names"));
                }
                Ok(first.eq_ignore_ascii_case(second))
            }
            _ => Err(error("cannot prove distinct missing-file names")),
        }
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    Ok(first == second)
}

/// Refuses file identities and missing-file parent/name aliases of the game config.
/// Unresolvable parents and final-file links fail closed; safe exports still work
/// when the configured file is absent. Writes use the resolved destination.
/// Missing-file names in the same directory must have provably distinct identities:
/// Windows/macOS refuse non-ASCII ambiguity, and Windows also refuses short names.
pub fn export_file(
    path: &Path,
    game_config: &Path,
    doc: &ModListDocument,
    json: bool,
) -> Result<(), ListError> {
    let (destination, destination_exists) = export_destination(path)?;
    let (protected, protected_exists) = export_destination(game_config)?;
    let same = if destination_exists && protected_exists {
        same_file::is_same_file(&destination, &protected).map_err(error)?
    } else {
        same_file::is_same_file(
            destination
                .parent()
                .ok_or_else(|| error("missing destination directory"))?,
            protected
                .parent()
                .ok_or_else(|| error("missing game directory"))?,
        )
        .map_err(error)?
            && same_export_name(&destination, &protected)?
    };
    if same {
        return Err(error(
            "export cannot overwrite the game config; use Apply to game",
        ));
    }
    let bytes = if json {
        export_json(doc)?
    } else {
        export_xml(doc)?
    };
    let parent = destination
        .parent()
        .ok_or_else(|| error("missing destination directory"))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(error)?;
    temp.write_all(&bytes).map_err(error)?;
    temp.as_file().sync_all().map_err(error)?;
    temp.persist(&destination).map_err(error)?;
    Ok(())
}
