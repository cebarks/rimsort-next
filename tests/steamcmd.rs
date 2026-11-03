#![cfg(target_os = "linux")]
use rimsort_rust::{
    application::acquisition::{DownloadOutcome, ItemOutcome},
    domain::types::GameVersion,
    integrations::{
        steamcmd::{SteamCmdConfig, build_script, run_downloads},
        workshop::PublishedFileId,
    },
};
use std::{fs, os::unix::fs::PermissionsExt, sync::atomic::AtomicBool, time::Duration};

fn config(root: &std::path::Path, body: &str) -> SteamCmdConfig {
    let executable = root.join("fake steamcmd");
    fs::write(&executable, format!("#!/usr/bin/python3\n{body}")).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let mods_root = root.join("Mods");
    fs::create_dir(&mods_root).unwrap();
    SteamCmdConfig {
        executable,
        workspace: root.join("private workspace"),
        mods_root,
        target_version: GameVersion::parse("1.6").unwrap(),
        batch_timeout: Duration::from_secs(5),
    }
}
const SUCCESS: &str = r#"import os,sys,pathlib
assert sys.argv[1] == '+runscript' and len(sys.argv)==3
cwd=pathlib.Path.cwd()
for name in ['HOME','XDG_CONFIG_HOME','XDG_DATA_HOME','XDG_CACHE_HOME']:
    assert pathlib.Path(os.environ[name]).is_relative_to(cwd)
script=pathlib.Path(sys.argv[2]).read_text()
root=pathlib.Path(script.splitlines()[0].split('"')[1])
assert root.is_relative_to(cwd)
for line in script.splitlines():
    if line.startswith('workshop_download_item '):
        _,app,id,validate=line.split()
        assert app=='294100' and validate=='validate'
        folder=root/'steamapps/workshop/content/294100'/id
        (folder/'About').mkdir(parents=True)
        (folder/'About/About.xml').write_text('<ModMetaData><name>Fixture</name><packageId>test.fixture</packageId></ModMetaData>')
        print('Success. Downloaded item '+id+' to "ignored log path"',flush=True)
"#;

#[test]
fn real_process_is_isolated_and_only_proven_content_is_published() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = config(temp.path(), SUCCESS);
    let id = PublishedFileId::try_from(42).unwrap();
    let report = run_downloads(&cfg, &[id], &AtomicBool::new(false), &mut |_| {}).unwrap();
    assert_eq!(report.items[0].download, DownloadOutcome::Succeeded);
    assert_eq!(report.items[0].outcome, ItemOutcome::Succeeded);
    assert!(cfg.mods_root.join("42/About/About.xml").is_file());
    assert_eq!(report.items[0].installations.len(), 1);
    let second = run_downloads(&cfg, &[id], &AtomicBool::new(false), &mut |_| {}).unwrap();
    assert_ne!(report.runs, second.runs);
}

#[test]
fn sgr_wrapped_real_receipts_publish_only_proven_content_and_failure_wins() {
    for failed in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let fixture = SUCCESS.replace("        print(", "        # print(");
        let mut body = format!(
            "{fixture}\n{}",
            r#"receipt=b'\x1b[0mSuccess. Downloaded item 2009463077 to "/work/content/steamapps/workshop/content/294100/2009463077" (8306239 bytes) \x1b[0m\x1b[1mquit\n'
for byte in receipt: os.write(1,bytes([byte]))
"#
        );
        if failed {
            body.push_str(
                r#"os.write(2,b'\x1b[31mERROR! Download item 2009463077 failed (Failure).\x1b[0m\n')
os.write(1,receipt)
"#,
            );
        }
        let cfg = config(temp.path(), &body);
        let id = PublishedFileId::try_from(2009463077).unwrap();
        let report = run_downloads(&cfg, &[id], &AtomicBool::new(false), &mut |_| {}).unwrap();
        assert_eq!(
            report.items[0].download,
            if failed {
                DownloadOutcome::Failed
            } else {
                DownloadOutcome::Succeeded
            }
        );
        assert_eq!(
            report.items[0].outcome,
            if failed {
                ItemOutcome::Failed
            } else {
                ItemOutcome::Succeeded
            }
        );
        assert_eq!(
            cfg.mods_root.join("2009463077/About/About.xml").is_file(),
            !failed
        );
        assert!(report.cleanup_error.is_none());
    }
}

