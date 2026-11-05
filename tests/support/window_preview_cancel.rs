use super::*;

pub(super) fn cancellation_stops_http_and_next_preview_works() {
    for status in [200, 503] {
        let (_root, ws) = fixture();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let client = Arc::new(WorkshopClient::at(&format!(
            "http://{}",
            listener.local_addr().unwrap()
        )));
        let (received_tx, received_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = done.clone();
        let server = std::thread::spawn(move || {
            let mut count = 0;
            let deadline = Instant::now() + Duration::from_secs(10);
            while !stop.load(std::sync::atomic::Ordering::Relaxed) && Instant::now() < deadline {
                let Ok((mut socket, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(2));
                    continue;
                };
                socket
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut header = Vec::new();
                loop {
                    let mut byte = [0];
                    socket.read_exact(&mut byte).unwrap();
                    header.push(byte[0]);
                    if header.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                let header = String::from_utf8(header).unwrap();
                let size = header
                    .lines()
                    .find_map(|line| {
                        line.to_lowercase()
                            .strip_prefix("content-length:")
                            .map(|n| n.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                socket.read_exact(&mut vec![0; size]).unwrap();
                count += 1;
                if count == 1 {
                    received_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                }
                let body = r#"{"response":{"publishedfiledetails":[{"publishedfileid":"42","result":1,"consumer_app_id":294100,"title":"Held"}],"collectiondetails":[{"publishedfileid":"42","result":1,"children":[{"publishedfileid":"43","filetype":0}]}]}}"#;
                let _ = write!(
                    socket,
                    "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
            count
        });
        let state = ws.state.clone();
        ws.with_acquisition(|w| {
            w.on_preview(move |text| acquisition::preview(&state, &text, client.clone()));
            w.invoke_preview("42".into());
        });
        received_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        ws.with_acquisition(|w| w.invoke_cancel_operation());
        release_tx.send(()).unwrap();
        wait(&ws, |w| w.with_acquisition(|a| !a.get_busy()));
        done.store(true, std::sync::atomic::Ordering::Relaxed);
        let requests = server.join().unwrap();
        assert_eq!(
            requests, 1,
            "Cancel must stop subsequent lookup/expansion/retry for HTTP {status}"
        );
        ws.with_acquisition(|w| {
            assert!(!w.get_preview_text().contains("Held"));
            assert!(w.get_status().contains("cancelled"));
        });
        preview(&ws);
        ws.with_acquisition(|w| assert!(w.get_status().contains("complete")));
    }
}
