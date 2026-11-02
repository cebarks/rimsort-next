//! Explicit, bounded HTTPS updates and validated last-known-good community cache.
use crate::domain::{
    rules::{ModRules, RuleEdge, Ruleset},
    types::PackageId,
};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

pub const SOURCE: &str =
    "https://raw.githubusercontent.com/RimSort/Community-Rules-Database/main/communityRules.json";
const MAX_BYTES: u64 = 4 * 1024 * 1024;
#[derive(Debug, thiserror::Error)]
#[error("community rules: {0}")]
pub struct RulesError(String);
fn failure(error: impl std::fmt::Display) -> RulesError {
    RulesError(error.to_string())
}
fn object<'a>(value: &'a Value, context: &str) -> Result<&'a Map<String, Value>, RulesError> {
    value
        .as_object()
        .ok_or_else(|| failure(format!("{context} must be an object")))
}
fn text_list(value: Option<&Value>, context: &str) -> Result<Vec<String>, RulesError> {
    match value {
        None => Ok(Vec::new()),
        Some(Value::String(text)) => Ok(vec![text.clone()]),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| failure(format!("{context} must contain strings")))
            })
            .collect(),
        _ => Err(failure(format!(
            "{context} must be a string or string list"
        ))),
    }
}
fn id(text: &str) -> Result<PackageId, RulesError> {
    PackageId::normalize(text).ok_or_else(|| failure("empty package ID"))
}
fn edges(value: Option<&Value>, context: &str) -> Result<Vec<RuleEdge>, RulesError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let mut seen = BTreeMap::new();
    for (target, value) in object(value, context)? {
        let detail = object(value, context)?;
        text_list(detail.get("name"), "name")?;
        let edge = RuleEdge {
            target: id(target)?,
            comments: text_list(detail.get("comment"), "comment")?,
        };
        if seen.insert(edge.target.clone(), edge).is_some() {
            return Err(failure(format!("duplicate normalized target in {context}")));
        }
    }
    Ok(seen.into_values().collect())
}
fn flag(value: Option<&Value>, context: &str) -> Result<(bool, Vec<String>), RulesError> {
    let Some(value) = value else {
        return Ok((false, Vec::new()));
    };
    let map = object(value, context)?;
    let value = map
        .get("value")
        .and_then(Value::as_bool)
        .ok_or_else(|| failure(format!("{context}.value must be a boolean")))?;
    text_list(map.get("name"), "name")?;
    Ok((value, text_list(map.get("comment"), "comment")?))
}

pub fn parse_rules(bytes: &[u8]) -> Result<Ruleset, RulesError> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err(failure("database exceeds 4 MiB"));
    }
    let UniqueValue(value) = serde_json::from_slice(bytes).map_err(failure)?;
    let root = object(&value, "database")?;
    let timestamp = root
        .get("timestamp")
        .map(|v| {
            v.as_u64()
                .ok_or_else(|| failure("timestamp must be a nonnegative integer"))
        })
        .transpose()?;
    let rules = object(
        root.get("rules").ok_or_else(|| failure("missing rules"))?,
        "rules",
    )?;
    let mut parsed = BTreeMap::new();
    for (package, value) in rules {
        let map = object(value, package)?;
        text_list(map.get("name"), "name")?;
        text_list(map.get("comment"), "comment")?;
        let (top, top_comments) = flag(map.get("loadTop"), "loadTop")?;
        let (bottom, bottom_comments) = flag(map.get("loadBottom"), "loadBottom")?;
        let entry = ModRules {
            before: edges(map.get("loadBefore"), "loadBefore")?,
            after: edges(map.get("loadAfter"), "loadAfter")?,
            top,
            bottom,
            top_comments,
            bottom_comments,
        };
        if parsed.insert(id(package)?, entry).is_some() {
            return Err(failure(format!("duplicate normalized package {package}")));
        }
    }
    Ok(Ruleset {
        timestamp,
        rules: parsed,
    })
}

