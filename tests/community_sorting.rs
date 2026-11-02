use rimsort_rust::domain::{
    catalog::{ActiveEntry, Catalog},
    metadata::parse_about,
    sorting::{sort_active, sort_active_with_rules},
    types::{GameVersion, ModId, ModMetadata, Source},
};
use rimsort_rust::integrations::community::parse_rules;

fn fixture(extra: &str) -> (Catalog, Vec<ActiveEntry>) {
    let records = [
        ("bottom", Source::Local),
        ("normal", Source::Local),
        ("top", Source::Local),
        ("core", Source::Official),
        ("alpha", Source::Local),
    ]
    .into_iter()
    .map(|(name, source)| {
        let xml = format!(
            "<ModMetaData><name>{name}</name><packageId>{name}</packageId>{}</ModMetaData>",
            if name == "top" { extra } else { "" }
        );
        ModMetadata::new(
            ModId::new(format!("/fixture/{name}")),
            source,
            parse_about(xml.as_bytes(), &GameVersion::parse("1.5").unwrap()).unwrap(),
        )
    })
    .collect();
    let active = ["bottom", "normal", "top", "core", "alpha"]
        .into_iter()
        .map(|name| ActiveEntry::Resolved {
            mod_id: ModId::new(format!("/fixture/{name}")),
            original_token: name.into(),
        })
        .collect();
    (Catalog::new(records).unwrap(), active)
}
fn tokens(active: &[ActiveEntry]) -> Vec<&str> {
    active.iter().map(ActiveEntry::original_token).collect()
}

