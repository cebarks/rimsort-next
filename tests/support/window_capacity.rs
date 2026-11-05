use super::*;

pub(super) fn capacity_refuses_before_either_backend_and_preserves_origins() {
    let (root, ws) = fixture();
    prepared(&ws, root.path(), "", "900");
    let helper = super::super::native_acquisition_tests::helper(root.path(), "");
    super::super::native_acquisition_tests::configure(&ws, &helper);
    let history = root.path().join("private/last-acquisition.json");
    preview_with(&ws, "1", vec![
        serde_json::json!({"response":{"publishedfiledetails":[{"publishedfileid":"1","result":1,"consumer_app_id":294100,"title":format!("{} COLLECTION-METADATA-END", "c".repeat(70_000))}]}}).to_string(),
        r#"{"response":{"collectiondetails":[{"publishedfileid":"1","result":1,"children":[{"publishedfileid":"42","filetype":0}]}]}}"#.into(),
        r#"{"response":{"publishedfiledetails":[{"publishedfileid":"42","result":1,"consumer_app_id":294100,"title":"Candidate"}]}}"#.into(),
        r#"{"response":{"collectiondetails":[{"publishedfileid":"42","result":9}]}}"#.into(),
    ]);
    ws.with_acquisition(|w| {
        assert!(
            w.get_preview_text().contains("COLLECTION-METADATA-END"),
            "collection metadata is not diagnostic verbosity to discard"
        )
    });
    let huge = "x".repeat(4 * 1024 * 1024 - 2048);
    preview_with(&ws, "42", vec![
        serde_json::json!({"response":{"publishedfiledetails":[{"publishedfileid":"42","result":1,"consumer_app_id":294100,"title":huge}]}}).to_string(),
        r#"{"response":{"collectiondetails":[{"publishedfileid":"42","result":9}]}}"#.into(),
    ]);
    for backend in ["steamcmd", "steam-client"] {
        ws.with_acquisition(|w| {
            w.invoke_select_backend(backend.into());
            w.invoke_acquire();
            assert!(
                w.get_question_token().is_empty(),
                "capacity refusal must precede confirmation"
            );
            assert!(
                w.get_status()
                    .contains("Receipt capacity refused before dispatch"),
                "{}",
                w.get_status()
            );
        });
        assert!(!root.path().join("requests").exists());
        assert!(!root.path().join("private/acquisition-workspace").exists());
        assert!(!root.path().join("Mods/42").exists());
        assert!(!history.exists());
    }
    // A smaller, explicitly requested preview remains usable, not an automatic retry.
    preview(&ws);
    ws.with_acquisition(|w| w.invoke_select_backend("steamcmd".into()));
    start(&ws);
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    assert!(root.path().join("Mods/42/About/About.xml").is_file());
    ws.with_acquisition(|w| w.invoke_select_backend("steam-client".into()));
    super::super::native_acquisition_tests::start(&ws, "subscribe");
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    let mut legacy: serde_json::Value =
        serde_json::from_slice(&fs::read(&history).unwrap()).unwrap();
    // Legacy GUI operations could hold >20 identities in one PendingEffects. Use
    // the real returned unknown/acknowledged origin as the schema witness.
    let item = legacy["native"]["items"][0].clone();
    let run = legacy["native"]["runs"][0].clone();
    let origin = legacy["request_id"].as_str().unwrap().to_string();
    legacy["native"]["pending"][0]["items"] = serde_json::json!((0..300).map(|index| {
        let mut item = item.clone();
        item["item_id"] = serde_json::json!(1000+index);
        item["request_id"] = serde_json::json!(format!("{origin}-{}", index/20));
        let facts = serde_json::json!({"subscription":"unknown","install":{"path":format!("/{}", "a".repeat(2047)),"size_bytes":1,"timestamp":7,"freshness":"unknown"}});
        item["before"] = facts.clone(); item["after"] = facts;
        item
    }).collect::<Vec<_>>());
    legacy["native"]["pending"][0]["runs"] = serde_json::json!(
        (0..15)
            .map(|index| {
                let mut run = run.clone();
                run["request_id"] = serde_json::json!(format!("{origin}-{index}"));
                run
            })
            .collect::<Vec<_>>()
    );
    let bytes = serde_json::to_vec(&legacy).unwrap();
    assert!(bytes.len() < 4 * 1024 * 1024);
    fs::write(&history, &bytes).unwrap();
    let next = reopen(&ws, root.path());
    wait(&next, |w| {
        w.installed() && w.with_acquisition(|a| !a.get_busy())
    });
    next.with_acquisition(|w| w.invoke_select_backend("steam-client".into()));
    super::super::native_acquisition_tests::preview(&next, &(1..=40).collect::<Vec<_>>());
    let requests = fs::read(root.path().join("requests")).unwrap();
    next.with_acquisition(|w| {
        w.invoke_select_action("inspect".into());
        w.invoke_change_page(1);
        assert!(w.get_page_status().contains("21–40 of 40"));
        w.invoke_acquire();
        assert!(w.get_question_token().is_empty());
        assert!(
            w.get_status()
                .contains("Receipt capacity refused before dispatch")
        );
    });
    assert_eq!(fs::read(&history).unwrap(), bytes);
    assert_eq!(fs::read(root.path().join("requests")).unwrap(), requests);
    assert!(next.state.app.lock().unwrap().acquisition.launch_blocked());
    next.with_acquisition(|w| {
        w.invoke_acknowledge_native();
        w.invoke_answer_question(w.get_question_token(), "confirm".into());
    });
    wait(&next, |w| w.with_acquisition(|a| !a.get_busy()));
    let resolved: serde_json::Value = serde_json::from_slice(&fs::read(history).unwrap()).unwrap();
    assert_eq!(
        resolved["native"]["pending"][0]["items"],
        legacy["native"]["pending"][0]["items"]
    );
    assert_eq!(
        resolved["native"]["pending"][0]["runs"],
        legacy["native"]["pending"][0]["runs"]
    );
    assert!(resolved["native"]["pending"][0]["resolved_by"].is_string());
    assert_eq!(fs::read(root.path().join("requests")).unwrap(), requests);
}
