//! Public Workshop identity and bounded metadata resolution; never fetch pasted URLs.
#[cfg(test)]
#[path = "../../tests/support/workshop_http.rs"]
mod tests;
use crate::application::acquisition::AcquisitionError;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    io::Read,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub struct PublishedFileId(u64);

impl PublishedFileId {
    pub fn get(self) -> u64 {
        self.0
    }
}
impl TryFrom<u64> for PublishedFileId {
    type Error = AcquisitionError;
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        if value == 0 {
            Err(AcquisitionError::Invalid(
                "Workshop ID must be positive".into(),
            ))
        } else {
            Ok(Self(value))
        }
    }
}
impl From<PublishedFileId> for u64 {
    fn from(value: PublishedFileId) -> Self {
        value.0
    }
}

pub fn parse_id(input: &str) -> Result<PublishedFileId, AcquisitionError> {
    let invalid = || {
        AcquisitionError::Invalid(
            "expected a positive decimal ID or canonical Steam Community filedetails URL".into(),
        )
    };
    let digits = if input.bytes().all(|b| b.is_ascii_digit()) {
        input
    } else {
        let url = url::Url::parse(input).map_err(|_| invalid())?;
        if url.scheme() != "https"
            || url.host_str() != Some("steamcommunity.com")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port().is_some()
            || url.fragment().is_some()
            || !matches!(
                url.path(),
                "/sharedfiles/filedetails/" | "/workshop/filedetails/"
            )
        {
            return Err(invalid());
        }
        let prefix = format!("https://steamcommunity.com{}?id=", url.path());
        input.strip_prefix(&prefix).ok_or_else(invalid)?
    };
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    PublishedFileId::try_from(digits.parse::<u64>().map_err(|_| invalid())?)
}

