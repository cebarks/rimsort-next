use super::*;

pub(crate) fn large_preview_confirms_only_current_batch() {
    let (root, ws) = fixture();
    let exe = super::super::native_acquisition_tests::helper(root.path(), "");
    super::super::native_acquisition_tests::configure(&ws, &exe);
    let ids: Vec<u64> = (1..=10_000).collect();
    let mut responses = Vec::new();
    for chunk in ids.chunks(100) {
        let details: Vec<_> = chunk.iter().map(|id| serde_json::json!({"publishedfileid":id.to_string(),"result":1,"consumer_app_id":294100,"title":"t".repeat(128)})).collect();
        let collections: Vec<_> = chunk
            .iter()
            .map(|id| serde_json::json!({"publishedfileid":id.to_string(),"result":9}))
            .collect();
        responses
            .push(serde_json::json!({"response":{"publishedfiledetails":details}}).to_string());
        responses
            .push(serde_json::json!({"response":{"collectiondetails":collections}}).to_string());
    }
    preview_with(
        &ws,
        &ids.iter().map(u64::to_string).collect::<Vec<_>>().join(" "),
        responses,
    );
    assert!(!root.path().join("requests").exists());
    ws.with_acquisition(|w| {
        w.invoke_acquire();
        assert!(
            w.get_question().contains("20 candidates"),
            "large preview must explicitly confirm only its current bounded batch"
        );
        let token = w.get_question_token();
        let captured = w.get_question();
        let page = w.get_page_status();
        assert!(page.contains("1–20 of 10000"));
        assert!(page.contains("9980 remaining"));
        w.invoke_change_page(1);
        assert_eq!(
            w.get_page_status(),
            page,
            "navigation cannot retarget confirmation"
        );
        assert_eq!(w.get_question(), captured);
        w.invoke_answer_question(token, "confirm".into());
        w.invoke_change_page(1);
        assert_eq!(
            w.get_page_status(),
            page,
            "navigation cannot retarget running work"
        );
    });
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    let history = root.path().join("private/last-acquisition.json");
    let first: serde_json::Value = serde_json::from_slice(&fs::read(&history).unwrap()).unwrap();
    assert_eq!(first["candidates"].as_array().unwrap().len(), 20);
    assert_eq!(first["native"]["items"].as_array().unwrap().len(), 20);
    assert_eq!(first["report"]["items"].as_array().unwrap().len(), 20);
    assert!(
        !first["preview_diagnostics"]
            .as_str()
            .unwrap()
            .contains(&"t".repeat(128))
    );
    let requests = root.path().join("requests");
    let read_requests = || {
        fs::read_to_string(&requests)
            .unwrap()
            .lines()
            .map(|s| serde_json::from_str::<serde_json::Value>(s).unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(read_requests().len(), 1);
    assert_eq!(
        read_requests()[0]["ids"],
        serde_json::json!((1..=20).collect::<Vec<u64>>())
    );
    ws.with_acquisition(|w| {
        w.invoke_change_page(1);
        assert!(w.get_page_status().contains("21–40 of 10000"));
        w.invoke_change_page(-1);
        assert!(w.get_page_status().contains("1–20 of 10000"));
        w.invoke_change_page(1);
    });
    assert_eq!(
        read_requests().len(),
        1,
        "navigation cannot dispatch automatically"
    );
    super::super::native_acquisition_tests::start(&ws, "subscribe");
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    let original: serde_json::Value = serde_json::from_slice(&fs::read(&history).unwrap()).unwrap();
    assert_eq!(read_requests().len(), 2);
    assert_eq!(
        read_requests()[1]["ids"],
        serde_json::json!((21..=40).collect::<Vec<u64>>())
    );
    assert_eq!(
        original["native"]["pending"][0]["items"]
            .as_array()
            .unwrap()
            .len(),
        20
    );
    assert!(ws.state.app.lock().unwrap().acquisition.launch_blocked());
    super::super::native_acquisition_tests::helper(
        root.path(),
        r#"
for i in r['ids']:
 for phase in ['before','after']:
  emit(dict(kind='observation',item_id=i,phase=phase,facts=dict(subscription='subscribed',install=None)))
 emit(dict(kind='outcome',item_id=i,outcome='observed',detail=None))
emit(dict(kind='terminal'))
sys.exit(0)
"#,
    );
    super::super::native_acquisition_tests::start(&ws, "inspect");
    wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
    let resolved: serde_json::Value = serde_json::from_slice(&fs::read(&history).unwrap()).unwrap();
    assert_eq!(read_requests().len(), 3);
    assert_eq!(resolved["native"]["items"].as_array().unwrap().len(), 20);
    assert_eq!(
        resolved["native"]["pending"][0]["items"],
        original["native"]["items"]
    );
    assert_eq!(
        resolved["native"]["pending"][0]["runs"],
        original["native"]["runs"]
    );
    assert!(!ws.state.app.lock().unwrap().acquisition.launch_blocked());
    ws.with_window(|w| w.invoke_close_window());
    assert!(!ws.with_window(|w| w.window().is_visible()));
    let next = reopen(&ws, root.path());
    wait(&next, |w| {
        w.installed() && w.with_acquisition(|a| !a.get_busy())
    });
    assert!(!next.state.app.lock().unwrap().acquisition.launch_blocked());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(history).unwrap()).unwrap(),
        resolved
    );
}
