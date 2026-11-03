//! Bounded, versioned wire contract; contains no native SDK or GUI dependencies.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fmt,
    io::{BufRead, Read},
};

pub const SCHEMA: u32 = 1;
pub const APP_ID: u32 = 294100;
pub const MAX_REQUEST: usize = 1024 * 1024;
pub const MAX_LINE: usize = 256 * 1024;
pub const MAX_ITEMS: usize = 20;
pub const PREFIX: &[u8] = b"RIMSORT_STEAM_V1 ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolError(pub &'static str);
impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for ProtocolError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Inspect,
    Subscribe,
    Unsubscribe,
    Download,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub schema: u32,
    pub app_id: u32,
    pub request_id: String,
    pub action: Action,
    pub ids: Vec<u64>,
}
impl Request {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.schema != SCHEMA || self.app_id != APP_ID {
            return Err(ProtocolError("unsupported schema or app"));
        }
        if self.request_id.is_empty()
            || self.request_id.len() > 128
            || !self
                .request_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        {
            return Err(ProtocolError("invalid request identity"));
        }
        if self.ids.is_empty() || self.ids.len() > MAX_ITEMS {
            return Err(ProtocolError("expected 1 to 20 items"));
        }
        let mut seen = std::collections::BTreeSet::new();
        if self.ids.iter().any(|&id| id == 0 || !seen.insert(id)) {
            return Err(ProtocolError("zero or duplicate item"));
        }
        Ok(())
    }
}

pub fn parse_request(bytes: &[u8]) -> Result<Request, ProtocolError> {
    if bytes.len() > MAX_REQUEST {
        return Err(ProtocolError("request too large"));
    }
    let request: Request =
        serde_json::from_slice(bytes).map_err(|_| ProtocolError("invalid request JSON"))?;
    request.validate()?;
    Ok(request)
}