#[test]
fn background_snapshot_and_apply_gate_use_identical_rules_and_reject_stale_results() {
    use rimsort_rust::application::{jobs::*, session::Session};
    use rimsort_rust::integrations::config::parse_config;
    use std::sync::Arc;
    let (catalog, _) = fixture("");
    let config = parse_config(b"<ModsConfigData><activeMods><li>bottom</li><li>normal</li><li>top</li><li>core</li><li>alpha</li></activeMods></ModsConfigData>").unwrap();
    let mut session = Session::new(
        catalog,
        config,
        "/fixture/ModsConfig.xml".into(),
        GameVersion::parse("1.5").unwrap(),
    );
    let rules = Arc::new(parse_rules(br#"{"rules":{"top":{"loadTop":{"value":true}},"bottom":{"loadBottom":{"value":true}}}}"#).unwrap());
    assert!(session.set_rules(rules.clone()));
    assert!(!session.is_dirty());
    assert!(!session.set_rules(rules));
    let host = JobHost::new();
    let bound = JobSource::of(&session);
    host.dispatch(JobRequest {
        owner: JobOwner::Startup,
        bound: Some(bound),
        cancel: CancelToken::new(),
        work: JobWork::Sort(ModListSnapshot::from_session(&session)),
    });
    host.reap_workers();
    let Delivered::Current(mut completed) = host.poll_completion(Some(bound)).unwrap() else {
        panic!("current result")
    };
    let JobOutcome::Validated(report) = &mut completed.outcome else {
        panic!("sort result")
    };
    assert_eq!(
        tokens(report.order.as_ref().unwrap()),
        ["core", "top", "alpha", "normal", "bottom"]
    );
    let correct = report.order.clone();
    report.order = Ok(session.working_entries().to_vec());
    assert_eq!(
        apply_sort_order(&mut session, &completed),
        SortApplied::Refused
    );
    if let JobOutcome::Validated(report) = &mut completed.outcome {
        report.order = correct;
    }
    assert_eq!(
        apply_sort_order(&mut session, &completed),
        SortApplied::Published
    );
    assert!(session.is_dirty());
    let bound = JobSource::of(&session);
    let snapshot = ModListSnapshot::from_session(&session);
    session.set_rules(Arc::default());
    host.dispatch(JobRequest {
        owner: JobOwner::Startup,
        bound: Some(bound),
        cancel: CancelToken::new(),
        work: JobWork::Sort(snapshot),
    });
    host.reap_workers();
    assert!(matches!(
        host.poll_completion(Some(JobSource::of(&session))),
        Some(Delivered::Discarded { .. })
    ));
    assert_eq!(
        apply_sort_order(&mut session, &completed),
        SortApplied::Refused
    );
    assert!(session.is_dirty());
    let bound = JobSource::of(&session);
    let snapshot = ModListSnapshot::from_session(&session);
    session.set_rules(Arc::new(
        parse_rules(br#"{"timestamp":3,"rules":{"normal":{"loadTop":{"value":true}}}}"#).unwrap(),
    ));
    host.dispatch(JobRequest {
        owner: JobOwner::Startup,
        bound: Some(bound),
        cancel: CancelToken::new(),
        work: JobWork::Sort(snapshot),
    });
    host.reap_workers();
    assert!(matches!(
        host.poll_completion(Some(JobSource::of(&session))),
        Some(Delivered::Discarded { .. })
    ));
    assert!(session.is_dirty());
}

#[test]
fn tiers_override_alphabetical_order_but_never_official_content() {
    let (catalog, active) = fixture("");
    let rules = parse_rules(br#"{"rules":{"top":{"loadTop":{"value":true}},"bottom":{"loadBottom":{"value":true}},"core":{"loadBottom":{"value":true},"loadTop":{"value":true}},"inactive":{"loadTop":{"value":true},"loadBottom":{"value":true}}}}"#).unwrap();
    assert_eq!(
        tokens(&sort_active_with_rules(&catalog, &active, &rules).unwrap()),
        ["core", "top", "alpha", "normal", "bottom"]
    );
    assert_eq!(
        tokens(&sort_active(&catalog, &active).unwrap()),
        ["core", "alpha", "bottom", "normal", "top"]
    );
    let findings = rimsort_rust::domain::validation::validate_with_rules(
        &catalog,
        &active,
        &GameVersion::parse("1.5").unwrap(),
        &rules,
    );
    assert!(
        findings
            .iter()
            .any(|issue| issue.message.contains("flags ignored for official"))
    );
}

#[test]
fn contradictions_refuse_without_dropping_constraints_or_mutating_input() {
    for rule in [
        r#"{"top":{"loadTop":{"value":true},"loadBottom":{"value":true}}}"#,
        r#"{"top":{"loadTop":{"value":true},"loadAfter":{"normal":{"comment":"must follow"}}}}"#,
        r#"{"top":{"loadBefore":{"core":{}}}}"#,
        r#"{"top":{"loadAfter":{"normal":{}}},"normal":{"loadAfter":{"top":{}}}}"#,
        r#"{"top":{"loadAfter":{"top":{}}}}"#,
    ] {
        let (catalog, active) = fixture("");
        let old = active.clone();
        let rules = parse_rules(format!(r#"{{"rules":{rule}}}"#).as_bytes()).unwrap();
        let error = sort_active_with_rules(&catalog, &active, &rules).unwrap_err();
        assert!(error.to_string().contains("community"), "{error}");
        assert_eq!(active, old);
    }
}

#[test]
fn metadata_alternative_provider_cannot_be_discarded_by_top_flag() {
    let (catalog, active) = fixture(
        "<modDependencies><li><packageId>missing</packageId><alternativePackageIds><li>normal</li></alternativePackageIds></li></modDependencies>",
    );
    let rules = parse_rules(br#"{"rules":{"top":{"loadTop":{"value":true}}}}"#).unwrap();
    let error = sort_active_with_rules(&catalog, &active, &rules)
        .unwrap_err()
        .to_string();
    assert!(error.contains("dependency"), "{error}");
}

#[test]
fn explicit_edges_and_alphabetical_levels_are_stable_and_inactive_targets_ignored() {
    let (catalog, active) = fixture("");
    let rules = parse_rules(br#"{"rules":{"alpha":{"loadAfter":{"normal":{},"missing":{}}},"top":{"loadAfter":{"normal":{}}}}}"#).unwrap();
    for _ in 0..10 {
        assert_eq!(
            tokens(&sort_active_with_rules(&catalog, &active, &rules).unwrap()),
            ["core", "bottom", "normal", "alpha", "top"]
        );
    }
}
