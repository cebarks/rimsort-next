use std::collections::VecDeque;
use steam_helper::*;
use steam_protocol::*;

#[derive(Default)]
struct Fake {
    pending: VecDeque<Callback>,
    queries: Vec<u64>,
    mutations: Vec<(Action, u64)>,
    old_copy: bool,
    subscription: Option<Subscription>,
    fresh: bool,
    reject: bool,
    reject_query: bool,
    pumps: usize,
}
impl SteamApi for Fake {
    fn query(&mut self, id: u64) -> Result<(), ApiError> {
        self.queries.push(id);
        if self.reject_query {
            Err(ApiError::new(
                FailureStage::Verify,
                FailureKind::MetadataQueryFailed,
                "Could not create workshop query",
            ))
        } else {
            Ok(())
        }
    }
    fn observe(&self, _: u64) -> Facts {
        Facts {
            subscription: self.subscription.unwrap_or(Subscription::Subscribed),
            install: self.old_copy.then(|| Install {
                path: std::env::current_dir()
                    .unwrap()
                    .join("fixture/old-mod")
                    .to_str()
                    .unwrap()
                    .into(),
                size_bytes: 100,
                timestamp: 1,
                freshness: if self.fresh {
                    Freshness::Current
                } else {
                    Freshness::NeedsUpdate
                },
            }),
        }
    }
    fn start(&mut self, action: Action, id: u64) -> Result<(), ApiError> {
        self.mutations.push((action, id));
        if self.reject {
            Err(ApiError::new(
                FailureStage::Request,
                FailureKind::RequestRejected,
                "Steam rejected the download request",
            ))
        } else {
            Ok(())
        }
    }
    fn pump(&mut self) -> Vec<Callback> {
        self.pumps += 1;
        self.pending.drain(..).collect()
    }
}
fn request(action: Action) -> Request {
    Request {
        schema: 1,
        app_id: 294100,
        request_id: "req".into(),
        action,
        ids: vec![7],
    }
}
fn verified(id: u64, app_id: u32, community: bool) -> Callback {
    Callback::Verified {
        item_id: id,
        result: Ok(Identity { app_id, community }),
    }
}
fn completed(action: Action, app_id: u32, item_id: u64, success: bool) -> Callback {
    Callback::Completed {
        action,
        app_id,
        item_id,
        result: if success {
            Ok(())
        } else {
            Err(ApiError::new(
                FailureStage::Complete,
                FailureKind::NativeFailure,
                "Access denied",
            ))
        },
    }
}
fn outcomes(events: &[Event]) -> Vec<Outcome> {
    events
        .iter()
        .filter_map(|e| {
            if let Payload::Outcome { outcome, .. } = e.payload {
                Some(outcome)
            } else {
                None
            }
        })
        .collect()
}
fn drive(action: Action, mut fake: Fake) -> (Machine, Fake) {
    let machine = Machine::new(request(action), &mut fake).unwrap();
    (machine, fake)
}

fn assert_failure(events: &[Event], outcome: Outcome, stage: &str, kind: &str) {
    assert_eq!(outcomes(events), [outcome]);
    let event = events
        .iter()
        .find(|e| matches!(e.payload, Payload::Outcome { .. }))
        .unwrap();
    let line = String::from_utf8(encode_event(event).unwrap()).unwrap();
    assert!(
        line.contains(&format!("\"stage\":\"{stage}\"")),
        "missing stage: {line}"
    );
    assert!(
        line.contains(&format!("\"kind\":\"{kind}\"")),
        "missing failure kind: {line}"
    );
    assert!(
        matches!(&event.payload, Payload::Outcome { item_id: 7, detail: Some(detail), .. }
        if !detail.message.is_empty() && detail.message.len() <= 512)
    );
    let mut stream = EventStream::new(request(Action::Download)).unwrap();
    for event in events {
        stream.accept_line(&encode_event(event).unwrap()).unwrap();
    }
    stream.finish().unwrap();
}

