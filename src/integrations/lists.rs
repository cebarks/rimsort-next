//! Compatible local list files. Imports never choose a fallback active list.
use crate::integrations::{config::parse_config, xml};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    io::{Read, Write},
    path::Path,
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
/// Refuses lexical and canonical aliases of the bound game config before atomic export.
pub fn export_file(
    path: &Path,
    game_config: &Path,
    doc: &ModListDocument,
    json: bool,
) -> Result<(), ListError> {
    if path == game_config
        || (path.exists()
            && fs::canonicalize(path).map_err(error)?
                == fs::canonicalize(game_config).map_err(error)?)
    {
        return Err(error(
            "export cannot overwrite the game config; use Apply to game",
        ));
    }
    let bytes = if json {
        export_json(doc)?
    } else {
        export_xml(doc)?
    };
    let parent = path
        .parent()
        .ok_or_else(|| error("missing destination directory"))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(error)?;
    temp.write_all(&bytes).map_err(error)?;
    temp.as_file().sync_all().map_err(error)?;
    temp.persist(path).map_err(error)?;
    Ok(())
}