pub const RIMWORLD_APP_ID: u64 = 294100;
const MAX_ITEMS: usize = 10_000;
const MAX_RESPONSE: u64 = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkshopKind {
    ItemCandidate,
    Collection,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkshopItem {
    pub id: PublishedFileId,
    pub title: String,
    pub app_id: u64,
    pub kind: WorkshopKind,
    pub upstream_updated: Option<u64>,
}
#[derive(Clone, Debug)]
pub struct ResolutionDiagnostic {
    pub item_id: PublishedFileId,
    pub message: String,
}
#[derive(Clone, Debug, Default)]
pub struct WorkshopPreview {
    pub items: Vec<WorkshopItem>,
    pub collections: Vec<WorkshopItem>,
    pub diagnostics: Vec<ResolutionDiagnostic>,
}

pub struct WorkshopClient {
    agent: ureq::Agent,
    base: String,
}
impl Default for WorkshopClient {
    fn default() -> Self {
        Self::new()
    }
}
impl WorkshopClient {
    pub fn new() -> Self {
        Self::at("https://api.steampowered.com")
    }
    pub(crate) fn at(base: &str) -> Self {
        Self {
            base: base.into(),
            agent: ureq::Agent::config_builder()
                .timeout_global(Some(Duration::from_secs(15)))
                .max_redirects(0)
                .http_status_as_error(false)
                .build()
                .into(),
        }
    }
    pub fn resolve(&self, ids: &[PublishedFileId]) -> WorkshopPreview {
        self.resolve_cancellable(ids, &AtomicBool::new(false))
            .unwrap_or_default()
    }
    pub(crate) fn resolve_cancellable(
        &self,
        ids: &[PublishedFileId],
        cancel: &AtomicBool,
    ) -> Option<WorkshopPreview> {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let mut resolution = Resolution {
            client: self,
            cancel,
            preview: WorkshopPreview::default(),
            cache: HashMap::new(),
            visited: HashSet::new(),
        };
        if ids.len() > MAX_ITEMS {
            if let Some(id) = ids.first() {
                resolution.error(*id, "selection exceeds 10000 IDs");
            }
            return Some(resolution.preview);
        }
        resolution.load(ids);
        for id in ids {
            resolution.visit(*id, &mut Vec::new());
        }
        (!cancel.load(Ordering::Relaxed)).then_some(resolution.preview)
    }
    fn request(
        &self,
        method: &str,
        ids: &[PublishedFileId],
        cancel: &AtomicBool,
    ) -> Result<serde_json::Value, String> {
        let count = if method == "GetPublishedFileDetails" {
            "itemcount"
        } else {
            "collectioncount"
        };
        let mut form = vec![(count.to_string(), ids.len().to_string())];
        form.extend(
            ids.iter()
                .enumerate()
                .map(|(i, id)| (format!("publishedfileids[{i}]"), id.get().to_string())),
        );
        for attempt in 0..=3 {
            if cancel.load(Ordering::Relaxed) {
                return Err("Public preview cancelled".into());
            }
            let mut response = self
                .agent
                .post(format!("{}/ISteamRemoteStorage/{method}/v1/", self.base))
                .send_form(form.iter().map(|(key, value)| (key, value)))
                .map_err(|e| e.to_string())?;
            if cancel.load(Ordering::Relaxed) {
                return Err("Public preview cancelled".into());
            }
            let status = response.status().as_u16();
            if matches!(status, 429 | 500 | 502 | 503 | 504) && attempt < 3 {
                std::thread::sleep(Duration::from_millis(50 << attempt));
                continue;
            }
            if status != 200 {
                return Err(format!("{method}: HTTP {status}"));
            }
            let mut bytes = Vec::new();
            response
                .body_mut()
                .as_reader()
                .take(MAX_RESPONSE + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            if cancel.load(Ordering::Relaxed) {
                return Err("Public preview cancelled".into());
            }
            if bytes.len() as u64 > MAX_RESPONSE {
                return Err("response exceeds 4 MiB".into());
            }
            return serde_json::from_slice(&bytes).map_err(|e| format!("invalid API JSON: {e}"));
        }
        unreachable!("bounded request loop returns on last attempt")
    }
}

#[derive(Clone)]
struct Resolved {
    item: WorkshopItem,
    children: Vec<(PublishedFileId, bool)>,
}
struct Resolution<'a> {
    client: &'a WorkshopClient,
    cancel: &'a AtomicBool,
    preview: WorkshopPreview,
    cache: HashMap<PublishedFileId, Result<Resolved, String>>,
    visited: HashSet<PublishedFileId>,
}
impl Resolution<'_> {
    fn error(&mut self, id: PublishedFileId, message: impl Into<String>) {
        self.preview.diagnostics.push(ResolutionDiagnostic {
            item_id: id,
            message: message.into(),
        });
    }
    fn load(&mut self, ids: &[PublishedFileId]) -> bool {
        if self.cancel.load(Ordering::Relaxed) {
            return false;
        }
        let mut seen = HashSet::new();
        let missing: Vec<_> = ids
            .iter()
            .copied()
            .filter(|id| !self.cache.contains_key(id) && seen.insert(*id))
            .collect();
        if self.cache.len() + missing.len() > MAX_ITEMS {
            if let Some(id) = missing.first() {
                self.error(*id, "collection expansion exceeds 10000 unique IDs; this entire expansion was refused");
            }
            return false;
        }
        for batch in missing.chunks(100) {
            if self.cancel.load(Ordering::Relaxed) {
                return false;
            }
            let details = self
                .client
                .request("GetPublishedFileDetails", batch, self.cancel)
                .and_then(|v| rows(v, "publishedfiledetails", batch));
            let collections = self
                .client
                .request("GetCollectionDetails", batch, self.cancel)
                .and_then(|v| rows(v, "collectiondetails", batch));
            for id in batch {
                let resolved = match (&details, &collections) {
                    (Ok(d), Ok(c)) => resolve_row(*id, d.get(id), c.get(id)),
                    (Err(e), _) | (_, Err(e)) => Err(e.clone()),
                };
                self.cache.insert(*id, resolved);
            }
        }
        true
    }
    fn visit(&mut self, id: PublishedFileId, ancestors: &mut Vec<PublishedFileId>) {
        if self.cancel.load(Ordering::Relaxed) {
            return;
        }
        if ancestors.contains(&id) {
            self.error(id, "collection cycle detected");
            return;
        }
        if ancestors.len() > 8 {
            self.error(id, "collection depth exceeds 8");
            return;
        }
        if !self.visited.insert(id) {
            return;
        }
        let Some(result) = self.cache.get(&id).cloned() else {
            return;
        };
        let resolved = match result {
            Ok(r) => r,
            Err(e) => {
                self.error(id, e);
                return;
            }
        };
        if resolved.item.kind == WorkshopKind::ItemCandidate {
            self.preview.items.push(resolved.item);
            return;
        }
        self.preview.collections.push(resolved.item);
        ancestors.push(id);
        if !self.load(
            &resolved
                .children
                .iter()
                .map(|(id, _)| *id)
                .collect::<Vec<_>>(),
        ) {
            ancestors.pop();
            return;
        }
        for (child, collection_expected) in resolved.children {
            if collection_expected
                && self.cache.get(&child).is_some_and(|r| {
                    r.as_ref()
                        .is_ok_and(|r| r.item.kind != WorkshopKind::Collection)
                })
            {
                self.error(
                    child,
                    "known collection child has no usable collection details",
                );
            } else {
                self.visit(child, ancestors);
            }
        }
        ancestors.pop();
    }
}