#[test]
fn subscribe_acknowledgement_is_not_install_and_requires_pumping() {
    let (mut machine, mut fake) = drive(Action::Subscribe, Fake::default());
    assert_eq!(fake.queries, [7]);
    assert!(fake.mutations.is_empty());
    fake.pending.push_back(verified(7, 294100, true));
    machine.tick(&mut fake, false);
    assert_eq!(fake.mutations, [(Action::Subscribe, 7)]);
    assert!(!machine.is_done());
    fake.pending
        .push_back(completed(Action::Subscribe, 294100, 7, true));
    assert!(!machine.is_done());
    machine.tick(&mut fake, false);
    assert_eq!(fake.pumps, 2);
    let events = machine.take_events();
    assert_eq!(outcomes(&events), [Outcome::Acknowledged]);
    assert!(events.iter().all(|e| !matches!(
        &e.payload,
        Payload::Observation {
            facts: Facts {
                install: Some(_),
                ..
            },
            ..
        }
    )));
    assert!(machine.is_done());
}

#[test]
fn successful_subscription_callbacks_preserve_observed_facts_independently() {
    for action in [Action::Subscribe, Action::Unsubscribe] {
        for subscription in [
            Subscription::Unknown,
            Subscription::NotSubscribed,
            Subscription::Subscribed,
        ] {
            let (mut machine, mut fake) = drive(
                action,
                Fake {
                    old_copy: true,
                    subscription: Some(subscription),
                    ..Fake::default()
                },
            );
            fake.pending.push_back(verified(7, 294100, true));
            machine.tick(&mut fake, false);
            fake.pending.push_back(completed(action, 294100, 7, true));
            machine.tick(&mut fake, false);
            let events = machine.take_events();
            assert_eq!(outcomes(&events), [Outcome::Acknowledged]);
            let after = events.iter().find(|event| {
                matches!(
                    event.payload,
                    Payload::Observation {
                        phase: Phase::After,
                        ..
                    }
                )
            });
            assert_eq!(
                after,
                Some(&Event::new(
                    "req",
                    Payload::Observation {
                        item_id: 7,
                        phase: Phase::After,
                        facts: Facts {
                            subscription,
                            install: Some(Install {
                                path: std::env::current_dir()
                                    .unwrap()
                                    .join("fixture/old-mod")
                                    .to_str()
                                    .unwrap()
                                    .into(),
                                size_bytes: 100,
                                timestamp: 1,
                                freshness: Freshness::NeedsUpdate,
                            }),
                        },
                    }
                )),
                "{action:?} must not synthesize {subscription:?} observation"
            );
            let mut stream = EventStream::new(request(action)).unwrap();
            for event in events {
                stream.accept_line(&encode_event(&event).unwrap()).unwrap();
            }
            stream.finish().unwrap();
        }
    }
}

#[test]
fn foreign_or_collection_identity_never_mutates() {
    for (app, community) in [(42, true), (294100, false)] {
        let (mut machine, mut fake) = drive(Action::Subscribe, Fake::default());
        fake.pending.push_back(verified(7, app, community));
        machine.tick(&mut fake, false);
        assert!(fake.mutations.is_empty());
        assert_eq!(outcomes(&machine.take_events()), [Outcome::Failed]);
    }
}

#[test]
fn download_matches_app_item_and_action_then_requires_current_install() {
    let (mut machine, mut fake) = drive(
        Action::Download,
        Fake {
            old_copy: true,
            ..Fake::default()
        },
    );
    fake.pending.push_back(verified(7, 294100, true));
    machine.tick(&mut fake, false);
    for callback in [
        completed(Action::Download, 42, 7, true),
        completed(Action::Download, 294100, 8, true),
        completed(Action::Subscribe, 294100, 7, true),
    ] {
        fake.pending.push_back(callback);
        machine.tick(&mut fake, false);
        assert!(!machine.is_done());
    }
    fake.pending
        .push_back(completed(Action::Download, 294100, 7, true));
    machine.tick(&mut fake, false);
    assert_eq!(outcomes(&machine.take_events()), [Outcome::Unknown]);
}

#[test]
fn failed_update_preserves_available_old_copy() {
    let (mut machine, mut fake) = drive(
        Action::Download,
        Fake {
            old_copy: true,
            ..Fake::default()
        },
    );
    fake.pending.push_back(verified(7, 294100, true));
    machine.tick(&mut fake, false);
    fake.pending
        .push_back(completed(Action::Download, 294100, 7, false));
    machine.tick(&mut fake, false);
    let events = machine.take_events();
    assert_eq!(outcomes(&events), [Outcome::Failed]);
    assert!(events.iter().any(|e| matches!(
        &e.payload,
        Payload::Observation {
            phase: Phase::After,
            facts: Facts {
                install: Some(Install {
                    freshness: Freshness::NeedsUpdate,
                    ..
                }),
                ..
            },
            ..
        }
    )));
}

