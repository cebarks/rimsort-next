use super::*;
use serde_json::json;
use std::{
    io::{Read, Write},
    net::TcpListener,
    thread,
};

fn server(responses: Vec<(u16, serde_json::Value)>) -> (WorkshopClient, thread::JoinHandle<()>) {
    let socket = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", socket.local_addr().unwrap());
    let worker = thread::spawn(move || {
        for (status, body) in responses {
            let (mut stream, _) = socket.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                .unwrap();
            let mut data = Vec::new();
            let end = loop {
                let mut b = [0];
                stream.read_exact(&mut b).unwrap();
                data.push(b[0]);
                if data.ends_with(b"\r\n\r\n") {
                    break data.len();
                }
            };
            let header = String::from_utf8_lossy(&data);
            assert!(header.starts_with("POST /ISteamRemoteStorage/Get"));
            let length = header
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap())
                })
                .unwrap();
            data.resize(end + length, 0);
            stream.read_exact(&mut data[end..]).unwrap();
            let form = String::from_utf8_lossy(&data[end..]);
            assert!(form.contains("publishedfileids"));
            let fields: Vec<_> = url::form_urlencoded::parse(form.as_bytes()).collect();
            let count = fields[0].1.parse::<usize>().unwrap();
            assert!(count <= 100);
            assert_eq!(fields.len(), count + 1);
            let body = body.to_string();
            write!(stream,"HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
    });
    (WorkshopClient::at(&base), worker)
}
fn details(id: u64, app: u64) -> serde_json::Value {
    json!({"response":{"result":1,"resultcount":1,"publishedfiledetails":[{"publishedfileid":id.to_string(),"result":1,"consumer_app_id":app,"title":"fixture","time_updated":7}]}})
}
fn ordinary(id: u64) -> serde_json::Value {
    json!({"response":{"result":1,"resultcount":0,"collectiondetails":[{"publishedfileid":id.to_string(),"result":9}]}})
}

#[test]
fn public_details_without_file_type_and_zero_count_error_row_resolve() {
    let (client, worker) = server(vec![(200, details(42, 294100)), (200, ordinary(42))]);
    let preview = client.resolve(&[PublishedFileId::try_from(42).unwrap()]);
    worker.join().unwrap();
    assert!(preview.diagnostics.is_empty(), "{:?}", preview.diagnostics);
    assert_eq!(preview.items.len(), 1);
    assert_eq!(preview.items[0].kind, WorkshopKind::ItemCandidate);
    assert_eq!(preview.items[0].upstream_updated, Some(7));
}
#[test]
fn foreign_app_and_missing_correlated_row_remain_errors() {
    for response in [details(42, 108600), details(99, 294100)] {
        let (client, worker) = server(vec![(200, response), (200, ordinary(42))]);
        let preview = client.resolve(&[PublishedFileId::try_from(42).unwrap()]);
        worker.join().unwrap();
        assert!(preview.items.is_empty());
        assert!(!preview.diagnostics.is_empty());
    }
}
#[test]
fn collection_cycles_partial_children_and_order_are_visible() {
    let collection = json!({"response":{"resultcount":1,"collectiondetails":[{"publishedfileid":"1","result":1,"children":[{"publishedfileid":"2","filetype":0},{"publishedfileid":"1","filetype":2},{"publishedfileid":"2","filetype":0},{"publishedfileid":"3","filetype":0}]}]}});
    let children = json!({"response":{"publishedfiledetails":[{"publishedfileid":"2","result":1,"consumer_app_id":294100,"title":"two"},{"publishedfileid":"3","result":9}]}});
    let noncollections = json!({"response":{"resultcount":0,"collectiondetails":[{"publishedfileid":"2","result":9},{"publishedfileid":"3","result":9}]}});
    let (client, worker) = server(vec![
        (200, details(1, 294100)),
        (200, collection),
        (200, children),
        (200, noncollections),
    ]);
    let preview = client.resolve(&[PublishedFileId::try_from(1).unwrap()]);
    worker.join().unwrap();
    assert_eq!(
        preview.items.iter().map(|i| i.id.get()).collect::<Vec<_>>(),
        vec![2]
    );
    assert!(
        preview
            .diagnostics
            .iter()
            .any(|d| d.message.contains("cycle"))
    );
    assert!(preview.diagnostics.iter().any(|d| d.item_id.get() == 3));
}
#[test]
fn known_collection_child_never_degrades_to_an_item_candidate() {
    let collection = json!({"response":{"collectiondetails":[{"publishedfileid":"1","result":1,"children":[{"publishedfileid":"2","filetype":2}]}]}});
    let (client, worker) = server(vec![
        (200, details(1, 294100)),
        (200, collection),
        (200, details(2, 294100)),
        (200, ordinary(2)),
    ]);
    let preview = client.resolve(&[PublishedFileId::try_from(1).unwrap()]);
    worker.join().unwrap();
    assert!(preview.items.is_empty());
    assert!(preview.diagnostics.iter().any(|d| d.item_id.get() == 2));
}

