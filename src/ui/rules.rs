//! Community panel state. Download completion never owns the enable preference.
use crate::{
    domain::rules::Ruleset,
    integrations::community::{CommunityStore, RulesError, SOURCE},
};
use std::{
    path::Path,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

pub(crate) struct RulesState {
    pub store: Option<CommunityStore>,
    cached: Option<Arc<Ruleset>>,
    pub enabled: bool,
    pub open: bool,
    pub updating: bool,
    pub pending_adoption: bool,
    notice: String,
}
impl RulesState {
    pub fn load(settings_file: Option<&Path>) -> Self {
        let store = settings_file
            .and_then(Path::parent)
            .map(|root| CommunityStore::new(root.to_path_buf()));
        let mut state = Self {
            store,
            cached: None,
            enabled: true,
            open: false,
            updating: false,
            pending_adoption: false,
            notice: String::new(),
        };
        if let Some(store) = &state.store {
            match store.enabled() {
                Ok(enabled) => state.enabled = enabled,
                Err(e) => {
                    state.enabled = false;
                    state.notice = e.to_string();
                }
            }
            match store.load() {
                Ok(cached) => state.cached = cached.map(Arc::new),
                Err(e) => {
                    state.notice.push_str(&format!(" Cache unavailable: {e}"));
                }
            }
        } else {
            state.enabled = false;
            state.notice =
                "No application data root; updates and preference persistence unavailable".into();
        }
        state
    }
    pub fn effective(&self) -> Arc<Ruleset> {
        if self.enabled {
            self.cached.clone().unwrap_or_default()
        } else {
            Arc::default()
        }
    }
    pub fn toggle(&mut self, enabled: bool) {
        if let Some(store) = &self.store {
            match store.set_enabled(enabled) {
                Ok(()) => self.enabled = enabled,
                Err(e) => self.notice = format!("Preference not changed: {e}"),
            }
        }
    }
    pub fn finish(&mut self, result: Result<Ruleset, RulesError>) {
        self.updating = false;
        match result {
            Ok(rules) => {
                self.cached = Some(Arc::new(rules));
                self.notice = "Validated cache updated".into();
            }
            Err(e) => self.notice = format!("Update failed; retained last-known-good cache: {e}"),
        }
    }
    pub fn failed_start(&mut self, error: &std::io::Error) {
        self.updating = false;
        self.notice = format!("Update could not start: {error}");
    }
    pub fn status(&self) -> String {
        let mode = if self.enabled && self.cached.is_some() {
            "Community rules enabled"
        } else {
            "Metadata-only sorting"
        };
        let cache = match self.cached.as_ref() {
            None => "No validated cache; offline metadata fallback".into(),
            Some(rules) => match rules.timestamp() {
                None => "Cache timestamp unknown; freshness unknown".into(),
                Some(timestamp) => {
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    if timestamp > now {
                        format!("Cache timestamp {timestamp} (future timestamp; freshness unknown)")
                    } else {
                        let age = now - timestamp;
                        format!(
                            "Cache timestamp {timestamp}; {} days old{}",
                            age / 86400,
                            if age > 30 * 86400 {
                                "; stale (over 30 days)"
                            } else {
                                ""
                            }
                        )
                    }
                }
            },
        };
        format!(
            "{mode}\n{cache}\nSource: {SOURCE}\n{}{}{}",
            if self.updating { "Updating… " } else { "" },
            self.notice,
            if self.pending_adoption {
                "\nCache ready; pending adoption after the protected operation"
            } else {
                ""
            }
        )
    }
}