#[test]
fn timeout_is_unknown_without_retry_and_ignores_late_callbacks() {
    let (mut machine, mut fake) = drive(Action::Subscribe, Fake::default());
    fake.pending.push_back(verified(7, 294100, true));
    machine.tick(&mut fake, false);
    machine.tick(&mut fake, true);
    assert_eq!(outcomes(&machine.take_events()), [Outcome::Unknown]);
    fake.pending
        .push_back(completed(Action::Subscribe, 294100, 7, true));
    machine.tick(&mut fake, false);
    assert!(machine.take_events().is_empty());
    assert_eq!(fake.mutations.len(), 1);
}

#[test]
fn unsubscribe_acknowledgement_does_not_prove_file_removal() {
    let (mut machine, mut fake) = drive(
        Action::Unsubscribe,
        Fake {
            old_copy: true,
            ..Fake::default()
        },
    );
    fake.pending.push_back(verified(7, 294100, true));
    machine.tick(&mut fake, false);
    fake.pending
        .push_back(completed(Action::Unsubscribe, 294100, 7, true));
    machine.tick(&mut fake, false);
    let events = machine.take_events();
    assert_eq!(outcomes(&events), [Outcome::Acknowledged]);
    assert!(events.iter().any(|e| matches!(
        &e.payload,
        Payload::Observation {
            phase: Phase::After,
            facts: Facts {
                subscription: Subscription::Subscribed,
                install: Some(_),
            },
            ..
        }
    )));
}

#[test]
fn inspect_is_read_only_and_output_passes_protocol_validator() {
    let (mut machine, mut fake) = drive(
        Action::Inspect,
        Fake {
            old_copy: true,
            ..Fake::default()
        },
    );
    fake.pending.push_back(verified(7, 294100, true));
    machine.tick(&mut fake, false);
    assert!(fake.mutations.is_empty());
    let events = machine.take_events();
    assert_eq!(outcomes(&events), [Outcome::Observed]);
    let mut stream = EventStream::new(request(Action::Inspect)).unwrap();
    for event in events {
        stream.accept_line(&encode_event(&event).unwrap()).unwrap();
    }
    stream.finish().unwrap();
}

#[test]
fn verified_download_and_current_install_is_installed() {
    let (mut machine, mut fake) = drive(
        Action::Download,
        Fake {
            old_copy: true,
            fresh: true,
            ..Fake::default()
        },
    );
    fake.pending.push_back(verified(7, 294100, true));
    machine.tick(&mut fake, false);
    fake.pending
        .push_back(completed(Action::Download, 294100, 7, true));
    machine.tick(&mut fake, false);
    assert_eq!(outcomes(&machine.take_events()), [Outcome::Installed]);
}

#[test]
fn notifications_already_queued_before_start_cannot_complete_new_download() {
    let (mut machine, mut fake) = drive(
        Action::Download,
        Fake {
            old_copy: true,
            fresh: true,
            ..Fake::default()
        },
    );
    fake.pending.push_back(verified(7, 294100, true));
    fake.pending
        .push_back(completed(Action::Download, 294100, 7, true));
    machine.tick(&mut fake, false);
    assert!(
        !machine.is_done(),
        "the download had not started when callbacks were pumped"
    );
    fake.pending
        .push_back(completed(Action::Download, 294100, 7, true));
    machine.tick(&mut fake, false);
    assert_eq!(outcomes(&machine.take_events()), [Outcome::Installed]);
}

#[test]
fn immediate_rejection_and_query_failure_produce_failed_terminal_results() {
    let (mut machine, mut fake) = drive(
        Action::Download,
        Fake {
            reject: true,
            old_copy: true,
            ..Fake::default()
        },
    );
    fake.pending.push_back(verified(7, 294100, true));
    machine.tick(&mut fake, false);
    assert!(machine.is_done());
    assert_eq!(outcomes(&machine.take_events()), [Outcome::Failed]);
    let (mut machine, mut fake) = drive(Action::Subscribe, Fake::default());
    fake.pending.push_back(Callback::Verified {
        item_id: 7,
        result: Err(ApiError::new(
            FailureStage::Verify,
            FailureKind::MetadataQueryFailed,
            "Metadata query unavailable",
        )),
    });
    machine.tick(&mut fake, false);
    assert!(machine.is_done());
    assert!(fake.mutations.is_empty());
    assert_eq!(outcomes(&machine.take_events()), [Outcome::Failed]);
}

