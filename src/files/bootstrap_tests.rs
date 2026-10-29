fn bootstrap_remote(e: &Engine, name: &str, relative: &str, snapshot: Snapshot) -> RemoteState {
    let mut state = remote(e, name, &format!("revision-{name}"), "", snapshot);
    let metadata_name = super::bootstrap::destination_collection(name);
    let metadata = Snapshot {
        directory: false,
        entries: BTreeMap::from([(
            "file".into(),
            Entry {
                data: serde_json::to_vec(&Destination {
                    version: 1,
                    collection: name.into(),
                    home_relative: Some(relative.into()),
                })
                .unwrap(),
                executable: false,
            },
        )]),
    };
    let catalog = remote(e, &metadata_name, &format!("metadata-{name}"), "", metadata);
    state.heads.extend(catalog.heads);
    std::sync::Arc::make_mut(&mut state.revisions).extend(catalog.revisions.iter().cloned());
    state
}
fn merge_remote(a: &mut RemoteState, b: RemoteState) {
    a.heads.extend(b.heads);
    std::sync::Arc::make_mut(&mut a.revisions).extend(b.revisions.iter().cloned());
}
fn rex_snapshot() -> Snapshot {
    Snapshot {
        directory: true,
        entries: BTreeMap::from([(
            "init.lua".into(),
            Entry {
                data: b"return {}\n".to_vec(),
                executable: false,
            },
        )]),
    }
}
#[test]
fn bootstrap_installs_all_mapped_configs_and_is_idempotent() {
    let t = Temp::new();
    let mut e = t.engine();
    let home = t.0.join("new-home");
    let mut state = bootstrap_remote(&e, "rex", ".config/rex", rex_snapshot());
    merge_remote(
        &mut state,
        bootstrap_remote(&e, "shell", ".zshrc", snapshot("export EDITOR=nvim\n")),
    );
    e.remote = Some(state);
    let result = e.bootstrap(&home, false, false, &[], &[]).unwrap();
    assert!(result.contains("Installed/adopted 2"));
    assert_eq!(
        fs::read_to_string(home.join(".config/rex/init.lua")).unwrap(),
        "return {}\n"
    );
    assert_eq!(
        fs::read_to_string(home.join(".zshrc")).unwrap(),
        "export EDITOR=nvim\n"
    );
    assert_eq!(e.registry.collections.len(), 2);
    let head = e.git("rex", &["rev-parse", "HEAD"]).unwrap();
    assert!(
        e.bootstrap(&home, false, false, &[], &[])
            .unwrap()
            .contains("already registered rex")
    );
    assert_eq!(e.git("rex", &["rev-parse", "HEAD"]).unwrap(), head);
}
#[test]
fn bootstrap_preview_has_no_registration_or_filesystem_writes() {
    let t = Temp::new();
    let mut e = t.engine();
    let home = t.0.join("new-home");
    e.remote = Some(bootstrap_remote(&e, "rex", ".config/rex", rex_snapshot()));
    assert!(
        e.bootstrap(&home, true, false, &[], &[])
            .unwrap()
            .contains("install rex")
    );
    assert!(e.registry.collections.is_empty());
    assert!(!home.exists());
    assert!(!e.root.join("registry.json").exists());
    assert!(!e.repo("rex").exists());
}
#[test]
fn bootstrap_preserves_local_conflicts_and_replacement_keeps_encrypted_backup() {
    let t = Temp::new();
    let mut e = t.engine();
    let home = t.0.join("home");
    fs::create_dir(&home).unwrap();
    fs::write(home.join("config"), "my local config").unwrap();
    e.remote = Some(bootstrap_remote(
        &e,
        "test",
        "config",
        snapshot("remote config"),
    ));
    let result = e.bootstrap(&home, false, false, &[], &[]).unwrap();
    assert!(result.contains("preserved 1 local conflicts"));
    assert_eq!(
        fs::read_to_string(home.join("config")).unwrap(),
        "my local config"
    );
    assert!(e.registry.collections["test"].conflict.is_some());
    let initial = e
        .git("test", &["rev-list", "--max-parents=0", "HEAD"])
        .unwrap();
    assert_eq!(
        e.historical_snapshot("test", initial.trim()).unwrap(),
        snapshot("my local config")
    );
    e.bootstrap(&home, false, true, &[], &[]).unwrap();
    assert_eq!(
        fs::read_to_string(home.join("config")).unwrap(),
        "remote config"
    );
    assert!(e.registry.collections["test"].conflict.is_none());
    assert_eq!(
        e.historical_snapshot("test", initial.trim()).unwrap(),
        snapshot("my local config")
    );
}
#[test]
fn bootstrap_supports_overrides_and_reports_missing_destinations() {
    let t = Temp::new();
    let mut e = t.engine();
    let home = t.0.join("home");
    e.remote = Some(remote(&e, "test", "r", "", snapshot("data")));
    assert!(
        e.bootstrap(&home, false, false, &[], &[])
            .unwrap()
            .contains("no saved destination")
    );
    assert!(e.registry.collections.is_empty());
    let custom = t.0.join("custom/config");
    e.bootstrap(
        &home,
        false,
        false,
        &["test".into()],
        &[format!("test={}", custom.display())],
    )
    .unwrap();
    assert_eq!(fs::read_to_string(&custom).unwrap(), "data");
    assert!(
        e.bootstrap(
            &home,
            false,
            false,
            &[],
            &[format!("test={}", t.0.join("elsewhere").display())]
        )
        .is_err()
    );
}
#[test]
fn bootstrap_rejects_traversal_and_overlaps_before_installing_anything() {
    let t = Temp::new();
    let mut e = t.engine();
    let home = t.0.join("home");
    e.remote = Some(bootstrap_remote(
        &e,
        "escape",
        "../outside",
        snapshot("data"),
    ));
    assert!(e.bootstrap(&home, false, false, &[], &[]).is_err());
    assert!(!home.exists());
    assert!(!t.0.join("outside").exists());
    let mut state = bootstrap_remote(&e, "a", ".config", rex_snapshot());
    merge_remote(
        &mut state,
        bootstrap_remote(&e, "b", ".config/rex", rex_snapshot()),
    );
    e.remote = Some(state);
    assert!(
        e.bootstrap(&home, false, false, &[], &[])
            .unwrap_err()
            .to_string()
            .contains("overlap")
    );
    assert!(e.registry.collections.is_empty());
    assert!(!home.exists());
}
#[test]
fn bootstrap_only_filters_collections_and_rejects_unknown_names() {
    let t = Temp::new();
    let mut e = t.engine();
    let home = t.0.join("home");
    let mut state = bootstrap_remote(&e, "rex", ".config/rex", rex_snapshot());
    merge_remote(
        &mut state,
        bootstrap_remote(&e, "shell", ".zshrc", snapshot("export EDITOR=nvim\n")),
    );
    e.remote = Some(state);
    assert!(
        e.bootstrap(&home, false, false, &["unknown".into()], &[])
            .is_err()
    );
    assert!(e.registry.collections.is_empty());
    e.bootstrap(&home, false, false, &["rex".into()], &[])
        .unwrap();
    assert_eq!(e.registry.collections.len(), 1);
    assert!(!home.join(".zshrc").exists());
}
#[test]
fn destination_metadata_is_durable_encrypted_and_keeps_first_default() {
    let t = Temp::new();
    let mut e = t.engine();
    let home = t.0.join("home");
    let path = home.join(".config/rex");
    fs::create_dir_all(&path).unwrap();
    add(&mut e, "rex", path);
    e.publish_destinations(&home).unwrap();
    let pending = &e.registry.destinations["rex"];
    let envelope = open(&e.identity, &pending.encrypted).unwrap();
    let destination: Destination =
        serde_json::from_slice(&envelope.snapshot.entries["file"].data).unwrap();
    assert_eq!(destination.home_relative.as_deref(), Some(".config/rex"));
    assert!(
        !destination
            .home_relative
            .as_ref()
            .unwrap()
            .contains(home.to_str().unwrap())
    );
    let saved: Registry =
        serde_json::from_slice(&fs::read(e.root.join("registry.json")).unwrap()).unwrap();
    assert_eq!(saved.destinations["rex"].encrypted, pending.encrypted);
    e.remote = Some(bootstrap_remote(&e, "rex", "first/default", rex_snapshot()));
    e.publish_destinations(&home).unwrap();
    assert!(e.registry.destinations.is_empty());
    assert!(
        e.bootstrap(&t.0.join("different-home"), true, false, &[], &[])
            .unwrap()
            .contains("already registered")
    );
}
#[test]
fn destination_metadata_omits_paths_outside_home_and_updates_unpublished_remaps() {
    let t = Temp::new();
    let mut e = t.engine();
    let home = t.0.join("home");
    fs::create_dir_all(&home).unwrap();
    let path = home.join("old");
    fs::write(&path, "x").unwrap();
    add(&mut e, "test", path);
    e.publish_destinations(&home).unwrap();
    let first = e.registry.destinations["test"].id.clone();
    e.registry.collections.get_mut("test").unwrap().path = home.join("new");
    e.publish_destinations(&home).unwrap();
    assert_ne!(e.registry.destinations["test"].id, first);
    e.registry.collections.get_mut("test").unwrap().path = t.0.join("outside-home");
    e.publish_destinations(&home).unwrap();
    assert!(e.registry.destinations.is_empty());
}
#[test]
fn bootstrap_never_registers_reserved_metadata_streams_or_own_state() {
    let t = Temp::new();
    let mut e = t.engine();
    let home = t.0.join("home");
    e.remote = Some(bootstrap_remote(&e, "rex", ".config/rex", rex_snapshot()));
    let result = e.bootstrap(&home, true, false, &[], &[]).unwrap();
    assert!(!result.contains(DESTINATION_PREFIX));
    let name = super::bootstrap::destination_collection("rex");
    assert!(
        e.command(FileCommand::Add {
            name,
            path: home.join("bad"),
            directory: false
        })
        .is_err()
    );
    let mapping = format!("rex={}", e.root.join("own-state").display());
    assert!(e.bootstrap(&home, false, false, &[], &[mapping]).is_err());
    assert!(e.registry.collections.is_empty());
}

