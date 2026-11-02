//! Worker receipts keep operation outcomes independent from subscription and copy observations.
use crate::integrations::{
    steamcmd::{self, SteamCmdConfig},
    workshop::{PublishedFileId, RIMWORLD_APP_ID, WorkshopItem, WorkshopKind},
};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, path::PathBuf, sync::atomic::AtomicBool};

/// Captured inputs for a background job. The caller owns confirmation and UI/launch gates.
#[derive(Clone, Debug)]
pub struct AcquisitionRequest {
    pub config: SteamCmdConfig,
    pub items: Vec<WorkshopItem>,
}

/// Blocking worker entrypoint; never invoke from the event loop.
pub fn run_acquisition(
    request: &AcquisitionRequest,
    cancel: &AtomicBool,
    event: &mut dyn FnMut(AcquisitionEvent),
) -> Result<AcquisitionReport, AcquisitionError> {
    if request
        .items
        .iter()
        .any(|item| item.app_id != RIMWORLD_APP_ID || item.kind != WorkshopKind::ItemCandidate)
    {
        return Err(AcquisitionError::Invalid(
            "only resolved RimWorld item candidates can be downloaded".into(),
        ));
    }
    let ids: Vec<_> = request.items.iter().map(|item| item.id).collect();
    let updated: HashMap<_, _> = request
        .items
        .iter()
        .map(|item| (item.id, item.upstream_updated))
        .collect();
    steamcmd::run_downloads_with_updates(&request.config, &ids, &updated, cancel, event)
}

/// Comparison to the captured public metadata snapshot, not a live guarantee.
pub fn freshness(recorded: Option<u64>, upstream: Option<u64>) -> Freshness {
    match (recorded, upstream) {
        (Some(old), Some(new)) if old >= new => Freshness::Current,
        (Some(_), Some(_)) => Freshness::UpdateAvailable,
        _ => Freshness::Unknown,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AcquisitionError {
    #[error("invalid acquisition input: {0}")]
    Invalid(String),
    #[error("acquisition IO: {0}")]
    Io(#[from] std::io::Error),
    #[error("public Workshop API: {0}")]
    Api(String),
    #[error("recovery required: {0}")]
    Recovery(String),
    #[error("owned process cleanup incomplete: {0}")]
    Cleanup(String),
    #[error("acquisition cancelled")]
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AcquisitionBackend {
    SteamCmd,
    SteamClient,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubscriptionState {
    Unknown,
    Subscribed,
    NotSubscribed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Freshness {
    Unknown,
    Current,
    UpdateAvailable,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallObservation {
    pub backend: AcquisitionBackend,
    pub item_id: PublishedFileId,
    pub path: PathBuf,
    pub freshness: Freshness,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ItemOutcome {
    Requested,
    Accepted,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DownloadOutcome {
    NotStarted,
    Succeeded,
    Failed,
    Unverified,
    Cancelled,
    TimedOut,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AcquisitionItem {
    pub item_id: PublishedFileId,
    pub subscription: SubscriptionState,
    pub installations: Vec<InstallObservation>,
    pub download: DownloadOutcome,
    pub outcome: ItemOutcome,
    pub diagnostics: Vec<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AcquisitionReport {
    pub items: Vec<AcquisitionItem>,
    pub logs: Vec<String>,
    pub logs_truncated: bool,
    pub runs: Vec<PathBuf>,
    pub warnings: Vec<String>,
    pub recovery_required: Option<String>,
    pub cleanup_error: Option<String>,
}
#[derive(Clone, Debug)]
pub enum AcquisitionEvent {
    Item(AcquisitionItem),
    Log(String),
    Finished(AcquisitionReport),
}
