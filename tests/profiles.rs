use rimsort_rust::integrations::profiles::ProfileLibrary;

fn tokens(value: &str) -> Vec<String> {
    vec![value.into()]
}

#[test]
fn replacement_requires_current_answer_and_exact_save_ack() {
    use rimsort_rust::{
        application::{session::Session, workflow::*},
        domain::{catalog::Catalog, types::GameVersion},
        integrations::{config::parse_config, launch::Platform, settings::Settings},
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ModsConfig.xml");
    let raw = b"<ModsConfigData><activeMods><li>game</li></activeMods></ModsConfigData>";
    std::fs::write(&path, raw).unwrap();
    let mut session = Session::new(
        Catalog::new(vec![]).unwrap(),
        parse_config(raw).unwrap(),
        path,
        GameVersion::parse("1.6").unwrap(),
    );
    let mut workflow = Workflow::new(&Settings::default(), Platform::Linux, dir.path());
    session.replace_tokens(&tokens("edited")).unwrap();
    assert!(!session.replace_tokens(&tokens("edited")).unwrap());
    let request = || WorkflowEvent::ReplaceRequested {
        tokens: tokens("candidate_steam"),
        source: "checkpoint".into(),
        confirm_warnings: false,
    };
    let effects = workflow.reduce(&mut session, request());
    let WorkflowEffect::PromptUnsavedReplace { prompt, .. } = effects[0] else {
        panic!("expected prompt")
    };
    session.replace_tokens(&tokens("new edit")).unwrap();
    workflow.reduce(
        &mut session,
        WorkflowEvent::PromptAnswered {
            prompt,
            answer: PromptAnswer::Discard,
        },
    );
    assert_eq!(session.working_tokens(), tokens("new edit"));
    let effects = workflow.reduce(&mut session, request());
    let WorkflowEffect::PromptUnsavedReplace { prompt, .. } = effects[0] else {
        panic!("expected prompt")
    };
    let effects = workflow.reduce(
        &mut session,
        WorkflowEvent::PromptAnswered {
            prompt,
            answer: PromptAnswer::Save,
        },
    );
    let WorkflowEffect::Save { operation, .. } = effects[0] else {
        panic!("expected save")
    };
    workflow.reduce(&mut session, request());
    assert_eq!(session.working_tokens(), tokens("new edit"));
    workflow.reduce(
        &mut session,
        WorkflowEvent::SaveFinished {
            operation,
            outcome: SaveOutcome::Failed(OperationFailure::new("disk full")),
        },
    );
    assert_eq!(session.working_tokens(), tokens("new edit"));
    assert_eq!(std::fs::read(session.config_path()).unwrap(), raw);
}

#[test]
fn seed_reopen_and_crud_preserve_checkpoints() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("profiles.json");
    let seed = vec![
        "ludeon.rimworld".into(),
        "missing.mod".into(),
        "copy.mod_steam".into(),
        "".into(),
    ];
    let mut library = ProfileLibrary::open(&path, &seed).unwrap();
    assert_eq!(library.profiles()[0].name, "Default");
    assert_eq!(library.profiles()[0].revisions[0].tokens, seed);
    let id = library.create(" ../strange/name ", &tokens("new")).unwrap();
    assert_eq!(library.profiles()[1].name, "../strange/name");
    assert!(!dir.path().join("strange").exists());
    library.rename(id, "Named").unwrap();
    let copy = library.duplicate(id, "Copy").unwrap();
    assert!(copy > id);
    library.delete(copy).unwrap();
    assert!(library.create("Next", &[]).unwrap() > copy);
    let reopened = ProfileLibrary::open(&path, &tokens("NOT GAME")).unwrap();
    assert_eq!(reopened.profiles()[0].revisions[0].tokens, seed);
    assert_eq!(reopened.profiles()[1].name, "Named");
}

