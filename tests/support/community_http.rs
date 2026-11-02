// Included inside the integration module: loopback transport is private, not an app setting.
use super::*;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc;
use std::thread;

fn request_headers(stream: &mut TcpStream) -> String {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut headers = Vec::new();
    while !headers.ends_with(b"\r\n\r\n") {
        assert!(headers.len() < 16 * 1024, "oversized request headers");
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        headers.push(byte[0]);
    }
    String::from_utf8(headers).unwrap()
}

pub(crate) fn delayed_transport(
    store: &mut CommunityStore,
    received: std::sync::mpsc::Sender<()>,
    release: std::sync::mpsc::Receiver<()>,
) -> thread::JoinHandle<()> {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    store.transport = Transport {
        url: format!(
            "http://{}/communityRules.json",
            listener.local_addr().unwrap()
        ),
        https_only: false,
        connect: Duration::from_secs(10),
        total: Duration::from_secs(60),
    };
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        assert!(request_headers(&mut stream).starts_with("GET /communityRules.json HTTP/1.1\r\n"));
        received.send(()).unwrap();
        release.recv_timeout(Duration::from_secs(30)).unwrap();
        let body = br#"{"timestamp":2,"rules":{"z":{"loadTop":{"value":true}}}}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(body).unwrap();
    })
}

fn serve(status: u16, bytes: Vec<u8>) -> (Transport, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/communityRules.json",
        listener.local_addr().unwrap()
    );
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = request_headers(&mut stream);
        write!(
            stream,
            "HTTP/1.1 {status} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            bytes.len()
        )
        .unwrap();
        let _ = stream.write_all(&bytes);
        request
    });
    (
        Transport {
            url,
            https_only: false,
            connect: Duration::from_secs(10),
            total: Duration::from_secs(60),
        },
        handle,
    )
}

#[test]
fn real_http_validates_before_replacing_and_checks_request_path() {
    let root = tempfile::tempdir().unwrap();
    let store = CommunityStore::new(root.path().into());
    let old = br#"{"timestamp":1,"rules":{"old":{}}}"#;
    std::fs::write(store.cache_path(), old).unwrap();
    for (status, body) in [
        (500, b"failed".to_vec()),
        (200, b"invalid".to_vec()),
        (200, br#"{"rules":{"a":{"loadTop":true}}}"#.to_vec()),
        (200, vec![b' '; 4 * 1024 * 1024 + 1]),
        (302, b"".to_vec()),
    ] {
        let (transport, server) = serve(status, body);
        assert!(store.update_using(&transport).is_err());
        assert!(
            server
                .join()
                .unwrap()
                .starts_with("GET /communityRules.json HTTP/1.1")
        );
        assert_eq!(std::fs::read(store.cache_path()).unwrap(), old);
    }
    let new = br#"{"timestamp":2,"rules":{"new":{}}}"#;
    let (transport, server) = serve(200, new.to_vec());
    assert_eq!(store.update_using(&transport).unwrap().timestamp(), Some(2));
    server.join().unwrap();
    assert_eq!(std::fs::read(store.cache_path()).unwrap(), new);
    let mut at_limit = new.to_vec();
    at_limit.resize(4 * 1024 * 1024, b' ');
    let (transport, server) = serve(200, at_limit.clone());
    assert!(store.update_using(&transport).is_ok());
    server.join().unwrap();
    assert_eq!(std::fs::read(store.cache_path()).unwrap(), at_limit);
}

#[test]
fn total_timeout_interrupts_stalled_http_without_destroying_cache() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let transport = Transport {
        url: format!(
            "http://{}/communityRules.json",
            listener.local_addr().unwrap()
        ),
        https_only: false,
        connect: Duration::from_millis(100),
        total: Duration::from_millis(100),
    };
    let (received_tx, received_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        request_headers(&mut stream);
        received_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(30)).unwrap();
        drop(stream);
    });
    let root = tempfile::tempdir().unwrap();
    let mut store = CommunityStore::new(root.path().into());
    store.transport = transport;
    let old = br#"{"timestamp":1,"rules":{"old":{}}}"#;
    fs::write(store.cache_path(), old).unwrap();
    let (result_tx, result_rx) = mpsc::channel();
    let worker_store = store.clone();
    let worker = thread::spawn(move || result_tx.send(worker_store.update()).unwrap());
    received_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    // The server remains connected until AFTER the client reports its timeout.
    let result = result_rx.recv_timeout(Duration::from_secs(10));
    release_tx.send(()).unwrap();
    server.join().unwrap();
    worker.join().unwrap();
    assert!(
        result
            .unwrap()
            .unwrap_err()
            .to_string()
            .to_lowercase()
            .contains("timeout")
    );
    assert_eq!(fs::read(store.cache_path()).unwrap(), old);
}

#[test]
fn production_transport_enforces_https_and_bounded_operations() {
    let store = CommunityStore::new(PathBuf::from("unused-test-root"));
    let transport = &store.transport;
    let config = transport.agent().config().clone();
    assert!(config.https_only());
    assert_eq!(config.timeouts().connect, Some(Duration::from_secs(10)));
    assert_eq!(config.timeouts().global, Some(Duration::from_secs(60)));
    assert!(transport.agent().get("http://127.0.0.1:1/").call().is_err());
    assert_eq!(transport.url, SOURCE);
}