#[test]
fn zero_exit_without_item_success_and_explicit_failure_never_install() {
    for body in [
        "print('all done')",
        "print('Success. Downloaded item 999 to x')",
        "print('ERROR! Download item 42 failed (Failure).')",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let cfg = config(temp.path(), body);
        let report = run_downloads(
            &cfg,
            &[PublishedFileId::try_from(42).unwrap()],
            &AtomicBool::new(false),
            &mut |_| {},
        )
        .unwrap();
        assert_ne!(report.items[0].outcome, ItemOutcome::Succeeded);
        assert!(!cfg.mods_root.join("42").exists());
    }
}

#[test]
fn bounded_streams_and_deadline_do_not_hang() {
    let temp = tempfile::tempdir().unwrap();
    let mut cfg = config(
        temp.path(),
        "import os,time\nfor i in range(5000):\n os.write(1,b'x'*80+b'\\n'); os.write(2,b'y'*80+b'\\n')\ntime.sleep(30)",
    );
    cfg.batch_timeout = Duration::from_millis(300);
    let start = std::time::Instant::now();
    let report = run_downloads(
        &cfg,
        &[PublishedFileId::try_from(42).unwrap()],
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap();
    assert!(start.elapsed() < Duration::from_secs(5));
    assert!(report.logs_truncated);
    assert!(report.logs.len() <= 1000);
    assert!(report.logs.iter().map(String::len).sum::<usize>() <= 256 * 1024);
    assert_eq!(report.items[0].download, DownloadOutcome::TimedOut);
}

#[test]
fn later_spawn_failure_retains_completed_batch_facts() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = config(
        temp.path(),
        &format!("{SUCCESS}\npathlib.Path(sys.argv[0]).unlink()"),
    );
    let ids: Vec<_> = (1..=21)
        .map(|v| PublishedFileId::try_from(v).unwrap())
        .collect();
    let report = run_downloads(&cfg, &ids, &AtomicBool::new(false), &mut |_| {}).unwrap();
    assert!(
        report.items[..20]
            .iter()
            .all(|i| i.outcome == ItemOutcome::Succeeded)
    );
    assert_eq!(report.items[20].outcome, ItemOutcome::Failed);
    assert_eq!(report.runs.len(), 2);
}

#[test]
fn failed_update_retains_separate_existing_copy_observation() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = config(temp.path(), SUCCESS);
    let id = PublishedFileId::try_from(42).unwrap();
    run_downloads(&cfg, &[id], &AtomicBool::new(false), &mut |_| {}).unwrap();
    fs::write(
        &cfg.executable,
        "#!/usr/bin/python3\nprint('ERROR! Download item 42 failed (Failure).')",
    )
    .unwrap();
    let report = run_downloads(&cfg, &[id], &AtomicBool::new(false), &mut |_| {}).unwrap();
    assert_eq!(report.items[0].download, DownloadOutcome::Failed);
    assert_eq!(report.items[0].installations.len(), 1);
    assert!(cfg.mods_root.join("42/About/About.xml").is_file());
}

