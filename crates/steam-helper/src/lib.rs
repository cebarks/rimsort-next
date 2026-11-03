//! Single-request state machine. Only the executable constructs the native adapter.
use std::collections::BTreeMap;
use steam_protocol::{
    APP_ID, Action, Event, Facts, FailureDetail, FailureKind, FailureStage, Freshness, Outcome,
    Payload, Phase, ProtocolError, Request,
};

pub type ApiError = FailureDetail;
#[derive(Debug, Clone, Copy)]
pub struct Identity {
    pub app_id: u32,
    pub community: bool,
}
#[derive(Debug)]
pub enum Callback {
    Verified {
        item_id: u64,
        result: Result<Identity, ApiError>,
    },
    Completed {
        action: Action,
        app_id: u32,
        item_id: u64,
        result: Result<(), ApiError>,
    },
}
pub trait SteamApi {
    fn query(&mut self, item_id: u64) -> Result<(), ApiError>;
    fn observe(&self, item_id: u64) -> Facts;
    fn start(&mut self, action: Action, item_id: u64) -> Result<(), ApiError>;
    fn pump(&mut self) -> Vec<Callback>;
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage {
    Verifying,
    Running,
    Done,
}
pub struct Machine {
    request: Request,
    stages: BTreeMap<u64, Stage>,
    events: Vec<Event>,
    terminal: bool,
}
impl Machine {
    pub fn new(request: Request, api: &mut impl SteamApi) -> Result<Self, ProtocolError> {
        request.validate()?;
        let stages = request
            .ids
            .iter()
            .map(|&id| (id, Stage::Verifying))
            .collect();
        let mut machine = Self {
            request,
            stages,
            events: Vec::new(),
            terminal: false,
        };
        for id in machine.request.ids.clone() {
            if let Err(detail) = api.query(id) {
                machine.unverified(id, Outcome::Failed, detail);
            }
        }
        machine.finish_if_ready();
        Ok(machine)
    }
    pub fn tick(&mut self, api: &mut impl SteamApi, timed_out: bool) {
        if self.terminal {
            return;
        }
        if timed_out {
            for (id, stage) in self.stages.clone() {
                match stage {
                    Stage::Verifying => self.unverified(
                        id,
                        Outcome::Unknown,
                        FailureDetail::new(
                            FailureStage::Verify,
                            FailureKind::TimedOut,
                            "Metadata verification timed out",
                        ),
                    ),
                    Stage::Running => self.complete(
                        id,
                        api.observe(id),
                        Outcome::Unknown,
                        Some(FailureDetail::new(
                            FailureStage::Complete,
                            FailureKind::TimedOut,
                            "Operation completion timed out",
                        )),
                    ),
                    Stage::Done => {}
                }
            }
        } else {
            // A notification collected before start cannot acknowledge that new operation.
            let active_at_pump = self.stages.clone();
            for callback in api.pump() {
                match callback {
                    Callback::Verified { item_id, result }
                        if self.stages.get(&item_id) == Some(&Stage::Verifying) =>
                    {
                        self.verified(api, item_id, result);
                    }
                    Callback::Completed {
                        action,
                        app_id,
                        item_id,
                        result,
                    } if app_id == APP_ID
                        && action == self.request.action
                        && active_at_pump.get(&item_id) == Some(&Stage::Running)
                        && self.stages.get(&item_id) == Some(&Stage::Running) =>
                    {
                        self.completed(api, item_id, result);
                    }
                    _ => {}
                }
            }
        }
        self.finish_if_ready();
    }
    fn verified(&mut self, api: &mut impl SteamApi, id: u64, result: Result<Identity, ApiError>) {
        match result {
            Ok(Identity {
                app_id: APP_ID,
                community: true,
            }) => {}
            Ok(_) => {
                self.unverified(
                    id,
                    Outcome::Failed,
                    FailureDetail::new(
                        FailureStage::Verify,
                        FailureKind::InvalidItemIdentity,
                        "Metadata does not identify an allowed RimWorld community item",
                    ),
                );
                return;
            }
            Err(detail) => {
                self.unverified(id, Outcome::Failed, detail);
                return;
            }
        }
        let facts = api.observe(id);
        self.observation(id, Phase::Before, facts.clone());
        if self.request.action == Action::Inspect {
            self.complete(id, facts, Outcome::Observed, None);
        } else if let Err(detail) = api.start(self.request.action, id) {
            self.complete(id, api.observe(id), Outcome::Failed, Some(detail));
        } else {
            self.stages.insert(id, Stage::Running);
        }
    }
    fn completed(&mut self, api: &impl SteamApi, id: u64, result: Result<(), ApiError>) {
        let facts = api.observe(id);
        if let Err(detail) = result {
            self.complete(id, facts, Outcome::Failed, Some(detail));
            return;
        }
        let (outcome, detail) = match self.request.action {
            Action::Subscribe | Action::Unsubscribe => (Outcome::Acknowledged, None),
            Action::Download
                if facts
                    .install
                    .as_ref()
                    .is_some_and(|i| i.freshness == Freshness::Current) =>
            {
                (Outcome::Installed, None)
            }
            Action::Download => (
                Outcome::Unknown,
                Some(FailureDetail::new(
                    FailureStage::Complete,
                    FailureKind::UnverifiedInstall,
                    "Download acknowledged but no current installation was observed",
                )),
            ),
            Action::Inspect => (Outcome::Observed, None),
        };
        self.complete(id, facts, outcome, detail);
    }
    pub fn is_done(&self) -> bool {
        self.terminal
    }
    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }
    fn emit(&mut self, payload: Payload) {
        self.events
            .push(Event::new(&self.request.request_id, payload));
    }
    fn observation(&mut self, id: u64, phase: Phase, facts: Facts) {
        self.emit(Payload::Observation {
            item_id: id,
            phase,
            facts,
        });
    }
    fn complete(&mut self, id: u64, facts: Facts, outcome: Outcome, detail: Option<FailureDetail>) {
        self.observation(id, Phase::After, facts);
        self.emit(Payload::Outcome {
            item_id: id,
            outcome,
            detail,
        });
        self.stages.insert(id, Stage::Done);
    }
    fn unverified(&mut self, id: u64, outcome: Outcome, detail: FailureDetail) {
        self.observation(id, Phase::Before, Facts::default());
        self.complete(id, Facts::default(), outcome, Some(detail));
    }
    fn finish_if_ready(&mut self) {
        if !self.terminal && self.stages.values().all(|s| *s == Stage::Done) {
            self.emit(Payload::Terminal);
            self.terminal = true;
        }
    }
}
