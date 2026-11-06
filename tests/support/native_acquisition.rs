use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
};
use steam_protocol::{Action, Request};
use tempfile::TempDir;

pub struct Plan {
    expected: Request,
    payloads: Vec<Value>,
    pub diagnostic_lines: usize,
    pub exit_code: u8,
}

impl Plan {
    pub fn new(request_id: &str, action: Action, ids: &[u64], payloads: Vec<Value>) -> Self {
        Self {
            expected: Request {
                schema: 1,
                app_id: 294100,
                request_id: request_id.into(),
                action,
                ids: ids.to_vec(),
            },
            payloads,
            diagnostic_lines: 0,
            exit_code: 0,
        }
    }
}

pub fn helper(root: &Path, plans: &[Plan]) -> PathBuf {
    static FIXTURE: OnceLock<TempDir> = OnceLock::new();
    let name = format!("fake-native{}", std::env::consts::EXE_SUFFIX);
    let compiled = FIXTURE.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let result = Command::new("rustc")
            .env_remove("LD_PRELOAD")
            .env_remove("LD_AUDIT")
            .env_remove("DYLD_INSERT_LIBRARIES")
            .args(["--edition=2024", "--crate-name", "native_acquisition_fake"])
            .arg(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/native_acquisition_fake.rs"),
            )
            .arg("-o")
            .arg(dir.path().join(&name))
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        dir
    });
    // Only the nested directory is kept; the caller's TempDir owns its cleanup.
    let dir = tempfile::Builder::new()
        .prefix("native-fixture-")
        .tempdir_in(root)
        .unwrap()
        .keep();
    for (index, plan) in plans.iter().enumerate() {
        let batch = dir.join(format!("plan-{index}"));
        fs::create_dir(&batch).unwrap();
        fs::write(
            batch.join("request"),
            serde_json::to_vec(&plan.expected).unwrap(),
        )
        .unwrap();
        let mut stdout = Vec::new();
        for _ in 0..plan.diagnostic_lines {
            writeln!(stdout, "{}", "x".repeat(400)).unwrap();
        }
        for payload in plan
            .payloads
            .iter()
            .cloned()
            .chain([json!({"kind": "terminal"})])
        {
            writeln!(
                stdout,
                "RIMSORT_STEAM_V1 {}",
                json!({
                    "schema": 1,
                    "app_id": 294100,
                    "request_id": plan.expected.request_id,
                    "payload": payload,
                })
            )
            .unwrap();
        }
        fs::write(batch.join("stdout"), stdout).unwrap();
        fs::write(batch.join("exit-code"), plan.exit_code.to_string()).unwrap();
    }
    let exe = dir.join(&name);
    // Reuse a closed executable inode, never copy/rewrite a running executable.
    fs::hard_link(compiled.path().join(name), &exe).unwrap();
    exe
}