#[cfg(target_os = "linux")]
#[test]
fn cancellation_stops_owned_descendants_not_unrelated_processes() {
    use rimsort_rust::application::acquisition::AcquisitionEvent;
    use std::sync::atomic::Ordering;
    let temp = tempfile::tempdir().unwrap();
    let cfg = config(
        temp.path(),
        "import subprocess,time\nchild=subprocess.Popen(['/usr/bin/sleep','30'])\nprint('READY '+str(child.pid),flush=True)\ntime.sleep(30)",
    );
    let cancel = AtomicBool::new(false);
    let mut pid = None;
    let mut unrelated = std::process::Command::new("/usr/bin/sleep")
        .arg("30")
        .spawn()
        .unwrap();
    let start = std::time::Instant::now();
    let report = run_downloads(
        &cfg,
        &[PublishedFileId::try_from(42).unwrap()],
        &cancel,
        &mut |event| {
            if let AcquisitionEvent::Log(line) = event
                && let Some(value) = line.strip_prefix("READY ")
            {
                pid = Some(value.parse::<u32>().unwrap());
                cancel.store(true, Ordering::Release);
            }
        },
    )
    .unwrap();
    assert!(start.elapsed() < Duration::from_secs(5));
    assert!(unrelated.try_wait().unwrap().is_none());
    unrelated.kill().unwrap();
    unrelated.wait().unwrap();
    let stat = fs::read_to_string(format!("/proc/{}/stat", pid.unwrap()));
    assert!(stat.is_err() || stat.unwrap().split_whitespace().nth(2) == Some("Z"));
    assert_eq!(report.items[0].download, DownloadOutcome::Cancelled);
}

#[test]
fn overlong_lines_cannot_smuggle_success_or_force_unbounded_retention() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = config(temp.path(), &format!("{SUCCESS}\nprint('x'*1000000)"));
    let report = run_downloads(
        &cfg,
        &[PublishedFileId::try_from(42).unwrap()],
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap();
    assert_ne!(report.items[0].outcome, ItemOutcome::Succeeded);
    assert!(!cfg.mods_root.join("42").exists());
}