/// One JSON document followed by EOF. The caller must close the input pipe.
pub fn read_request(reader: impl Read) -> Result<Request, ProtocolError> {
    let mut bytes = Vec::new();
    reader
        .take((MAX_REQUEST + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| ProtocolError("request read failed"))?;
    parse_request(&bytes)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Subscription {
    Unknown,
    Subscribed,
    NotSubscribed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Freshness {
    Unknown,
    NeedsUpdate,
    Current,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Install {
    pub path: String,
    pub size_bytes: u64,
    pub timestamp: u32,
    pub freshness: Freshness,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Facts {
    pub subscription: Subscription,
    pub install: Option<Install>,
}
impl Default for Facts {
    fn default() -> Self {
        Self {
            subscription: Subscription::Unknown,
            install: None,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Before,
    After,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Observed,
    Acknowledged,
    Installed,
    Failed,
    Unknown,
}

pub const MAX_FAILURE_MESSAGE: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureStage {
    Initialize,
    Verify,
    Request,
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    InitializationFailed,
    MetadataQueryFailed,
    InvalidItemIdentity,
    RequestRejected,
    NativeFailure,
    TimedOut,
    UnverifiedInstall,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailureDetail {
    pub stage: FailureStage,
    pub kind: FailureKind,
    pub message: String,
}
impl FailureDetail {
    pub fn new(stage: FailureStage, kind: FailureKind, message: &str) -> Self {
        let mut bounded = String::with_capacity(message.len().min(MAX_FAILURE_MESSAGE));
        for ch in message.chars() {
            let ch = if invalid_message_char(ch) { ' ' } else { ch };
            if bounded.len() + ch.len_utf8() > MAX_FAILURE_MESSAGE {
                break;
            }
            bounded.push(ch);
        }
        if bounded.is_empty() {
            bounded.push_str("No diagnostic available");
        }
        Self {
            stage,
            kind,
            message: bounded,
        }
    }

    fn validate(&self) -> Result<(), ProtocolError> {
        if self.message.is_empty()
            || self.message.len() > MAX_FAILURE_MESSAGE
            || self.message.chars().any(invalid_message_char)
        {
            return Err(ProtocolError("invalid failure message"));
        }
        Ok(())
    }
}

fn invalid_message_char(ch: char) -> bool {
    ch.is_control() || matches!(ch, '\u{2028}' | '\u{2029}')
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Payload {
    Observation {
        item_id: u64,
        phase: Phase,
        facts: Facts,
    },
    Outcome {
        item_id: u64,
        outcome: Outcome,
        detail: Option<FailureDetail>,
    },
    Terminal,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Event {
    pub schema: u32,
    pub app_id: u32,
    pub request_id: String,
    pub payload: Payload,
}
impl Event {
    pub fn new(request_id: &str, payload: Payload) -> Self {
        Self {
            schema: SCHEMA,
            app_id: APP_ID,
            request_id: request_id.into(),
            payload,
        }
    }
}

pub fn encode_event(event: &Event) -> Result<Vec<u8>, ProtocolError> {
    let mut line = PREFIX.to_vec();
    serde_json::to_writer(&mut line, event).map_err(|_| ProtocolError("event encoding failed"))?;
    line.push(b'\n');
    if line.len() > MAX_LINE {
        return Err(ProtocolError("event too large"));
    }
    Ok(line)
}

/// Includes the newline in the limit; does not allocate an unbounded diagnostic line.
pub fn read_line_bounded(reader: &mut impl BufRead) -> Result<Option<Vec<u8>>, ProtocolError> {
    let mut line = Vec::new();
    reader
        .take((MAX_LINE + 1) as u64)
        .read_until(b'\n', &mut line)
        .map_err(|_| ProtocolError("event read failed"))?;
    if line.len() > MAX_LINE {
        return Err(ProtocolError("line too large"));
    }
    Ok((!line.is_empty()).then_some(line))
}

pub struct EventStream {
    request: Request,
    stages: BTreeMap<u64, u8>,
    events: Vec<Event>,
    terminal: bool,
}
impl EventStream {
    pub fn new(request: Request) -> Result<Self, ProtocolError> {
        request.validate()?;
        let stages = request.ids.iter().map(|&id| (id, 0)).collect();
        Ok(Self {
            request,
            stages,
            events: Vec::new(),
            terminal: false,
        })
    }
    pub fn accept_line(&mut self, line: &[u8]) -> Result<Option<Event>, ProtocolError> {
        if line.len() > MAX_LINE {
            return Err(ProtocolError("line too large"));
        }
        let Some(json) = line.strip_prefix(PREFIX) else {
            return Ok(None);
        };
        let event: Event =
            serde_json::from_slice(json).map_err(|_| ProtocolError("invalid event JSON"))?;
        if self.terminal
            || event.schema != SCHEMA
            || event.app_id != APP_ID
            || event.request_id != self.request.request_id
        {
            return Err(ProtocolError("late event or wrong identity"));
        }
        match &event.payload {
            Payload::Terminal => {
                if self.stages.values().any(|&stage| stage != 3) {
                    return Err(ProtocolError("premature terminal"));
                }
                self.terminal = true;
            }
            Payload::Observation {
                item_id,
                phase,
                facts,
            } => {
                if let Some(install) = &facts.install
                    && (install.path.is_empty()
                        || install.path.len() > 4096
                        || install.path.chars().any(char::is_control)
                        || !std::path::Path::new(&install.path).is_absolute())
                {
                    return Err(ProtocolError("invalid install path"));
                }
                let expected = match phase {
                    Phase::Before => 0,
                    Phase::After => 1,
                };
                self.advance(*item_id, expected)?;
            }
            Payload::Outcome {
                item_id,
                outcome,
                detail,
            } => {
                if matches!(outcome, Outcome::Failed | Outcome::Unknown) != detail.is_some() {
                    return Err(ProtocolError("missing or forbidden failure detail"));
                }
                if let Some(detail) = detail {
                    detail.validate()?;
                }
                let compatible = match outcome {
                    Outcome::Observed => self.request.action == Action::Inspect,
                    Outcome::Acknowledged => matches!(self.request.action, Action::Subscribe | Action::Unsubscribe),
                    Outcome::Installed => self.request.action == Action::Download && self.events.iter().any(|e|
                        matches!(&e.payload, Payload::Observation { item_id: id, phase: Phase::After,
                            facts: Facts { install: Some(Install { freshness: Freshness::Current, .. }), .. } } if id == item_id)),
                    Outcome::Failed | Outcome::Unknown => true,
                };
                if !compatible {
                    return Err(ProtocolError("outcome contradicts action or install"));
                }
                self.advance(*item_id, 2)?;
            }
        }
        self.events.push(event.clone());
        Ok(Some(event))
    }
    fn advance(&mut self, item_id: u64, expected: u8) -> Result<(), ProtocolError> {
        let stage = self
            .stages
            .get_mut(&item_id)
            .ok_or(ProtocolError("foreign item"))?;
        if *stage != expected {
            return Err(ProtocolError("duplicate or out-of-order event"));
        }
        *stage += 1;
        Ok(())
    }
    pub fn events(&self) -> &[Event] {
        &self.events
    }
    pub fn finish(&self) -> Result<(), ProtocolError> {
        if self.terminal {
            Ok(())
        } else {
            Err(ProtocolError("EOF without terminal event"))
        }
    }
}
