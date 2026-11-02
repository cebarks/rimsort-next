//! Actual 0.13.1 SDK bindings. Construction is restricted to validated CLI requests.
use std::{
    collections::BTreeSet,
    sync::mpsc::{self, Receiver, SyncSender},
};
use steam_helper::{ApiError, Callback, Identity, SteamApi};
use steam_protocol::{APP_ID, Action, Facts, Freshness, Install, MAX_ITEMS, Request, Subscription};
use steamworks::{
    AppId, CallbackHandle, Client, DownloadItemResult, FileType, ItemState, PublishedFileId,
};

pub struct Native {
    // Dropped before the client: global download registration lives for the whole operation.
    _download_callback: CallbackHandle,
    client: Client,
    sender: SyncSender<Callback>,
    receiver: Receiver<Callback>,
}
impl Native {
    pub fn initialize(request: &Request) -> Result<Self, String> {
        request.validate().map_err(|e| e.to_string())?;
        let client = Client::init_app(AppId(APP_ID)).map_err(|e| e.to_string())?;
        let (sender, receiver) = mpsc::sync_channel(MAX_ITEMS * 2);
        let tx = sender.clone();
        let ids: BTreeSet<_> = request.ids.iter().copied().collect();
        let action = request.action;
        let handle = client.register_callback(move |event: DownloadItemResult| {
            if action == Action::Download
                && event.app_id.0 == APP_ID
                && ids.contains(&event.published_file_id.0)
            {
                // A full queue loses a notification, never blocks Steam's callback thread.
                // The operation then times out as unknown, never as success.
                let _ = tx.try_send(Callback::Completed {
                    action,
                    app_id: event.app_id.0,
                    item_id: event.published_file_id.0,
                    success: event.error.is_none(),
                });
            }
        });
        Ok(Self {
            _download_callback: handle,
            client,
            sender,
            receiver,
        })
    }
}
impl SteamApi for Native {
    fn query(&mut self, item_id: u64) -> Result<(), ApiError> {
        let tx = self.sender.clone();
        self.client
            .ugc()
            .query_item(PublishedFileId(item_id))
            .map_err(|_| ApiError)?
            .fetch(move |result| {
                let identity = result
                    .ok()
                    .and_then(|rows| {
                        if rows.returned_results() != 1 {
                            return None;
                        }
                        rows.get(0)
                            .filter(|row| row.published_file_id.0 == item_id)
                            .map(|row| Identity {
                                app_id: row.consumer_app_id.map_or(0, |app| app.0),
                                community: row.file_type == FileType::Community && !row.banned,
                            })
                    })
                    .ok_or(ApiError);
                let _ = tx.try_send(Callback::Verified {
                    item_id,
                    result: identity,
                });
            });
        Ok(())
    }
    fn observe(&self, item_id: u64) -> Facts {
        let ugc = self.client.ugc();
        let id = PublishedFileId(item_id);
        let state = ugc.item_state(id);
        let subscription = if state.contains(ItemState::SUBSCRIBED) {
            Subscription::Subscribed
        } else {
            Subscription::NotSubscribed
        };
        let install = if state.contains(ItemState::INSTALLED) {
            ugc.item_install_info(id).and_then(|info| {
                if !std::path::Path::new(&info.folder).is_absolute()
                    || info.folder.len() > 4096
                    || info.folder.chars().any(char::is_control)
                {
                    return None;
                }
                let freshness = if state.contains(ItemState::NEEDS_UPDATE) {
                    Freshness::NeedsUpdate
                } else if state.intersects(ItemState::DOWNLOADING | ItemState::DOWNLOAD_PENDING) {
                    Freshness::Unknown
                } else {
                    Freshness::Current
                };
                Some(Install {
                    path: info.folder,
                    size_bytes: info.size_on_disk,
                    timestamp: info.timestamp,
                    freshness,
                })
            })
        } else {
            None
        };
        Facts {
            subscription,
            install,
        }
    }
    fn start(&mut self, action: Action, item_id: u64) -> Result<(), ApiError> {
        let ugc = self.client.ugc();
        let id = PublishedFileId(item_id);
        match action {
            Action::Subscribe | Action::Unsubscribe => {
                let tx = self.sender.clone();
                let callback = move |result: Result<(), steamworks::SteamError>| {
                    let _ = tx.try_send(Callback::Completed {
                        action,
                        app_id: APP_ID,
                        item_id,
                        success: result.is_ok(),
                    });
                };
                // steamworks retains each call-result closure in Client's inner registry.
                if action == Action::Subscribe {
                    ugc.subscribe_item(id, callback);
                } else {
                    ugc.unsubscribe_item(id, callback);
                }
                Ok(())
            }
            Action::Download => {
                if ugc.download_item(id, false) {
                    Ok(())
                } else {
                    Err(ApiError)
                }
            }
            Action::Inspect => Err(ApiError),
        }
    }
    fn pump(&mut self) -> Vec<Callback> {
        self.client.run_callbacks();
        self.receiver.try_iter().take(MAX_ITEMS * 2).collect()
    }
}