#[test]
fn a_collection_without_a_portable_default_is_skipped_until_explicitly_mapped() {
    let t = Temp::new();
    let mut e = t.engine();
    let home = t.0.join("home");
    let mut state = remote(&e, "test", "r", "", snapshot("data"));
    let metadata_name = super::bootstrap::destination_collection("test");
    let metadata = Snapshot {
        directory: false,
        entries: BTreeMap::from([(
            "file".into(),
            Entry {
                data: serde_json::to_vec(&Destination {
                    version: 1,
                    collection: "test".into(),
                    home_relative: None,
                })
                .unwrap(),
                executable: false,
            },
        )]),
    };
    merge_remote(
        &mut state,
        remote(&e, &metadata_name, "metadata", "", metadata),
    );
    e.remote = Some(state);
    assert!(
        e.bootstrap(&home, false, false, &[], &[])
            .unwrap()
            .contains("no saved destination")
    );
    assert!(!home.exists());
    assert!(e.registry.collections.is_empty());
    e.bootstrap(
        &home,
        false,
        false,
        &[],
        &[format!("test={}", home.join("chosen").display())],
    )
    .unwrap();
    assert_eq!(fs::read_to_string(home.join("chosen")).unwrap(), "data");
    e.publish_destinations(&home).unwrap();
    assert!(
        e.registry.destinations.is_empty(),
        "Local override must not re-advertise a cleared shared default"
    );
}
