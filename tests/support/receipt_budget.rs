use super::*;
use serde_json::{Value, json};

fn input(native: bool) -> FinalReport {
    serde_json::from_value(json!({
        "schema_version":1,"request_id":"9".repeat(52),"source_generation":u64::MAX,"source_revision":u64::MAX,
        "backend":if native {"SteamClient"} else {"SteamCmd"},"mods_root":"/fixture/Mods","executable":"/fixture/fake",
        "timeout_seconds":86400,"target_version":"1.6","requested_at":u64::MAX,"returned_at":null,
        "candidates":(1..=20).map(|id| json!({"id":id,"title":"title","app_id":294100,"kind":"ItemCandidate","upstream_updated":u64::MAX})).collect::<Vec<_>>(),
        "preview_diagnostics":"Actual preview diagnostic","error":null,
        "report":{"items":[],"logs":[],"logs_truncated":false,"runs":[],"warnings":[],"recovery_required":null,"cleanup_error":null},
        "native":native.then(|| NativeReport::new(Action::Download, None))
    })).unwrap()
}

// Independent serialized result oracle: protocol maxima, rather than calling the
// admission shape generator. Includes inherited originals plus another uncertain run.
fn maximum_result(input: &FinalReport) -> Value {
    let mut value = serde_json::to_value(input).unwrap();
    let message = "m".repeat(4094);
    let path = format!("/{}", "\\".repeat(4095));
    value["returned_at"] = json!(u64::MAX);
    value["error"] = json!(message);
    value["report"]["warnings"] = json!(["w".repeat(16380)]);
    value["report"]["cleanup_error"] = json!(message);
    value["report"]["recovery_required"] = json!(message);
    value["report"]["items"] = json!((1..=20).map(|id| {
        let copies = if input.native.is_some() { vec![path.clone();2] } else { vec![format!("/fixture/Mods/{id}")] };
        json!({"item_id":id,"subscription":"NotSubscribed","installations":copies.into_iter().map(|p|json!({"backend":"SteamClient","item_id":id,"path":p,"freshness":"UpdateAvailable"})).collect::<Vec<_>>(),"download":"NotStarted","outcome":"Cancelled","diagnostics":["d".repeat(16380)]})
    }).collect::<Vec<_>>());
    if let Some(native) = &input.native {
        let request = format!("{}-0", input.request_id);
        let facts = json!({"subscription":"not_subscribed","install":{"path":path,"size_bytes":u64::MAX,"timestamp":u32::MAX,"freshness":"needs_update"}});
        let items: Vec<_> = (1..=20).map(|id|json!({"item_id":id,"request_id":request,"before":facts,"after":facts,"acknowledgement":false,"outcome":"unknown","failure":{"stage":"initialize","kind":"metadata_query_failed","message":"\\".repeat(512)},"validation_errors":[message,message]})).collect();
        let runs = json!([{"request_id":request,"status":"ProcessFailed","protocol_complete":false,"protocol_error":message,"error":message,"input_error":message,"exit_status":message,"spawned":true,"leader_reaped":false,"stdin_closed":false,"stdout_eof":false,"stderr_eof":false,"termination_requested":true,"termination_error":message,"detached_workers":usize::MAX,"cleanup_uncertain":true}]);
        let mut pending: Vec<Value> = native
            .pending
            .iter()
            .map(|p| serde_json::to_value(p).unwrap())
            .collect();
        for p in &mut pending {
            p["resolved_by"] = json!(format!("Inspect {}", input.request_id));
        }
        pending.push(json!({"request_id":input.request_id,"action":"download","items":items,"runs":runs,"resolved_by":ACKNOWLEDGEMENT}));
        value["native"]["items"] = json!(items);
        value["native"]["runs"] = runs;
        value["native"]["pending"] = json!(pending);
        value["native"]["diagnostics"] = json!(vec!["\u{1}".repeat(262); 1000]);
        value["native"]["dropped_lines"] = json!(usize::MAX);
        value["native"]["dropped_bytes"] = json!(usize::MAX);
    } else {
        value["report"]["runs"] = json!(["/fixture/workspace/run-abcdefghijklmnop"]);
    }
    value
}

#[test]
fn admitted_maximum_results_and_resolutions_fit_atomic_cap_at_boundary() {
    for native in [false, true] {
        for carried in [false, true] {
            let mut input = input(native);
            if native && carried {
                let original = maximum_result(&input);
                input.native.as_mut().unwrap().pending =
                    serde_json::from_value(original["native"]["pending"].clone()).unwrap();
                input.native.as_mut().unwrap().pending[0].resolved_by = None;
            }
            let workspace = (!native).then_some(Path::new("/fixture/workspace"));
            let reserved = required_bytes(&input, workspace).unwrap();
            assert!(
                reserved < 4 * 1024 * 1024,
                "normal20 plus originals must remain usable: {reserved}"
            );
            input.candidates[0]
                .title
                .push_str(&"x".repeat(4 * 1024 * 1024 - reserved));
            assert_eq!(required_bytes(&input, workspace).unwrap(), 4 * 1024 * 1024);
            check(&input, workspace).unwrap();
            let result = maximum_result(&input);
            let bytes = serde_json::to_vec(&result).unwrap();
            assert!(
                bytes.len() <= 4 * 1024 * 1024,
                "admitted result exceeded cap: {}",
                bytes.len()
            );
            let root = tempfile::tempdir().unwrap();
            let file = root.path().join("receipt.json");
            super::super::write(&file, &result).unwrap();
            assert_eq!(super::super::read::<Value>(&file).unwrap().unwrap(), result);
            input.candidates[0].title.push('x');
            assert!(
                check(&input, workspace)
                    .unwrap_err()
                    .contains("before dispatch")
            );
            assert_eq!(std::fs::read(file).unwrap(), bytes);
        }
    }
}

#[test]
fn relative_future_workspace_must_not_be_budgeted_without_its_absolute_prefix() {
    assert!(
        check(&input(false), Some(Path::new("relative/workspace"))).is_err(),
        "tempfile makes relative roots absolute, so the unknown cwd prefix must not escape admission"
    );
}

#[test]
fn prose_clipping_counts_encoded_bytes_and_keeps_utf8_and_presence() {
    for text in [
        "\u{1}".repeat(10000),
        "\\\"".repeat(10000),
        "日本語".repeat(10000),
    ] {
        let bounded = diagnostic(&text, MESSAGE_BYTES);
        assert!(serde_json::to_vec(&bounded).unwrap().len() <= 4096);
        assert!(bounded.contains("UTF-8 bytes omitted"));
        assert!(!bounded.is_empty());
        let mut lines = vec![text; 100];
        messages(&mut lines);
        assert!(serde_json::to_vec(&lines).unwrap().len() <= 16384);
        assert!(lines[0].contains("100 diagnostic messages"));
        assert!(lines[0].contains("UTF-8 bytes omitted"));
    }
}
