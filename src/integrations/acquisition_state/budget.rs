//! Last-receipt admission, not a second store. Known evidence is counted exactly;
//! future shapes include current results, possible pending duplication and resolution.
#[cfg(test)]
#[path = "../../../tests/support/receipt_budget.rs"]
mod tests;

use super::FinalReport;
use crate::application::{acquisition::*, native_acquisition::*};
use serde::Serialize;
use std::path::{Path, PathBuf};
use steam_protocol::{Action, Facts, FailureDetail, FailureKind, FailureStage, Install, Outcome};

pub(crate) const MESSAGE_BYTES: usize = 4096;
pub(crate) const VECTOR_BYTES: usize = 16 * 1024;
pub(crate) const PREVIEW_BYTES: usize = 64 * 1024;
pub(crate) const ACKNOWLEDGEMENT: &str = "Explicit user acknowledgement; effects not reversed";
pub(crate) const RUN_RANDOM_BYTES: usize = 16;

pub(crate) fn encoded(value: &impl Serialize) -> Result<usize, String> {
    serde_json::to_vec(value)
        .map(|b| b.len())
        .map_err(|e| e.to_string())
}

/// Ordinary prose only. Never apply to protocol facts, identities or FailureDetail.
/// The cap includes JSON quotes/escapes and the visible omitted-byte accounting.
pub(crate) fn diagnostic(text: &str, limit: usize) -> String {
    if encoded(&text).is_ok_and(|n| n <= limit) {
        return text.into();
    }
    let suffix = |n| format!(" [diagnostic shortened: {n} UTF-8 bytes omitted]");
    let available = limit.saturating_sub(encoded(&suffix(text.len())).expect("string") + 2);
    let mut end = 0;
    let mut bytes = 0;
    for ch in text.chars() {
        let size = match ch {
            '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
            '\u{0}'..='\u{1f}' => 6,
            _ => ch.len_utf8(),
        };
        if bytes + size > available {
            break;
        }
        bytes += size;
        end += ch.len_utf8();
    }
    format!("{}{}", &text[..end], suffix(text.len() - end))
}

pub(crate) fn messages(lines: &mut Vec<String>) {
    if encoded(lines).is_ok_and(|n| n <= VECTOR_BYTES) {
        return;
    }
    let text = format!("{} diagnostic messages:\n{}", lines.len(), lines.join("\n"));
    *lines = vec![diagnostic(&text, VECTOR_BYTES - 2)];
}

pub(crate) fn finish_diagnostics(report: &mut FinalReport) {
    for text in [
        &mut report.error,
        &mut report.report.cleanup_error,
        &mut report.report.recovery_required,
    ]
    .into_iter()
    .flatten()
    {
        *text = diagnostic(text, MESSAGE_BYTES);
    }
    messages(&mut report.report.warnings);
    for item in &mut report.report.items {
        messages(&mut item.diagnostics);
    }
}

pub(crate) fn check_stored(report: &FinalReport) -> Result<(), String> {
    if encoded(report)? > super::LIMIT as usize {
        return Err("Receipt resolution exceeds 4 MiB; original uncertainty retained".into());
    }
    Ok(())
}

pub(crate) fn check(report: &FinalReport, workspace: Option<&Path>) -> Result<(), String> {
    let required = required_bytes(report, workspace)?;
    if required > super::LIMIT as usize {
        return Err(format!(
            "Receipt capacity refused before dispatch: {required} bytes reserved exceeds 4 MiB. Choose a smaller explicit preview/batch; retained native uncertainty is not discarded."
        ));
    }
    Ok(())
}

pub(crate) fn required_bytes(
    report: &FinalReport,
    workspace: Option<&Path>,
) -> Result<usize, String> {
    if workspace.is_some_and(|p| !p.is_absolute()) {
        return Err("Receipt capacity requires a captured absolute workspace path".into());
    }
    if report.candidates.is_empty() || report.candidates.len() > steam_protocol::MAX_ITEMS {
        return Err("Confirm an explicit batch of 1–20 candidates".into());
    }
    let resolution = format!("Inspect {}", report.request_id);
    let resolution = if resolution.len() > ACKNOWLEDGEMENT.len() {
        resolution
    } else {
        ACKNOWLEDGEMENT.into()
    };
    let mut size = encoded(report)? + 20 + MESSAGE_BYTES; // returned_at and top-level error.
    size += encoded(&future_result(report, workspace))?;
    if let Some(native) = &report.native {
        size += native
            .pending
            .iter()
            .filter(|p| p.resolved_by.is_none())
            .count()
            * encoded(&resolution)?;
        size += encoded(&future_native(report, resolution))?;
    }
    Ok(size)
}

