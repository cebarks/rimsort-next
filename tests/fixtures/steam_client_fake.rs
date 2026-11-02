//! Controlled child fixture: no SDK, Steam, or network calls.
use std::{
    env, fs,
    io::{self, Read, Write},
    process, thread,
    time::Duration,
};

const BEFORE: &str = r#"RIMSORT_STEAM_V1 {"schema":1,"app_id":294100,"request_id":"adapter-test","payload":{"kind":"observation","item_id":123,"phase":"before","facts":{"subscription":"unknown","install":null}}}"#;
const AFTER: &str = r#"RIMSORT_STEAM_V1 {"schema":1,"app_id":294100,"request_id":"adapter-test","payload":{"kind":"observation","item_id":123,"phase":"after","facts":{"subscription":"not_subscribed","install":null}}}"#;
const ACK: &str = r#"RIMSORT_STEAM_V1 {"schema":1,"app_id":294100,"request_id":"adapter-test","payload":{"kind":"outcome","item_id":123,"outcome":"acknowledged","detail":null}}"#;
const FAILED: &str = r#"RIMSORT_STEAM_V1 {"schema":1,"app_id":294100,"request_id":"adapter-test","payload":{"kind":"outcome","item_id":123,"outcome":"failed","detail":{"stage":"request","kind":"request_rejected","message":"controlled rejection"}}}"#;
const TERMINAL: &str = r#"RIMSORT_STEAM_V1 {"schema":1,"app_id":294100,"request_id":"adapter-test","payload":{"kind":"terminal"}}"#;

fn main() {
    let exe = env::current_exe().unwrap();
    let dir = exe.parent().unwrap();
    let mode = exe.file_stem().unwrap().to_str().unwrap();
    if let Some(child_mode) = env::args().nth(1) {
        #[cfg(unix)]
        if child_mode == "escaped" {
            unsafe extern "C" {
                fn setsid() -> i32;
            }
            assert!(unsafe { setsid() } > 0);
        }
        fs::write(dir.join(format!("{child_mode}-ready")), b"ready").unwrap();
        thread::sleep(Duration::from_millis(1800));
        fs::write(dir.join(format!("{child_mode}-done")), b"done").unwrap();
        return;
    }
    fs::write(dir.join("started"), b"started").unwrap();
    assert_eq!(env::args_os().count(), 1, "adapter must not add argv");
    if mode == "loader" {
        eprintln!("controlled loader: runtime unavailable");
        process::exit(127);
    }
    if mode == "blocked" {
        thread::sleep(Duration::from_secs(10));
        return;
    }
    if mode == "environment" {
        for key in [
            "HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_CACHE_HOME",
            "LD_LIBRARY_PATH",
            "DYLD_LIBRARY_PATH",
            "PATH",
            "LD_PRELOAD",
            "LD_AUDIT",
            "DYLD_INSERT_LIBRARIES",
        ] {
            fs::write(
                dir.join(format!("env-{key}")),
                env::var_os(key)
                    .unwrap_or_default()
                    .to_string_lossy()
                    .as_bytes(),
            )
            .unwrap();
        }
    }
    let mut input = Vec::new();
    io::stdin().read_to_end(&mut input).unwrap();
    fs::write(dir.join("request.json"), input).unwrap();
    if mode.starts_with("owned") || mode.starts_with("escaped") {
        let child_mode = if mode.starts_with("owned") {
            "owned"
        } else {
            "escaped"
        };
        let _child = process::Command::new(&exe).arg(child_mode).spawn().unwrap();
        while !dir.join(format!("{child_mode}-ready")).exists() {
            thread::sleep(Duration::from_millis(2));
        }
    }
    println!("native stdout diagnostic");
    eprintln!("native stderr diagnostic");
    println!("{BEFORE}");
    println!("{AFTER}");
    println!("{}", if mode == "failed" { FAILED } else { ACK });
    io::stdout().flush().unwrap();
    if mode == "crash" {
        process::exit(17);
    }
    if mode == "partial" {
        return;
    }
    if mode == "hold" || mode == "owned" || mode == "escaped" {
        thread::sleep(Duration::from_secs(10));
        return;
    }
    match mode {
        "malformed" => println!("RIMSORT_STEAM_V1 {{bad-json"),
        "foreign" => println!("{}", TERMINAL.replace("adapter-test", "foreign")),
        "duplicate" => println!("{ACK}"),
        "late" => {
            println!("{TERMINAL}");
            println!("{ACK}");
        }
        "long-line" => {
            io::stdout().write_all(&vec![b'x'; 262145]).unwrap();
        }
        "flood" => {
            let stderr = thread::spawn(|| {
                for _ in 0..10000 {
                    eprintln!("{}", "e".repeat(1000));
                }
            });
            for _ in 0..10000 {
                println!("{}", "o".repeat(1000));
            }
            stderr.join().unwrap();
        }
        "retention" | "short-logs" => {
            let width = if mode == "retention" { 300 } else { 1 };
            for _ in 0..1500 {
                println!("{}", "r".repeat(width));
            }
            println!("{TERMINAL}");
        }
        _ => println!("{TERMINAL}"),
    }
}