#[test]
fn duplicate_callbacks_do_not_duplicate_facts_or_mutations() {
    let (mut machine, mut fake) = drive(Action::Subscribe, Fake::default());
    fake.pending.push_back(verified(7, 294100, true));
    fake.pending.push_back(verified(7, 294100, true));
    machine.tick(&mut fake, false);
    fake.pending
        .push_back(completed(Action::Subscribe, 294100, 7, true));
    fake.pending
        .push_back(completed(Action::Subscribe, 294100, 7, true));
    machine.tick(&mut fake, false);
    assert_eq!(fake.mutations.len(), 1);
    let events = machine.take_events();
    assert_eq!(events.len(), 4);
    assert_eq!(outcomes(&events), [Outcome::Acknowledged]);
}

#[test]
fn bounds_outstanding_calls_and_rejects_invalid_before_api_calls() {
    let mut req = request(Action::Subscribe);
    req.ids = (1..=21).collect();
    let mut fake = Fake::default();
    assert!(Machine::new(req.clone(), &mut fake).is_err());
    assert!(fake.queries.is_empty());
    req.ids.pop();
    let mut machine = Machine::new(req, &mut fake).unwrap();
    assert_eq!(fake.queries.len(), 20);
    machine.tick(&mut fake, true);
    assert_eq!(outcomes(&machine.take_events()), vec![Outcome::Unknown; 20]);
}

#[test]
fn native_diagnostics_remain_item_correlated_and_survive_missing_terminal() {
    for action in [Action::Subscribe, Action::Unsubscribe, Action::Download] {
        let mut req = request(action);
        req.ids = vec![7, 8];
        let mut fake = Fake {
            old_copy: true,
            fresh: true,
            ..Fake::default()
        };
        let mut machine = Machine::new(req.clone(), &mut fake).unwrap();
        fake.pending
            .extend([verified(7, APP_ID, true), verified(8, APP_ID, true)]);
        machine.tick(&mut fake, false);
        let failures = [(8, "Network failure"), (7, "Access denied")];
        for (item_id, message) in failures {
            fake.pending.push_back(Callback::Completed {
                action,
                app_id: APP_ID,
                item_id,
                result: Err(ApiError::new(
                    FailureStage::Complete,
                    FailureKind::NativeFailure,
                    message,
                )),
            });
        }
        machine.tick(&mut fake, false);
        let events = machine.take_events();
        for (id, message) in failures {
            assert!(events.iter().any(|event| matches!(&event.payload,
                Payload::Outcome { item_id, outcome: Outcome::Failed, detail: Some(detail) }
                    if *item_id == id && detail == &FailureDetail::new(FailureStage::Complete, FailureKind::NativeFailure, message))));
        }
        let mut stream = EventStream::new(req).unwrap();
        for event in events
            .iter()
            .filter(|e| !matches!(e.payload, Payload::Terminal))
        {
            stream.accept_line(&encode_event(event).unwrap()).unwrap();
        }
        assert!(stream.finish().is_err());
        assert_eq!(stream.events(), &events[..events.len() - 1]);
        assert_eq!(fake.mutations.len(), 2);
    }
}

#[test]
fn verification_diagnostics_preserve_exposed_causes_without_inventing_hidden_ones() {
    for (kind, message) in [
        (FailureKind::MetadataQueryFailed, "Network failure"),
        (
            FailureKind::MetadataQueryFailed,
            "Metadata response is missing or unusable",
        ),
        (
            FailureKind::InvalidItemIdentity,
            "Metadata response has a different item ID",
        ),
    ] {
        let detail = ApiError::new(FailureStage::Verify, kind, message);
        let (mut machine, mut fake) = drive(Action::Download, Fake::default());
        fake.pending.push_back(Callback::Verified {
            item_id: 7,
            result: Err(detail.clone()),
        });
        machine.tick(&mut fake, false);
        let events = machine.take_events();
        assert!(events.iter().any(|event| matches!(&event.payload,
            Payload::Outcome { item_id: 7, outcome: Outcome::Failed, detail: Some(actual) } if actual == &detail)));
        assert!(fake.mutations.is_empty());
    }
}

