#[test]
fn discovery_automatically_enrolls_missing_and_matching_configs() {
    let t = Temp::new();
    let mut e = t.engine();
    let home = t.0.join("home");
    fs::create_dir(&home).unwrap();
    fs::write(home.join("matching"), "same").unwrap();
    let mut state = bootstrap_remote(&e, "new", "new/config", snapshot("new config"));
    merge_remote(
        &mut state,
        bootstrap_remote(&e, "matching", "matching", snapshot("same")),
    );
    e.remote = Some(state);
    e.discover_collections(&home, &[]);
    assert_eq!(e.registry.collections.len(), 2);
    assert!(e.discovery.is_empty());
    assert_eq!(
        fs::read_to_string(home.join("new/config")).unwrap(),
        "new config"
    );
    assert_eq!(fs::read_to_string(home.join("matching")).unwrap(), "same");
    let head = e.git("new", &["rev-parse", "HEAD"]).unwrap();
    e.discover_collections(&home, &[]);
    assert_eq!(e.git("new", &["rev-parse", "HEAD"]).unwrap(), head);
    assert!(e.discovery_snapshots.is_empty());
}
#[test]
fn discovery_keeps_conflicts_unregistered_without_blocking_other_configs() {
    let t = Temp::new();
    let mut e = t.engine();
    let home = t.0.join("home");
    fs::create_dir(&home).unwrap();
    fs::write(home.join("conflict"), "my config").unwrap();
    let mut state = bootstrap_remote(&e, "conflict", "conflict", snapshot("cloud config"));
    merge_remote(
        &mut state,
        bootstrap_remote(&e, "ready", "ready", snapshot("ready")),
    );
    e.remote = Some(state);
    e.discover_collections(&home, &[]);
    assert!(!e.registry.collections.contains_key("conflict"));
    assert_eq!(
        fs::read_to_string(home.join("conflict")).unwrap(),
        "my config"
    );
    assert!(!e.repo("conflict").exists());
    assert!(e.discovery["conflict"].contains("local files differ"));
    assert!(
        e.command(FileCommand::Status)
            .unwrap()
            .contains("Not watching conflict")
    );
    assert_eq!(fs::read_to_string(home.join("ready")).unwrap(), "ready");
}
#[test]
fn discovery_never_picks_a_winner_for_overlapping_unregistered_destinations() {
    let t = Temp::new();
    let mut e = t.engine();
    let home = t.0.join("home");
    fs::create_dir_all(home.join(".config")).unwrap();
    fs::write(home.join(".config/personal"), "keep").unwrap();
    let mut state = bootstrap_remote(&e, "a", ".config", rex_snapshot());
    merge_remote(
        &mut state,
        bootstrap_remote(&e, "b", ".config/rex", rex_snapshot()),
    );
    e.remote = Some(state);
    e.discover_collections(&home, &[]);
    assert!(e.registry.collections.is_empty());
    assert!(!home.join(".config/rex").exists());
    assert!(e.discovery["a"].contains("overlaps"));
    assert!(e.discovery["b"].contains("overlaps"));
    assert_eq!(
        fs::read_to_string(home.join(".config/personal")).unwrap(),
        "keep"
    );
}
#[test]
fn removing_a_watch_persists_an_opt_out_and_explicit_bootstrap_can_reenable_it() {
    let t = Temp::new();
    let mut e = t.engine();
    let home = t.0.join("home");
    e.remote = Some(bootstrap_remote(&e, "test", "config", snapshot("data")));
    e.discover_collections(&home, &[]);
    e.command(FileCommand::Remove {
        name: "test".into(),
    })
    .unwrap();
    let saved: Registry =
        serde_json::from_slice(&fs::read(e.root.join("registry.json")).unwrap()).unwrap();
    assert!(saved.ignored.contains("test"));
    e.registry = saved;
    e.discover_collections(&home, &[]);
    assert!(!e.registry.collections.contains_key("test"));
    assert!(
        e.bootstrap(&home, false, false, &[], &[])
            .unwrap()
            .contains("locally excluded")
    );
    assert!(!e.registry.collections.contains_key("test"));
    e.bootstrap(&home, false, false, &["test".into()], &[])
        .unwrap();
    assert!(e.registry.collections.contains_key("test"));
    assert!(!e.registry.ignored.contains("test"));
}
#[test]
fn global_auto_add_opt_out_still_reports_available_configs_and_allows_manual_bootstrap() {
    let t = Temp::new();
    let mut e = t.engine();
    let home = t.0.join("home");
    e.auto_add_collections = false;
    e.remote = Some(bootstrap_remote(&e, "test", "config", snapshot("data")));
    e.discover_collections(&home, &[]);
    assert!(!home.exists());
    assert!(e.registry.collections.is_empty());
    assert!(e.discovery["test"].contains("automatic enrollment is disabled"));
    e.bootstrap(&home, false, false, &[], &[]).unwrap();
    assert_eq!(fs::read_to_string(home.join("config")).unwrap(), "data");
}
#[test]
fn discovery_rechecks_changed_heads_and_local_conflicts_without_repeated_decryption() {
    let t = Temp::new();
    let mut e = t.engine();
    let home = t.0.join("home");
    fs::create_dir(&home).unwrap();
    fs::write(home.join("config"), "mine").unwrap();
    let first = bootstrap_remote(&e, "test", "config", snapshot("old cloud config"));
    e.remote = Some(first);
    e.discover_collections(&home, &[]);
    assert!(e.discovery.contains_key("test"));
    assert_eq!(e.discovery_snapshots["test"].0, "revision-test");
    let mut next = remote(&e, "test", "newer", "revision-test", snapshot("mine"));
    let previous = e.remote.as_ref().unwrap();
    next.heads.extend(
        previous
            .heads
            .iter()
            .filter(|h| h.collection.starts_with(DESTINATION_PREFIX))
            .cloned(),
    );
    std::sync::Arc::make_mut(&mut next.revisions).extend(previous.revisions.iter().cloned());
    e.remote = Some(next);
    e.discover_collections(&home, &[]);
    assert_eq!(e.registry.collections["test"].base, "newer");
    assert!(e.discovery.is_empty());
    assert!(e.discovery_snapshots.is_empty());
    assert_eq!(fs::read_to_string(home.join("config")).unwrap(), "mine");
}
#[test]
fn discovery_reports_unmapped_and_unsafe_destinations_without_writing_files() {
    let t = Temp::new();
    let mut e = t.engine();
    let home = t.0.join("home");
    let mut state = remote(&e, "unmapped", "r", "", snapshot("x"));
    merge_remote(
        &mut state,
        bootstrap_remote(&e, "unsafe", "../outside", snapshot("x")),
    );
    e.remote = Some(state);
    e.discover_collections(&home, &[]);
    assert!(e.registry.collections.is_empty());
    assert!(!home.exists());
    assert!(e.discovery["unmapped"].contains("no saved destination"));
    assert!(e.discovery["unsafe"].contains("Unsafe managed path"));
}