// serde_json::Value otherwise overwrites duplicate keys before schema validation can
// see them, silently dropping constraints from a damaged upstream document.
struct UniqueValue(Value);
impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = UniqueValue;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("JSON with unique object keys")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> Result<Self::Value, M::Error> {
                let mut object = Map::new();
                while let Some((key, UniqueValue(value))) =
                    map.next_entry::<String, UniqueValue>()?
                {
                    if object.insert(key.clone(), value).is_some() {
                        return Err(serde::de::Error::custom(format!(
                            "duplicate JSON key {key}"
                        )));
                    }
                }
                Ok(UniqueValue(Value::Object(object)))
            }
            fn visit_seq<S: serde::de::SeqAccess<'de>>(
                self,
                mut seq: S,
            ) -> Result<Self::Value, S::Error> {
                let mut values = Vec::new();
                while let Some(UniqueValue(value)) = seq.next_element()? {
                    values.push(value);
                }
                Ok(UniqueValue(Value::Array(values)))
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::String(v.into())))
            }
            fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::Bool(v)))
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::Number(v.into())))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::Number(v.into())))
            }
            fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Self::Value, E> {
                serde_json::Number::from_f64(v)
                    .map(|n| UniqueValue(Value::Number(n)))
                    .ok_or_else(|| E::custom("nonfinite JSON number"))
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Self::Value, E> {
                Ok(UniqueValue(Value::Null))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

#[derive(Clone, Debug)]
pub struct CommunityStore {
    root: PathBuf,
    transport: Transport,
}
impl CommunityStore {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            transport: Transport::production(),
        }
    }
    fn cache_path(&self) -> PathBuf {
        self.root.join("community-rules.json")
    }
    pub fn load(&self) -> Result<Option<Ruleset>, RulesError> {
        let file = match fs::File::open(self.cache_path()) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(failure(e)),
        };
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(failure)?;
        parse_rules(&bytes).map(Some)
    }
    pub fn enabled(&self) -> Result<bool, RulesError> {
        let bytes = match fs::read(self.root.join("community-settings.json")) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(true),
            Err(e) => return Err(failure(e)),
        };
        let value: Value = serde_json::from_slice(&bytes).map_err(failure)?;
        if value.get("schema_version").and_then(Value::as_u64) != Some(1) {
            return Err(failure("unsupported community settings schema"));
        }
        value
            .get("enabled")
            .and_then(Value::as_bool)
            .ok_or_else(|| failure("enabled must be boolean"))
    }
    pub fn set_enabled(&self, enabled: bool) -> Result<(), RulesError> {
        atomic_write(
            &self.root.join("community-settings.json"),
            format!("{{\"schema_version\":1,\"enabled\":{enabled}}}\n").as_bytes(),
        )
    }
    /// The only production endpoint is the fixed HTTPS source. Loading never calls this.
    pub fn update(&self) -> Result<Ruleset, RulesError> {
        self.update_using(&self.transport)
    }
    fn update_using(&self, transport: &Transport) -> Result<Ruleset, RulesError> {
        let mut response = transport
            .agent()
            .get(&transport.url)
            .call()
            .map_err(failure)?;
        if response.status().as_u16() != 200 {
            return Err(failure(format!(
                "unexpected HTTP status {}",
                response.status()
            )));
        }
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(failure)?;
        let rules = parse_rules(&bytes)?;
        atomic_write(&self.cache_path(), &bytes)?;
        Ok(rules)
    }
}
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), RulesError> {
    let parent = path
        .parent()
        .ok_or_else(|| failure("missing application data root"))?;
    fs::create_dir_all(parent).map_err(failure)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(failure)?;
    temp.write_all(bytes).map_err(failure)?;
    temp.as_file().sync_all().map_err(failure)?;
    temp.persist(path).map_err(failure)?;
    Ok(())
}
#[derive(Clone, Debug)]
struct Transport {
    url: String,
    https_only: bool,
    connect: Duration,
    total: Duration,
}
impl Transport {
    fn production() -> Self {
        Self {
            url: SOURCE.into(),
            https_only: true,
            connect: Duration::from_secs(10),
            total: Duration::from_secs(60),
        }
    }
    fn agent(&self) -> ureq::Agent {
        ureq::Agent::config_builder()
            .https_only(self.https_only)
            .max_redirects(0)
            .timeout_connect(Some(self.connect))
            .timeout_global(Some(self.total))
            .build()
            .into()
    }
}
#[cfg(test)]
#[path = "../../tests/support/community_http.rs"]
pub(crate) mod http_tests;