#[test]
fn escaped_pipe_holder_reports_cleanup_failure_instead_of_hanging() {
    let temp = tempfile::tempdir().unwrap();
    let mut cfg = config(temp.path(), SUCCESS);
    let previous = run_downloads(
        &cfg,
        &[PublishedFileId::try_from(1).unwrap()],
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap();
    let manifest_path = temp.path().join(".rimsort-next-Mods/manifest.json");
    let manifest_before = fs::read(&manifest_path).unwrap();
    // The first batch includes IDs without receipts; the second must never start.
    let fixture = SUCCESS.replace(
        "        print('Success. Downloaded item '+id+",
        "        if int(id)==1: print('Success. Downloaded item '+id+",
    );
    fs::write(
        &cfg.executable,
        format!("#!/usr/bin/python3\n{fixture}\nimport subprocess\nfor i in range(1100): print('filler',flush=True)\nprint('ERROR! Download item 2 failed (Failure).',flush=True)\nprint('Success. Downloaded item 3 to ignored',flush=True)\nsubprocess.Popen(['/usr/bin/sleep','2'],start_new_session=True)"),
    )
    .unwrap();
    cfg.batch_timeout = Duration::from_secs(3);
    let start = std::time::Instant::now();
    let ids: Vec<_> = (1..=21)
        .map(|id| PublishedFileId::try_from(id).unwrap())
        .collect();
    let report = run_downloads(&cfg, &ids, &AtomicBool::new(false), &mut |_| {}).unwrap();
    assert!(start.elapsed() < Duration::from_millis(1500));
    assert!(report.cleanup_error.is_some());
    assert!(report.logs_truncated);
    assert_eq!(report.runs.len(), 1);
    assert_eq!(report.items[0].download, DownloadOutcome::Succeeded);
    assert_eq!(report.items[1].download, DownloadOutcome::Failed);
    assert_eq!(report.items[2].download, DownloadOutcome::Succeeded);
    assert!(
        report.items[3..20]
            .iter()
            .all(|row| row.download == DownloadOutcome::Unverified)
    );
    assert_eq!(report.items[20].download, DownloadOutcome::NotStarted);
    assert!(
        report
            .items
            .iter()
            .all(|row| row.outcome != ItemOutcome::Succeeded)
    );
    assert_eq!(
        report.items[0].installations,
        previous.items[0].installations
    );
    assert!(
        report.items[1..]
            .iter()
            .all(|row| row.installations.is_empty())
    );
    assert_eq!(fs::read(&manifest_path).unwrap(), manifest_before);
    assert_eq!(fs::read_dir(&cfg.mods_root).unwrap().count(), 1);
    assert_eq!(
        fs::read_dir(temp.path().join(".rimsort-next-Mods/backups"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn recovery_fault_stops_later_batches_and_is_reported_for_phase_b_gates() {
    use rimsort_rust::application::acquisition::AcquisitionEvent;
    let temp = tempfile::tempdir().unwrap();
    let cfg = config(temp.path(), SUCCESS);
    let ids: Vec<_> = (1..=21)
        .map(|id| PublishedFileId::try_from(id).unwrap())
        .collect();
    let mut injected = false;
    let report = run_downloads(&cfg, &ids, &AtomicBool::new(false), &mut |event| {
        if let AcquisitionEvent::Item(item) = event
            && item.outcome == ItemOutcome::Running
            && !injected
        {
            fs::write(
                temp.path().join(".rimsort-next-Mods/journal.json"),
                "corrupt",
            )
            .unwrap();
            injected = true;
        }
    })
    .unwrap();
    assert!(report.recovery_required.is_some());
    assert_eq!(report.runs.len(), 1);
    assert!(!cfg.mods_root.join("1").exists());
    assert!(
        report
            .items
            .iter()
            .all(|item| item.outcome == ItemOutcome::Failed)
    );
}

#[test]
fn workspace_failure_after_a_batch_preserves_completed_facts() {
    use rimsort_rust::application::acquisition::AcquisitionEvent;
    let temp = tempfile::tempdir().unwrap();
    let cfg = config(temp.path(), SUCCESS);
    let ids: Vec<_> = (1..=21)
        .map(|id| PublishedFileId::try_from(id).unwrap())
        .collect();
    let report = run_downloads(&cfg, &ids, &AtomicBool::new(false), &mut |event| {
        if let AcquisitionEvent::Item(item) = event
            && item.outcome == ItemOutcome::Succeeded
            && item.item_id.get() == 20
        {
            fs::rename(&cfg.workspace, temp.path().join("preserved-runs")).unwrap();
            fs::write(&cfg.workspace, "obstruction").unwrap();
        }
    })
    .unwrap();
    assert!(
        report.items[..20]
            .iter()
            .all(|i| i.outcome == ItemOutcome::Succeeded)
    );
    assert_eq!(report.items[20].outcome, ItemOutcome::Failed);
}

#[test]
fn captured_worker_records_resolved_timestamp_without_activating_content() {
    use rimsort_rust::{
        application::acquisition::{AcquisitionRequest, Freshness, run_acquisition},
        integrations::{
            install::Installer,
            workshop::{WorkshopItem, WorkshopKind},
        },
    };
    let temp = tempfile::tempdir().unwrap();
    let cfg = config(temp.path(), SUCCESS);
    let id = PublishedFileId::try_from(42).unwrap();
    let request = AcquisitionRequest {
        config: cfg.clone(),
        items: vec![WorkshopItem {
            id,
            title: "fixture".into(),
            app_id: 294100,
            kind: WorkshopKind::ItemCandidate,
            upstream_updated: Some(17),
        }],
    };
    let report = run_acquisition(&request, &AtomicBool::new(false), &mut |_| {}).unwrap();
    assert_eq!(report.items[0].outcome, ItemOutcome::Succeeded);
    assert_eq!(
        report.items[0].installations[0].freshness,
        Freshness::Current
    );
    assert_eq!(
        Installer::open(&cfg.mods_root)
            .unwrap()
            .observation(id)
            .unwrap()
            .unwrap()
            .upstream_updated,
        Some(17)
    );
    assert!(!temp.path().join("ModsConfig.xml").exists());
}

#[test]
fn out_of_order_partial_failure_is_correlated_by_id_across_streams() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = config(
        temp.path(),
        &format!("{SUCCESS}\nprint('ERROR! Download item 42 failed (Failure).',file=sys.stderr)"),
    );
    let ids = [
        PublishedFileId::try_from(42).unwrap(),
        PublishedFileId::try_from(43).unwrap(),
    ];
    let report = run_downloads(&cfg, &ids, &AtomicBool::new(false), &mut |_| {}).unwrap();
    assert_eq!(report.items[0].download, DownloadOutcome::Failed);
    assert_eq!(report.items[1].outcome, ItemOutcome::Succeeded);
    assert!(!cfg.mods_root.join("42").exists());
    assert!(cfg.mods_root.join("43/About/About.xml").exists());
}

#[test]
fn active_buffered_output_drain_uses_batch_deadline_not_short_idle_grace() {
    use rimsort_rust::application::acquisition::AcquisitionEvent;
    let temp = tempfile::tempdir().unwrap();
    let cfg = config(
        temp.path(),
        &format!(
            "import os\nfor i in range(1000):\n os.write(1,b'out\\n');os.write(2,b'err\\n')\n{SUCCESS}"
        ),
    );
    let report = run_downloads(
        &cfg,
        &[PublishedFileId::try_from(42).unwrap()],
        &AtomicBool::new(false),
        &mut |event| {
            if matches!(event, AcquisitionEvent::Log(_)) {
                std::thread::sleep(Duration::from_millis(1));
            }
        },
    )
    .unwrap();
    assert_eq!(
        report.items[0].outcome,
        ItemOutcome::Succeeded,
        "cleanup={:?}",
        report.cleanup_error
    );
}

#[test]
fn receipt_after_log_retention_limit_still_proves_download() {
    let temp = tempfile::tempdir().unwrap();
    let cfg = config(
        temp.path(),
        &format!(
            "import os\nfor i in range(3000):\n os.write(1,b'out\\n');os.write(2,b'err\\n')\n{SUCCESS}"
        ),
    );
    let report = run_downloads(
        &cfg,
        &[PublishedFileId::try_from(42).unwrap()],
        &AtomicBool::new(false),
        &mut |_| {},
    )
    .unwrap();
    assert!(report.logs_truncated);
    assert_eq!(
        report.items[0].outcome,
        ItemOutcome::Succeeded,
        "cleanup={:?}; diagnostics={:?}",
        report.cleanup_error,
        report.items[0].diagnostics
    );
}

#[test]
fn cancellation_between_batches_preserves_completed_installs() {
    use rimsort_rust::application::acquisition::AcquisitionEvent;
    use std::sync::atomic::Ordering;
    let temp = tempfile::tempdir().unwrap();
    let cfg = config(temp.path(), SUCCESS);
    let cancel = AtomicBool::new(false);
    let ids: Vec<_> = (1..=21)
        .map(|id| PublishedFileId::try_from(id).unwrap())
        .collect();
    let report = run_downloads(&cfg, &ids, &cancel, &mut |event| {
        if let AcquisitionEvent::Item(item) = event
            && item.outcome == ItemOutcome::Succeeded
            && item.item_id.get() == 20
        {
            cancel.store(true, Ordering::Release);
        }
    })
    .unwrap();
    assert_eq!(report.runs.len(), 1);
    assert!(
        report.items[..20]
            .iter()
            .all(|i| i.outcome == ItemOutcome::Succeeded)
    );
    assert_eq!(report.items[20].outcome, ItemOutcome::Cancelled);
    assert!(!cfg.mods_root.join("21").exists());
}

#[test]
fn script_rejects_injection_and_oversize_batch() {
    let temp = tempfile::tempdir().unwrap();
    let mut cfg = config(temp.path(), "pass");
    let id = PublishedFileId::try_from(42).unwrap();
    cfg.workspace = temp.path().join("evil\"\nquit");
    assert!(build_script(&cfg, &[id]).is_err());
    cfg.workspace = temp.path().join("safe");
    assert!(build_script(&cfg, &[id; 21]).is_err());
}
