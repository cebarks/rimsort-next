use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn invoke(args: &[&str], input: &[u8]) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rimsort-next-steam"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let _ = stdin.write_all(input);
    drop(stdin);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            panic!("helper did not exit within five seconds");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn version_needs_no_request_or_sdk_initialization() {
    let output = invoke(&["--version"], b"");
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .starts_with("rimsort-next-steam 0.1.0 protocol=1")
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn invalid_inputs_are_rejected_before_initialization() {
    for input in [
        b"not JSON".as_slice(),
        br#"{"schema":1,"app_id":42,"request_id":"test","action":"subscribe","ids":[1]}"#,
        br#"{"schema":1,"app_id":294100,"request_id":"test","action":"delete","ids":[1]}"#,
        br#"{"schema":1,"app_id":294100,"request_id":"test","action":"subscribe","ids":[0]}"#,
    ] {
        let output = invoke(&[], input);
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .starts_with("invalid request:")
        );
    }
    let output = invoke(&["--unknown"], b"");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .starts_with("usage: rimsort-next-steam ")
    );
}