#[test]
fn validation_and_failed_persistence_leave_memory_and_disk_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("profiles.json");
    let mut library = ProfileLibrary::open(&path, &[]).unwrap();
    let id = library.profiles()[0].id;
    for name in ["", "  ", "DEFAULT", "a\nb", "x\0"] {
        assert!(library.create(name, &[]).is_err());
    }
    let unicode = library.create("Ä", &[]).unwrap();
    assert!(library.create("ä", &[]).is_err());
    library.delete(unicode).unwrap();
    assert!(library.delete(id).is_err());
    assert!(library.rename(9999, "missing").is_err());
    assert!(library.save_revision(9999, &[], "save").is_err());
    let before = library.profiles().to_vec();
    let bytes = std::fs::read(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(
        library
            .save_revision(id, &tokens("changed"), "save")
            .is_err()
    );
    assert_eq!(library.profiles(), before);
    std::fs::remove_dir(&path).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    assert_eq!(ProfileLibrary::open(&path, &[]).unwrap().profiles(), before);
}

#[test]
fn history_is_newest_first_capped_and_consecutive_deduplicated() {
    let dir = tempfile::tempdir().unwrap();
    let mut library = ProfileLibrary::open(&dir.path().join("profiles.json"), &[]).unwrap();
    let id = library.profiles()[0].id;
    for n in 0..60 {
        library
            .save_revision(id, &tokens(&n.to_string()), "explicit save")
            .unwrap();
    }
    library
        .save_revision(id, &tokens("59"), "duplicate")
        .unwrap();
    let revisions = &library.profiles()[0].revisions;
    assert_eq!(revisions.len(), 50);
    assert_eq!(revisions[0].tokens, tokens("59"));
    assert_eq!(revisions[49].tokens, tokens("10"));
    assert_eq!(revisions[0].reason, "explicit save");
    assert!(revisions[0].timestamp > 0);
}

#[test]
fn stale_libraries_and_external_edits_cannot_be_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("profiles.json");
    let mut first = ProfileLibrary::open(&path, &tokens("game")).unwrap();
    let mut second = ProfileLibrary::open(&path, &[]).unwrap();
    first.create("First", &tokens("one")).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let before = second.profiles().to_vec();
    assert!(
        second
            .create("Second", &tokens("two"))
            .unwrap_err()
            .to_string()
            .contains("changed")
    );
    assert_eq!(second.profiles(), before);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let mut reopened = ProfileLibrary::open(&path, &[]).unwrap();
    reopened.create("Second", &tokens("two")).unwrap();
    let before = reopened.profiles().to_vec();
    std::fs::write(&path, "external change").unwrap();
    assert!(reopened.rename(1, "Renamed").is_err());
    assert_eq!(reopened.profiles(), before);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "external change");
}

#[test]
fn contended_sidecar_refuses_without_waiting_or_mutating() {
    use std::{fs, sync::mpsc, time::Duration};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("profiles.json");
    let mut library = ProfileLibrary::open(&path, &tokens("game")).unwrap();
    let before = library.profiles().to_vec();
    let bytes = fs::read(&path).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path.with_extension("json.lock"))
        .unwrap();
    lock.lock().unwrap();
    let (send, receive) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        let result = library.rename(1, "Blocked");
        send.send((library, result)).unwrap();
    });
    let receipt = receive.recv_timeout(Duration::from_secs(2));
    lock.unlock().unwrap();
    worker.join().unwrap();
    let (mut library, result) = receipt.expect("contended library must refuse before lock release");
    assert!(result.unwrap_err().to_string().contains("busy"));
    assert_eq!(library.profiles(), before);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert!(path.with_extension("json.lock").exists());
    library.rename(1, "After release").unwrap();
    assert_eq!(
        ProfileLibrary::open(&path, &[]).unwrap().profiles()[0].name,
        "After release"
    );
}

#[test]
fn corrupt_future_and_oversized_files_are_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("profiles.json");
    for bytes in [
        b"not json".to_vec(),
        br#"{"schema":999,"next_id":1,"profiles":[]}"#.to_vec(),
        vec![b' '; 32 * 1024 * 1024 + 1],
    ] {
        std::fs::write(&path, &bytes).unwrap();
        assert!(ProfileLibrary::open(&path, &[]).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}
