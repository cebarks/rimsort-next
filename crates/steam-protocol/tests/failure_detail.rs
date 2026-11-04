use steam_protocol::*;

fn stream(action: Action, installed: bool) -> EventStream {
    let mut stream = EventStream::new(Request {
        schema: 1,
        app_id: APP_ID,
        request_id: "detail".into(),
        action,
        ids: vec![7],
    })
    .unwrap();
    for phase in [Phase::Before, Phase::After] {
        let facts = Facts {
            subscription: Subscription::Unknown,
            install: installed.then(|| Install {
                path: std::env::current_dir()
                    .unwrap()
                    .join(std::env::temp_dir())
                    .join("synthetic-steam-mod")
                    .to_str()
                    .unwrap()
                    .into(),
                size_bytes: 1,
                timestamp: 1,
                freshness: Freshness::Current,
            }),
        };
        stream
            .accept_line(
                &encode_event(&Event::new(
                    "detail",
                    Payload::Observation {
                        item_id: 7,
                        phase,
                        facts,
                    },
                ))
                .unwrap(),
            )
            .unwrap();
    }
    stream
}

fn wire(outcome: &str, detail: Option<serde_json::Value>) -> Vec<u8> {
    let mut payload = serde_json::json!({"kind":"outcome", "item_id":7, "outcome":outcome});
    if let Some(detail) = detail {
        payload["detail"] = detail;
    }
    let event =
        serde_json::json!({"schema":1, "app_id":APP_ID, "request_id":"detail", "payload":payload});
    let mut bytes = PREFIX.to_vec();
    bytes.extend(serde_json::to_vec(&event).unwrap());
    bytes.push(b'\n');
    bytes
}

fn detail(message: &str) -> serde_json::Value {
    serde_json::json!({"stage":"verify", "kind":"metadata_query_failed", "message":message})
}

#[test]
fn failed_and_unknown_require_detail_without_advancing_on_rejection() {
    for outcome in ["failed", "unknown"] {
        for missing in [None, Some(serde_json::Value::Null)] {
            let mut stream = stream(Action::Inspect, false);
            assert!(
                stream.accept_line(&wire(outcome, missing)).is_err(),
                "{outcome} requires detail"
            );
            assert_eq!(stream.events().len(), 2);
            stream
                .accept_line(&wire(outcome, Some(detail("query unavailable"))))
                .unwrap();
            assert!(stream.finish().is_err());
            assert_eq!(stream.events().len(), 3);
        }
    }
}

#[test]
fn successful_outcomes_forbid_failure_detail() {
    for (action, outcome, installed) in [
        (Action::Inspect, "observed", false),
        (Action::Subscribe, "acknowledged", false),
        (Action::Unsubscribe, "acknowledged", false),
        (Action::Download, "installed", true),
    ] {
        let mut stream = stream(action, installed);
        assert!(
            stream
                .accept_line(&wire(outcome, Some(detail("failure"))))
                .is_err()
        );
        stream
            .accept_line(&wire(outcome, Some(serde_json::Value::Null)))
            .unwrap();
    }
}

#[test]
fn decoder_accepts_bounded_unicode_details_and_preserves_them_on_eof() {
    for message in ["x".repeat(512), "é".repeat(256), "界".repeat(170)] {
        let mut stream = stream(Action::Inspect, false);
        let event = stream
            .accept_line(&wire("failed", Some(detail(&message))))
            .unwrap()
            .unwrap();
        let json = serde_json::to_value(event).unwrap();
        assert_eq!(json["payload"]["detail"]["message"], message);
        assert_eq!(stream.events().len(), 3);
        assert!(stream.finish().is_err());
    }
}

#[test]
fn producer_bounds_sanitizes_and_preserves_utf8_character_boundaries() {
    let mut mismatches = Vec::new();
    for (message, expected) in [
        ("".into(), "No diagnostic available".into()),
        (
            "native\nerror\r\t\0\u{7f}\u{85}\u{2028}\u{2029}".into(),
            "native error       ".into(),
        ),
        ("x".repeat(513), "x".repeat(512)),
        (format!("{}界", "a".repeat(511)), "a".repeat(511)),
        ("é".repeat(257), "é".repeat(256)),
        ("界".repeat(171), "界".repeat(170)),
    ] {
        let bounded = FailureDetail::new(
            FailureStage::Verify,
            FailureKind::MetadataQueryFailed,
            &message,
        );
        if bounded.message != expected {
            mismatches.push(format!(
                "input {message:?}: got {:?}, expected {expected:?}",
                bounded.message
            ));
        }
        if bounded.message == expected {
            assert!(bounded.message.len() <= 512);
            stream(Action::Inspect, false)
                .accept_line(&wire(
                    "failed",
                    Some(serde_json::to_value(bounded).unwrap()),
                ))
                .unwrap();
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[test]
fn decoder_rejects_invalid_messages_fields_and_categories() {
    let mut invalid: Vec<_> = [
        "".into(),
        "x".repeat(513),
        "é".repeat(257),
        "a\nb".into(),
        "a\rb".into(),
        "a\tb".into(),
        "a\0b".into(),
        "a\u{7f}b".into(),
        "a\u{85}b".into(),
        "a\u{2028}b".into(),
        "a\u{2029}b".into(),
    ]
    .iter()
    .map(|s| detail(s))
    .collect();
    for (key, value) in [
        ("stage", "surprise"),
        ("kind", "surprise"),
        ("extra", "surprise"),
    ] {
        let mut bad = detail("query unavailable");
        bad[key] = value.into();
        invalid.push(bad);
    }
    for key in ["stage", "kind", "message"] {
        let mut bad = detail("query unavailable");
        bad.as_object_mut().unwrap().remove(key);
        invalid.push(bad);
    }
    let mut accepted = Vec::new();
    for bad in invalid {
        let mut stream = stream(Action::Inspect, false);
        if stream
            .accept_line(&wire("failed", Some(bad.clone())))
            .is_ok()
        {
            accepted.push(bad);
        } else {
            assert_eq!(stream.events().len(), 2);
        }
    }
    assert!(
        accepted.is_empty(),
        "accepted invalid details: {accepted:?}"
    );
}
