use std::io::Cursor;
use steam_protocol::*;

fn request() -> Request {
    parse_request(
        br#"{"schema":1,"app_id":294100,"request_id":"test-1","action":"inspect","ids":[1,2]}"#,
    )
    .unwrap()
}

#[test]
fn requests_are_strict_and_bounded() {
    let good =
        br#"{"schema":1,"app_id":294100,"request_id":"test-1","action":"inspect","ids":[1,2]}"#;
    assert_eq!(request().ids, vec![1, 2]);
    for (from, to) in [
        ("\"schema\":1", "\"schema\":2"),
        ("294100", "123"),
        ("test-1", ""),
        ("inspect", "delete"),
        ("[1,2]", "[0,2]"),
        ("[1,2]", "[1,1]"),
        ("[1,2]", "[]"),
        ("\"ids\"", "\"extra\":0,\"ids\""),
    ] {
        let bad = String::from_utf8(good.to_vec()).unwrap().replace(from, to);
        assert!(parse_request(bad.as_bytes()).is_err(), "{bad}");
    }
    let mut req = request();
    req.ids = (1..=21).collect();
    assert!(req.validate().is_err());
    req = request();
    req.request_id = "a".repeat(129);
    assert!(req.validate().is_err());
    assert!(read_request(Cursor::new(vec![b' '; 1_048_577])).is_err());
    let mut exact = good.to_vec();
    exact.resize(1_048_576, b' ');
    assert!(read_request(Cursor::new(exact)).is_ok());
}

fn fact(id: u64, phase: Phase) -> Event {
    Event::new(
        "test-1",
        Payload::Observation {
            item_id: id,
            phase,
            facts: Facts {
                subscription: Subscription::Unknown,
                install: None,
            },
        },
    )
}
fn outcome(id: u64) -> Event {
    Event::new(
        "test-1",
        Payload::Outcome {
            item_id: id,
            outcome: Outcome::Unknown,
            detail: Some(FailureDetail::new(
                FailureStage::Complete,
                FailureKind::TimedOut,
                "timed out",
            )),
        },
    )
}
fn line(e: &Event) -> Vec<u8> {
    encode_event(e).unwrap()
}

#[test]
fn diagnostics_are_not_events_and_lines_are_bounded() {
    let mut stream = EventStream::new(request()).unwrap();
    assert!(
        stream
            .accept_line(b"Steam diagnostic { \"schema\":1 }\n")
            .unwrap()
            .is_none()
    );
    assert!(stream.accept_line(&vec![b'x'; 262_145]).is_err());
    assert!(read_line_bounded(&mut Cursor::new(vec![b'x'; 262_145])).is_err());
    assert!(read_line_bounded(&mut Cursor::new(vec![b'x'; 262_144])).is_ok());
    assert!(stream.accept_line(b"RIMSORT_STEAM_V1 {garbage}\n").is_err());
}

#[test]
fn rejects_wrong_duplicate_late_events_and_requires_terminal() {
    let mut stream = EventStream::new(request()).unwrap();
    let first = fact(1, Phase::Before);
    assert!(stream.accept_line(&line(&first)).unwrap().is_some());
    assert!(stream.finish().is_err()); // facts remain usable on crash
    assert_eq!(stream.events().len(), 1);
    assert!(stream.accept_line(&line(&first)).is_err());
    let mut wrong = fact(2, Phase::Before);
    wrong.request_id = "another".into();
    assert!(stream.accept_line(&line(&wrong)).is_err());
    wrong = fact(3, Phase::Before);
    assert!(stream.accept_line(&line(&wrong)).is_err());
    wrong = fact(2, Phase::Before);
    wrong.app_id = 42;
    assert!(stream.accept_line(&line(&wrong)).is_err());
    let terminal = Event::new("test-1", Payload::Terminal);
    assert!(stream.accept_line(&line(&terminal)).is_err());
    for e in [
        fact(1, Phase::After),
        outcome(1),
        fact(2, Phase::Before),
        fact(2, Phase::After),
        outcome(2),
        terminal.clone(),
    ] {
        stream.accept_line(&line(&e)).unwrap();
    }
    stream.finish().unwrap();
    assert!(stream.accept_line(&line(&terminal)).is_err());
    assert!(stream.accept_line(&line(&outcome(1))).is_err());
}

#[test]
fn subscription_ack_is_independent_of_observation_and_survives_missing_terminal() {
    for action in [Action::Subscribe, Action::Unsubscribe] {
        for subscription in [
            Subscription::Unknown,
            Subscription::NotSubscribed,
            Subscription::Subscribed,
        ] {
            let mut req = request();
            req.ids = vec![1];
            req.action = action;
            let mut stream = EventStream::new(req).unwrap();
            let before = fact(1, Phase::Before);
            let after = Event::new(
                "test-1",
                Payload::Observation {
                    item_id: 1,
                    phase: Phase::After,
                    facts: Facts {
                        subscription,
                        install: None,
                    },
                },
            );
            let ack = Event::new(
                "test-1",
                Payload::Outcome {
                    item_id: 1,
                    outcome: Outcome::Acknowledged,
                    detail: None,
                },
            );
            for event in [&before, &after, &ack] {
                stream.accept_line(&line(event)).unwrap();
            }
            assert!(stream.finish().is_err());
            assert_eq!(stream.events(), &[before, after, ack]);
            stream
                .accept_line(&line(&Event::new("test-1", Payload::Terminal)))
                .unwrap();
            stream.finish().unwrap();
        }
    }
}

#[test]
fn inspect_and_download_cannot_report_subscription_acknowledgement() {
    for action in [Action::Inspect, Action::Download] {
        for subscription in [
            Subscription::Unknown,
            Subscription::NotSubscribed,
            Subscription::Subscribed,
        ] {
            let mut req = request();
            req.ids = vec![1];
            req.action = action;
            let mut stream = EventStream::new(req).unwrap();
            let before = fact(1, Phase::Before);
            let after = Event::new(
                "test-1",
                Payload::Observation {
                    item_id: 1,
                    phase: Phase::After,
                    facts: Facts {
                        subscription,
                        install: None,
                    },
                },
            );
            for event in [&before, &after] {
                stream.accept_line(&line(event)).unwrap();
            }
            let ack = Event::new(
                "test-1",
                Payload::Outcome {
                    item_id: 1,
                    outcome: Outcome::Acknowledged,
                    detail: None,
                },
            );
            assert!(stream.accept_line(&line(&ack)).is_err());
            assert_eq!(stream.events(), &[before, after]);
            assert!(stream.finish().is_err());
        }
    }
}

#[test]
fn unknown_event_fields_and_invalid_fact_paths_are_rejected() {
    let mut stream = EventStream::new(request()).unwrap();
    let encoded = String::from_utf8(line(&fact(1, Phase::Before))).unwrap();
    let bad = encoded.replace("\"schema\":1", "\"schema\":1,\"extra\":3");
    assert!(stream.accept_line(bad.as_bytes()).is_err());
    let bad = encoded.replace("\"item_id\":1", "\"item_id\":1,\"extra\":3");
    assert!(stream.accept_line(bad.as_bytes()).is_err());
    let invalid = Event::new(
        "test-1",
        Payload::Observation {
            item_id: 1,
            phase: Phase::Before,
            facts: Facts {
                subscription: Subscription::Subscribed,
                install: Some(Install {
                    path: "".into(),
                    size_bytes: 10,
                    timestamp: 1,
                    freshness: Freshness::Unknown,
                }),
            },
        },
    );
    assert!(stream.accept_line(&line(&invalid)).is_err());
}