#[test]
fn selection_is_stably_deduplicated_and_requests_are_batched_at_100() {
    let mut responses = Vec::new();
    for range in [1..=100, 101..=101] {
        let details:Vec<_>=range.clone().map(|id|json!({"publishedfileid":id.to_string(),"result":1,"consumer_app_id":294100,"title":"item"})).collect();
        let collections: Vec<_> = range
            .map(|id| json!({"publishedfileid":id.to_string(),"result":9}))
            .collect();
        responses.push((200, json!({"response":{"publishedfiledetails":details}})));
        responses.push((
            200,
            json!({"response":{"resultcount":0,"collectiondetails":collections}}),
        ));
    }
    let (client, worker) = server(responses);
    let mut ids: Vec<_> = (1..=101)
        .map(|id| PublishedFileId::try_from(id).unwrap())
        .collect();
    ids.extend(ids.clone());
    let preview = client.resolve(&ids);
    worker.join().unwrap();
    assert_eq!(
        preview.items.iter().map(|i| i.id.get()).collect::<Vec<_>>(),
        (1..=101).collect::<Vec<_>>()
    );
    assert!(preview.diagnostics.is_empty());
}

#[test]
fn oversize_response_and_collection_limits_are_visible_not_truncated_success() {
    let oversized = json!({"response":{"padding":"x".repeat(MAX_RESPONSE as usize)}});
    let children: Vec<_> = (1..=10001)
        .map(|id| json!({"publishedfileid":id.to_string(),"filetype":0}))
        .collect();
    for response in [
        oversized,
        json!({"response":{"collectiondetails":[{"publishedfileid":"42","result":1,"children":children}]}}),
    ] {
        let (client, worker) = server(vec![(200, details(42, 294100)), (200, response)]);
        let preview = client.resolve(&[PublishedFileId::try_from(42).unwrap()]);
        worker.join().unwrap();
        assert!(preview.items.is_empty());
        assert!(!preview.diagnostics.is_empty());
    }
    let client = WorkshopClient::at("http://127.0.0.1:1");
    let preview = client.resolve(&vec![PublishedFileId::try_from(42).unwrap(); 10001]);
    assert!(preview.diagnostics[0].message.contains("10000"));
}

#[test]
fn global_expansion_limit_does_not_allocate_one_error_per_unvisited_edge() {
    let client = WorkshopClient::at("http://127.0.0.1:1");
    let mut resolution = Resolution {
        client: &client,
        preview: WorkshopPreview::default(),
        cache: HashMap::new(),
        visited: HashSet::new(),
    };
    for id in 1..=9999 {
        resolution.cache.insert(
            PublishedFileId::try_from(id).unwrap(),
            Err("fixture".into()),
        );
    }
    resolution.load(&[
        PublishedFileId::try_from(10000).unwrap(),
        PublishedFileId::try_from(10001).unwrap(),
    ]);
    assert_eq!(resolution.preview.diagnostics.len(), 1);
    assert_eq!(resolution.cache.len(), 9999);
}

#[test]
fn nested_depth_limit_is_a_visible_error() {
    let mut responses = Vec::new();
    for id in 1..=10 {
        responses.push((200, details(id, 294100)));
        responses.push((200,json!({"response":{"collectiondetails":[{"publishedfileid":id.to_string(),"result":1,"children":[{"publishedfileid":(id+1).to_string(),"filetype":2}]}]}})));
    }
    let (client, worker) = server(responses);
    let preview = client.resolve(&[PublishedFileId::try_from(1).unwrap()]);
    worker.join().unwrap();
    assert!(preview.items.is_empty());
    assert!(
        preview
            .diagnostics
            .iter()
            .any(|d| d.message.contains("depth"))
    );
}

#[test]
fn collection_transport_errors_never_become_empty_success() {
    let (client, worker) = server(vec![
        (200, details(42, 294100)),
        (503, json!({})),
        (503, json!({})),
        (503, json!({})),
        (503, json!({})),
    ]);
    let preview = client.resolve(&[PublishedFileId::try_from(42).unwrap()]);
    worker.join().unwrap();
    assert!(preview.items.is_empty());
    assert!(!preview.diagnostics.is_empty());
}