#[test]
fn query_failures_have_verify_diagnostics() {
    for immediate in [true, false] {
        let (mut machine, mut fake) = drive(
            Action::Download,
            Fake {
                reject_query: immediate,
                ..Fake::default()
            },
        );
        if !immediate {
            fake.pending.push_back(Callback::Verified {
                item_id: 7,
                result: Err(ApiError::new(
                    FailureStage::Verify,
                    FailureKind::MetadataQueryFailed,
                    "Metadata query unavailable",
                )),
            });
            machine.tick(&mut fake, false);
        }
        assert!(fake.mutations.is_empty());
        assert_failure(
            &machine.take_events(),
            Outcome::Failed,
            "verify",
            "metadata_query_failed",
        );
    }
}

#[test]
fn invalid_identity_has_distinct_verify_diagnostic() {
    for (app, community) in [(42, true), (APP_ID, false)] {
        let (mut machine, mut fake) = drive(Action::Download, Fake::default());
        fake.pending.push_back(verified(7, app, community));
        machine.tick(&mut fake, false);
        assert!(fake.mutations.is_empty());
        assert_failure(
            &machine.take_events(),
            Outcome::Failed,
            "verify",
            "invalid_item_identity",
        );
    }
}

#[test]
fn rejected_start_has_request_diagnostic_and_retains_old_copy() {
    let (mut machine, mut fake) = drive(
        Action::Download,
        Fake {
            reject: true,
            old_copy: true,
            ..Fake::default()
        },
    );
    fake.pending.push_back(verified(7, APP_ID, true));
    machine.tick(&mut fake, false);
    let events = machine.take_events();
    assert_failure(&events, Outcome::Failed, "request", "request_rejected");
    assert!(events.iter().any(|event| matches!(&event.payload,
        Payload::Observation { phase: Phase::After, facts, .. } if facts == &fake.observe(7))));
}

#[test]
fn failed_callback_has_complete_diagnostic_even_with_current_copy() {
    let (mut machine, mut fake) = drive(
        Action::Download,
        Fake {
            old_copy: true,
            fresh: true,
            ..Fake::default()
        },
    );
    fake.pending.push_back(verified(7, APP_ID, true));
    machine.tick(&mut fake, false);
    fake.pending
        .push_back(completed(Action::Download, APP_ID, 7, false));
    machine.tick(&mut fake, false);
    let events = machine.take_events();
    assert_failure(&events, Outcome::Failed, "complete", "native_failure");
    assert!(events.iter().any(|event| matches!(&event.payload,
        Payload::Observation { phase: Phase::After, facts, .. } if facts == &fake.observe(7))));
}

#[test]
fn timeouts_identify_verify_or_complete_stage_without_retry() {
    for running in [false, true] {
        let (mut machine, mut fake) = drive(
            Action::Download,
            Fake {
                old_copy: true,
                fresh: true,
                ..Fake::default()
            },
        );
        if running {
            fake.pending.push_back(verified(7, APP_ID, true));
            machine.tick(&mut fake, false);
        }
        machine.tick(&mut fake, true);
        assert_failure(
            &machine.take_events(),
            Outcome::Unknown,
            if running { "complete" } else { "verify" },
            "timed_out",
        );
        assert_eq!(fake.mutations.len(), usize::from(running));
    }
}

#[test]
fn successful_download_without_verified_current_install_has_distinct_detail() {
    for old_copy in [false, true] {
        let (mut machine, mut fake) = drive(
            Action::Download,
            Fake {
                old_copy,
                ..Fake::default()
            },
        );
        fake.pending.push_back(verified(7, APP_ID, true));
        machine.tick(&mut fake, false);
        fake.pending
            .push_back(completed(Action::Download, APP_ID, 7, true));
        machine.tick(&mut fake, false);
        assert_failure(
            &machine.take_events(),
            Outcome::Unknown,
            "complete",
            "unverified_install",
        );
    }
}
