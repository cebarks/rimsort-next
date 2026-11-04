//! Native receipts retain SDK evidence separately from usable filesystem observations.
use super::acquisition::*;
use crate::{
    domain::types::GameVersion,
    integrations::{
        install::validate_about,
        steam_client::{self, RunStatus, SteamClientConfig},
        workshop::PublishedFileId,
    },
};
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};
use steam_protocol::{
    Action, Event, Facts, FailureDetail, Outcome, Payload, Phase, Request, Subscription,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NativeItem {
    pub item_id: u64,
    pub request_id: String,
    pub before: Option<Facts>,
    pub after: Option<Facts>,
    pub acknowledgement: bool,
    pub outcome: Option<Outcome>,
    pub failure: Option<FailureDetail>,
    pub validation_errors: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PendingEffects {
    pub request_id: String,
    pub action: Action,
    pub items: Vec<NativeItem>,
    #[serde(default)]
    pub runs: Vec<NativeRun>,
    pub resolved_by: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NativeRun {
    pub request_id: String,
    pub status: String,
    pub protocol_complete: bool,
    pub protocol_error: Option<String>,
    pub error: Option<String>,
    pub input_error: Option<String>,
    pub exit_status: Option<String>,
    pub spawned: bool,
    pub leader_reaped: bool,
    pub stdin_closed: bool,
    pub stdout_eof: bool,
    pub stderr_eof: bool,
    pub termination_requested: bool,
    pub termination_error: Option<String>,
    pub detached_workers: usize,
    pub cleanup_uncertain: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NativeReport {
    pub action: Action,
    pub runtime_dir: Option<PathBuf>,
    pub items: Vec<NativeItem>,
    pub runs: Vec<NativeRun>,
    pub pending: Vec<PendingEffects>,
    pub diagnostics: Vec<String>,
    pub dropped_lines: usize,
    pub dropped_bytes: usize,
}
impl NativeReport {
    pub fn new(action: Action, runtime_dir: Option<PathBuf>) -> Self {
        Self {
            action,
            runtime_dir,
            items: Vec::new(),
            runs: Vec::new(),
            pending: Vec::new(),
            diagnostics: Vec::new(),
            dropped_lines: 0,
            dropped_bytes: 0,
        }
    }
    pub fn uncertain(&self) -> bool {
        self.pending.iter().any(|p| p.resolved_by.is_none())
    }
    pub fn event(&mut self, event: &Event) {
        let id = match &event.payload {
            Payload::Observation { item_id, .. } | Payload::Outcome { item_id, .. } => *item_id,
            Payload::Terminal => return,
        };
        let Some(item) = self.items.iter_mut().find(|i| i.item_id == id) else {
            return;
        };
        match &event.payload {
            Payload::Observation {
                phase: Phase::Before,
                facts,
                ..
            } => item.before = Some(facts.clone()),
            Payload::Observation {
                phase: Phase::After,
                facts,
                ..
            } => item.after = Some(facts.clone()),
            Payload::Outcome {
                outcome, detail, ..
            } => {
                item.acknowledgement |= *outcome == Outcome::Acknowledged;
                item.outcome = Some(*outcome);
                item.failure = detail.clone();
            }
            Payload::Terminal => (),
        }
    }
}

pub(crate) fn validate_item_path(path: &Path, id: u64) -> Result<(), String> {
    if let Some(folder_id) = path
        .file_name()
        .and_then(|s| s.to_str())
        .and_then(|s| s.parse::<u64>().ok())
        && folder_id != id
    {
        return Err("native installation folder belongs to a different item".into());
    }
    let file = match std::fs::File::open(path.join("About/PublishedFileId.txt")) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.to_string()),
    };
    let mut bytes = Vec::new();
    file.take(65)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 64
        || std::str::from_utf8(&bytes)
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            != Some(id)
    {
        return Err("PublishedFileId does not match the native item".into());
    }
    Ok(())
}

/// The caller owns the queue, confirmation, post-return persistence, and launch interlock.
/// Observer receives only accepted protocol events and must return promptly.
pub fn run(
    config: &SteamClientConfig,
    request_id: &str,
    ids: &[u64],
    version: &GameVersion,
    report: &mut NativeReport,
    cancel: &AtomicBool,
    observer: &mut dyn FnMut(&Event),
) -> (AcquisitionReport, Option<String>) {
    report.items = ids
        .iter()
        .enumerate()
        .map(|(index, &id)| NativeItem {
            item_id: id,
            request_id: format!("{request_id}-{}", index / steam_protocol::MAX_ITEMS),
            before: None,
            after: None,
            acknowledgement: false,
            outcome: None,
            failure: None,
            validation_errors: Vec::new(),
        })
        .collect();
    let mut result = AcquisitionReport::default();
    let mut error = None;
    let mut possible_effects = false;
    let mut uncertain = false;
    let mut diagnostic_bytes = 0;
    for (index, ids) in ids.chunks(steam_protocol::MAX_ITEMS).enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let request = Request {
            schema: 1,
            app_id: 294100,
            request_id: format!("{request_id}-{index}"),
            action: report.action,
            ids: ids.to_vec(),
        };
        let returned = steam_client::run_request(
            config,
            &request,
            cancel,
            Some(&mut |event| {
                report.event(event);
                observer(event);
            }),
        );
        let returned = match returned {
            Ok(r) => r,
            Err(e) => {
                error = Some(e.to_string());
                break;
            }
        };
        possible_effects |= returned.cleanup.spawned;
        uncertain |= returned.cleanup.spawned && returned.status != RunStatus::Completed;
        if returned.cleanup.uncertain() {
            result.cleanup_error =
                Some("Native helper cleanup is uncertain; acknowledgement cannot clear it".into());
        }
        if returned.status != RunStatus::Completed {
            error = Some(format!(
                "Native {:?}: {}",
                returned.status,
                returned.error.as_deref().unwrap_or("incomplete request")
            ));
        }
        report.dropped_lines += returned.dropped_diagnostic_lines;
        report.dropped_bytes += returned.dropped_diagnostic_bytes;
        for d in &returned.diagnostics {
            let line = format!("{:?}: {}", d.source, d.text);
            if report.diagnostics.len() < 1000 && diagnostic_bytes + line.len() < 256 * 1024 {
                diagnostic_bytes += line.len() + 1;
                report.diagnostics.push(line);
            } else {
                report.dropped_lines += 1;
                report.dropped_bytes += line.len() + 1;
            }
        }
        report.runs.push(NativeRun {
            request_id: request.request_id,
            status: format!("{:?}", returned.status),
            protocol_complete: returned.protocol_complete,
            protocol_error: returned.protocol_error,
            error: returned.error,
            input_error: returned.input_error,
            exit_status: returned.exit_status.map(|s| s.to_string()),
            spawned: returned.cleanup.spawned,
            leader_reaped: returned.cleanup.leader_reaped,
            stdin_closed: returned.cleanup.stdin_closed,
            stdout_eof: returned.cleanup.stdout_eof,
            stderr_eof: returned.cleanup.stderr_eof,
            termination_requested: returned.cleanup.termination_requested,
            termination_error: returned.cleanup.termination_error.clone(),
            detached_workers: returned.cleanup.detached_workers,
            cleanup_uncertain: returned.cleanup.uncertain(),
        });
        if returned.status != RunStatus::Completed || result.cleanup_error.is_some() {
            break;
        }
    }
    for item in &mut report.items {
        uncertain |= item.outcome.is_none()
            || matches!(item.outcome, Some(Outcome::Failed | Outcome::Unknown))
            || item.after.as_ref().is_none_or(|f| {
                f.subscription == Subscription::Unknown
                    || (report.action == Action::Subscribe
                        && f.subscription != Subscription::Subscribed)
                    || (report.action == Action::Unsubscribe
                        && f.subscription != Subscription::NotSubscribed)
            });
        let mut installs: Vec<InstallObservation> = Vec::new();
        let mut current_after_valid = false;
        for (after, facts) in item
            .before
            .iter()
            .map(|facts| (false, facts))
            .chain(item.after.iter().map(|facts| (true, facts)))
        {
            if let Some(install) = &facts.install {
                let path = PathBuf::from(&install.path);
                let valid = if !path.is_absolute() || !path.is_dir() {
                    Err("native install is not an absolute existing directory".into())
                } else {
                    validate_about(&path, version)
                        .map_err(|e| e.to_string())
                        .and_then(|()| validate_item_path(&path, item.item_id))
                };
                match valid {
                    Ok(()) => {
                        current_after_valid |=
                            after && install.freshness == steam_protocol::Freshness::Current;
                        installs.retain(|i| i.path != path);
                        installs.push(InstallObservation {
                            backend: AcquisitionBackend::SteamClient,
                            item_id: PublishedFileId::try_from(item.item_id)
                                .expect("validated candidate"),
                            path,
                            freshness: match install.freshness {
                                steam_protocol::Freshness::Current => Freshness::Current,
                                steam_protocol::Freshness::NeedsUpdate => {
                                    Freshness::UpdateAvailable
                                }
                                steam_protocol::Freshness::Unknown => Freshness::Unknown,
                            },
                        });
                    }
                    Err(e) => {
                        let message = format!("{}: unusable mod metadata: {e}", path.display());
                        if !item.validation_errors.contains(&message) {
                            item.validation_errors.push(message);
                        }
                    }
                }
            }
        }
        let subscription = match item
            .after
            .as_ref()
            .map(|f| f.subscription)
            .unwrap_or(Subscription::Unknown)
        {
            Subscription::Unknown => SubscriptionState::Unknown,
            Subscription::Subscribed => SubscriptionState::Subscribed,
            Subscription::NotSubscribed => SubscriptionState::NotSubscribed,
        };
        result.items.push(AcquisitionItem {
            item_id: PublishedFileId::try_from(item.item_id).expect("validated candidate"),
            subscription,
            installations: installs,
            download: if report.action != Action::Download {
                DownloadOutcome::NotStarted
            } else if item.outcome == Some(Outcome::Installed) && current_after_valid {
                DownloadOutcome::Succeeded
            } else {
                DownloadOutcome::Unverified
            },
            outcome: match item.outcome {
                Some(Outcome::Observed | Outcome::Acknowledged | Outcome::Installed) => {
                    ItemOutcome::Succeeded
                }
                Some(Outcome::Failed) => ItemOutcome::Failed,
                _ => ItemOutcome::Unknown,
            },
            diagnostics: item.validation_errors.clone(),
        });
    }
    if report.action != Action::Inspect
        && possible_effects
        && uncertain
        && !report.pending.iter().any(|p| p.request_id == request_id)
    {
        report.pending.push(PendingEffects {
            request_id: request_id.into(),
            action: report.action,
            items: report.items.clone(),
            runs: report.runs.clone(),
            resolved_by: None,
        });
    }
    if report.action == Action::Inspect && error.is_none() && result.cleanup_error.is_none() {
        for pending in &mut report.pending {
            if pending.resolved_by.is_none()
                && pending.items.iter().all(|old| {
                    report.items.iter().any(|new| {
                        new.item_id == old.item_id
                            && new.outcome == Some(Outcome::Observed)
                            && new.after.as_ref().is_some_and(|f| {
                                f.subscription != Subscription::Unknown
                                    && (pending.action != Action::Download
                                        || f.install.as_ref().is_some_and(|install| {
                                            install.freshness == steam_protocol::Freshness::Current
                                                && result
                                                    .items
                                                    .iter()
                                                    .find(|i| i.item_id.get() == new.item_id)
                                                    .is_some_and(|i| {
                                                        i.installations.iter().any(|copy| {
                                                            copy.path == Path::new(&install.path)
                                                                && copy.freshness
                                                                    == Freshness::Current
                                                        })
                                                    })
                                        }))
                            })
                    })
                })
            {
                pending.resolved_by = Some(format!("Inspect {request_id}"));
            }
        }
    }
    result.logs_truncated = report.dropped_lines != 0;
    (result, error)
}