#[test]
fn discovery_rejects_empty_directory_file_collisions_before_registering() {
    let t = Temp::new();
    let mut e = t.engine();
    let home = t.0.join("home");
    fs::create_dir_all(home.join("rex/init.lua")).unwrap();
    e.remote = Some(bootstrap_remote(&e, "rex", "rex", rex_snapshot()));
    e.discover_collections(&home, &[]);
    assert!(e.registry.collections.is_empty());
    assert!(e.discovery["rex"].contains("collision"));
    assert!(home.join("rex/init.lua").is_dir());
    assert!(!e.repo("rex").exists());
}
#[test]
fn discovery_rejects_case_only_destination_aliases_on_every_platform() {
    let t = Temp::new();
    let mut e = t.engine();
    let home = t.0.join("home");
    let mut state = bootstrap_remote(&e, "a", ".config/rex", rex_snapshot());
    merge_remote(
        &mut state,
        bootstrap_remote(&e, "b", ".config/Rex", rex_snapshot()),
    );
    e.remote = Some(state);
    e.discover_collections(&home, &[]);
    assert!(e.registry.collections.is_empty());
    assert!(!home.exists());
    assert!(e.discovery["a"].contains("overlaps"));
    assert!(e.discovery["b"].contains("overlaps"));
}
#[test]
fn discovery_protects_case_only_aliases_of_clipsync_state() {
    let t = Temp::new();
    let e = t.engine();
    let alias = PathBuf::from(e.root.to_string_lossy().to_uppercase()).join("config");
    assert!(e.validate_new_path(&alias).is_err());
}

#[test]
fn portable_snapshots_reject_case_only_file_directory_collisions() {
    let mut snapshot = rex_snapshot();
    snapshot.entries.clear();
    snapshot.entries.insert(
        "dir".into(),
        Entry {
            data: b"file".to_vec(),
            executable: false,
        },
    );
    snapshot.entries.insert(
        "DIR/child".into(),
        Entry {
            data: b"child".to_vec(),
            executable: false,
        },
    );
    assert!(snapshot.validate().is_err());
}

#[test]
fn an_advertised_conflict_can_be_excluded_without_registering_it() {
    let t = Temp::new();
    let mut e = t.engine();
    let home = t.0.join("home");
    fs::create_dir(&home).unwrap();
    fs::write(home.join("config"), "mine").unwrap();
    e.remote = Some(bootstrap_remote(&e, "test", "config", snapshot("cloud")));
    e.discover_collections(&home, &[]);
    assert!(e.discovery.contains_key("test"));
    e.command(FileCommand::Remove {
        name: "test".into(),
    })
    .unwrap();
    assert!(
        !e.command(FileCommand::Status)
            .unwrap()
            .contains("Not watching test")
    );
    e.discover_collections(&home, &[]);
    assert!(e.registry.collections.is_empty());
    assert!(e.registry.ignored.contains("test"));
    assert!(e.discovery.is_empty());
    assert_eq!(fs::read_to_string(home.join("config")).unwrap(), "mine");
}