fn rows(
    value: serde_json::Value,
    field: &str,
    requested: &[PublishedFileId],
) -> Result<HashMap<PublishedFileId, serde_json::Value>, String> {
    let rows = value
        .get("response")
        .and_then(|v| v.get(field))
        .and_then(|v| v.as_array())
        .ok_or_else(|| format!("missing {field} rows"))?;
    let mut map = HashMap::new();
    for row in rows {
        let id = row
            .get("publishedfileid")
            .and_then(|v| v.as_str())
            .and_then(|v| parse_id(v).ok())
            .ok_or("invalid row ID")?;
        if !requested.contains(&id) || map.insert(id, row.clone()).is_some() {
            return Err("uncorrelated or duplicate response ID".into());
        }
    }
    Ok(map)
}
fn resolve_row(
    id: PublishedFileId,
    details: Option<&serde_json::Value>,
    collection: Option<&serde_json::Value>,
) -> Result<Resolved, String> {
    let d = details.ok_or("missing public item details")?;
    if d.get("result").and_then(|v| v.as_u64()) != Some(1) {
        return Err("item is missing, private or inaccessible".into());
    }
    if d.get("consumer_app_id").and_then(|v| v.as_u64()) != Some(RIMWORLD_APP_ID) {
        return Err("item belongs to another or unknown app".into());
    }
    let c = collection.ok_or("missing collection result")?;
    let kind = match c.get("result").and_then(|v| v.as_u64()) {
        Some(1) => WorkshopKind::Collection,
        Some(9) => WorkshopKind::ItemCandidate,
        other => return Err(format!("collection lookup failed: {other:?}")),
    };
    let mut children = Vec::new();
    if kind == WorkshopKind::Collection {
        let rows = c
            .get("children")
            .and_then(|v| v.as_array())
            .ok_or("collection children unavailable")?;
        if rows.len() > MAX_ITEMS {
            return Err("collection exceeds 10000 children".into());
        }
        for row in rows {
            children.push((
                row.get("publishedfileid")
                    .and_then(|v| v.as_str())
                    .and_then(|v| parse_id(v).ok())
                    .ok_or("invalid collection child ID")?,
                row.get("filetype").and_then(|v| v.as_u64()) == Some(2),
            ));
        }
    }
    Ok(Resolved {
        item: WorkshopItem {
            id,
            title: d
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            app_id: RIMWORLD_APP_ID,
            kind,
            upstream_updated: d.get("time_updated").and_then(|v| v.as_u64()),
        },
        children,
    })
}