fn prose() -> String {
    "x".repeat(MESSAGE_BYTES - 2)
}
fn vector() -> Vec<String> {
    vec!["x".repeat(VECTOR_BYTES - 4)]
}
fn native_path() -> String {
    // Strict protocol paths have <=4096 UTF-8 bytes and no controls: at most
    // two JSON bytes per input byte (quote/backslash). This is an absolute path.
    format!("/{}", "\\".repeat(4095))
}
fn future_result(report: &FinalReport, workspace: Option<&Path>) -> AcquisitionReport {
    let items = report
        .candidates
        .iter()
        .map(|candidate| {
            let paths = if report.native.is_some() {
                vec![PathBuf::from(native_path()); 2] // validated Before and After may differ
            } else {
                vec![report.mods_root.join(candidate.id.get().to_string())]
            };
            AcquisitionItem {
                item_id: candidate.id,
                subscription: SubscriptionState::NotSubscribed,
                installations: paths
                    .into_iter()
                    .map(|path| InstallObservation {
                        backend: AcquisitionBackend::SteamClient,
                        item_id: candidate.id,
                        path,
                        freshness: Freshness::UpdateAvailable,
                    })
                    .collect(),
                download: DownloadOutcome::NotStarted,
                outcome: ItemOutcome::Succeeded,
                diagnostics: vector(),
            }
        })
        .collect();
    AcquisitionReport {
        items,
        logs: Vec::new(),
        logs_truncated: false,
        runs: workspace
            .map(|w| vec![w.join(format!("run-{}", "x".repeat(RUN_RANDOM_BYTES)))])
            .unwrap_or_default(),
        warnings: vector(),
        recovery_required: Some(prose()),
        cleanup_error: Some(prose()),
    }
}
fn future_native(report: &FinalReport, resolution: String) -> NativeReport {
    let request_id = format!("{}-0", report.request_id);
    let facts = Facts {
        subscription: steam_protocol::Subscription::NotSubscribed,
        install: Some(Install {
            path: native_path(),
            size_bytes: u64::MAX,
            timestamp: u32::MAX,
            freshness: steam_protocol::Freshness::NeedsUpdate,
        }),
    };
    let items: Vec<_> = report
        .candidates
        .iter()
        .map(|c| NativeItem {
            item_id: c.id.get(),
            request_id: request_id.clone(),
            before: Some(facts.clone()),
            after: Some(facts.clone()),
            acknowledgement: false,
            outcome: Some(Outcome::Acknowledged),
            failure: Some(FailureDetail {
                stage: FailureStage::Initialize,
                kind: FailureKind::InitializationFailed,
                message: "\\".repeat(steam_protocol::MAX_FAILURE_MESSAGE),
            }),
            validation_errors: vec![prose(); 2],
        })
        .collect();
    let runs = vec![NativeRun {
        request_id,
        status: prose(),
        protocol_complete: false,
        protocol_error: Some(prose()),
        error: Some(prose()),
        input_error: Some(prose()),
        exit_status: Some(prose()),
        spawned: false,
        leader_reaped: false,
        stdin_closed: false,
        stdout_eof: false,
        stderr_eof: false,
        termination_requested: false,
        termination_error: Some(prose()),
        detached_workers: usize::MAX,
        cleanup_uncertain: false,
    }];
    // The adapter retains <=256 KiB of UTF-8 diagnostic text and <=1000 lines.
    // Six is the maximum JSON expansion for ordinary prose (unlike strict paths).
    let mut diagnostics = vec![String::new(); 1000];
    diagnostics[0] = "\u{1}".repeat(256 * 1024);
    NativeReport {
        action: Action::Unsubscribe,
        runtime_dir: None,
        items: items.clone(),
        runs: runs.clone(),
        pending: vec![PendingEffects {
            request_id: report.request_id.clone(),
            action: Action::Unsubscribe,
            items,
            runs,
            resolved_by: Some(resolution),
        }],
        diagnostics,
        dropped_lines: usize::MAX,
        dropped_bytes: usize::MAX,
    }
}
