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
    pumps: usize,
}
impl SteamApi for Fake {
    fn query(&mut self, id: u64) -> Result<(), ApiError> {
        self.queries.push(id);
        Ok(())
    }
    fn observe(&self, _: u64) -> Facts {
        Facts {
            subscription: self.subscription.unwrap_or(Subscription::Subscribed),
            install: self.old_copy.then(|| Install {
                path: "/fixture/old-mod".into(),
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
        if self.reject { Err(ApiError) } else { Ok(()) }
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
        success,
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
                                path: "/fixture/old-mod".into(),
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
        result: Err(ApiError),
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
