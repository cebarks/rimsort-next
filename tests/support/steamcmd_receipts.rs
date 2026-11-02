use super::*;

const LIVE_SUCCESS: &str = "\x1b[0mSuccess. Downloaded item 2009463077 to \"/work/content/steamapps/workshop/content/294100/2009463077\" (8306239 bytes) \x1b[0m\x1b[1mquit";

struct Chunked {
    bytes: std::io::Cursor<Vec<u8>>,
    size: usize,
}
impl Read for Chunked {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let size = buffer.len().min(self.size);
        self.bytes.read(&mut buffer[..size])
    }
}

fn stream_events(text: &str, size: usize) -> Vec<StreamEvent> {
    let (tx, rx) = mpsc::sync_channel(64);
    let handle = reader(
        Chunked {
            bytes: std::io::Cursor::new(text.as_bytes().to_vec()),
            size,
        },
        tx,
    )
    .unwrap();
    let events = rx.into_iter().collect();
    handle.join().unwrap();
    events
}

#[test]
fn sgr_receipts_survive_every_byte_boundary_and_keep_failure_precedence() {
    let id = PublishedFileId::try_from(2009463077).unwrap();
    for size in [1, 2, 3, 7, 4096] {
        let mut receipts = HashMap::new();
        for event in stream_events(&format!("{LIVE_SUCCESS}\r\n"), size) {
            if let StreamEvent::Line(line) = event {
                receipt(&line, &[id], &mut receipts);
            }
        }
        assert_eq!(
            receipts.get(&id),
            Some(&DownloadOutcome::Succeeded),
            "chunk size {size}"
        );
        let failures = format!(
            "\x1b[31mERROR! Download item 2009463077 failed (Failure).\x1b[0m\n{LIVE_SUCCESS}"
        );
        for event in stream_events(&failures, size) {
            if let StreamEvent::Line(line) = event {
                receipt(&line, &[id], &mut receipts);
            }
        }
        assert_eq!(receipts.get(&id), Some(&DownloadOutcome::Failed));
    }
}

#[test]
fn sgr_normalization_does_not_unanchor_receipts_or_accept_unrequested_ids() {
    let id = PublishedFileId::try_from(2009463077).unwrap();
    let input = format!(
        "prefix {LIVE_SUCCESS}\n\x1b[2J{LIVE_SUCCESS}\n\x1b]0;title\x07{LIVE_SUCCESS}\n\x1b[0m Success. Downloaded item 2009463077 to ignored\n\x1b[0mSuccess. Downloaded item 42 to ignored\n\x1b[0mSuccess. Downloaded item 2009463077suffix to ignored\n"
    );
    let mut receipts = HashMap::new();
    for event in stream_events(&input, 1) {
        if let StreamEvent::Line(line) = event {
            receipt(&line, &[id], &mut receipts);
        }
    }
    assert!(receipts.is_empty());
}

#[test]
fn sgr_bytes_count_toward_raw_line_limit_before_normalization() {
    let id = PublishedFileId::try_from(2009463077).unwrap();
    let input = format!("{}{LIVE_SUCCESS}\n", "\x1b[0m".repeat(2048));
    let mut receipts = HashMap::new();
    let mut rejected = false;
    for event in stream_events(&input, 3) {
        match event {
            StreamEvent::Line(line) => receipt(&line, &[id], &mut receipts),
            StreamEvent::InvalidOutput(_) => rejected = true,
            _ => {}
        }
    }
    assert!(rejected);
    assert!(receipts.is_empty());
}
