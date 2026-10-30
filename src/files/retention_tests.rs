fn sequenced_remote(e: &Engine, id: &str, parent: &str, sequence: u64, text: &str) -> RemoteState {
    let mut state = remote(e, "test", id, parent, snapshot(text));
    std::sync::Arc::make_mut(&mut state.revisions)[0].encrypted_data = seal(
        &e.identity,
        Envelope {
            version: 2,
            sequence: Some(sequence),
            collection: "test".into(),
            id: id.into(),
            parent: parent.into(),
            snapshot: snapshot(text),
        },
    )
    .unwrap();
    state
}

#[test]
fn pruned_ancestry_allows_offline_catchup_but_not_replay() {
    let t = Temp::new();
    let mut e = t.engine();
    let path = t.0.join("config");
    add(&mut e, "test", path.clone());
    e.remote = Some(sequenced_remote(&e, "first", "", 1, "first"));
    e.tick("test").unwrap();
    e.remote = Some(sequenced_remote(
        &e,
        "latest",
        "pruned-parent",
        100,
        "latest",
    ));
    e.tick("test").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "latest");
    let registry: Registry =
        serde_json::from_slice(&fs::read(e.root.join("registry.json")).unwrap()).unwrap();
    assert_eq!(registry.collections["test"].sequence, 100);
    for sequence in [1, 99, 100] {
        e.remote = Some(sequenced_remote(&e, "replayed", "pruned", sequence, "old"));
        assert!(e.tick("test").is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "latest");
    }
    e.remote = Some(remote(&e, "test", "legacy", "latest", snapshot("old")));
    assert!(e.tick("test").is_err());
}

#[test]
fn pruned_ancestry_preserves_offline_edits_and_local_rollback_history() {
    let t = Temp::new();
    let mut e = t.engine();
    let path = t.0.join("config");
    add(&mut e, "test", path.clone());
    e.remote = Some(sequenced_remote(&e, "first", "", 1, "first"));
    e.tick("test").unwrap();
    let old = e
        .git("test", &["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    fs::write(&path, "offline edit").unwrap();
    e.queue("test", snapshot("offline edit")).unwrap();
    e.remote = Some(sequenced_remote(
        &e,
        "latest",
        "pruned-parent",
        100,
        "latest",
    ));
    e.tick("test").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "offline edit");
    assert!(e.registry.collections["test"].conflict.is_some());
    assert_eq!(
        e.historical_snapshot("test", &old).unwrap(),
        snapshot("first")
    );
}

#[test]
fn sequence_is_authenticated_and_legacy_envelopes_still_open() {
    let t = Temp::new();
    let e = t.engine();
    let legacy = remote(&e, "test", "legacy", "", snapshot("old"));
    assert!(open(&e.identity, &legacy.revisions[0].encrypted_data).is_ok());
    let state = sequenced_remote(&e, "head", "", 1, "new");
    let plaintext =
        crypto::decrypt_limited(&state.revisions[0].encrypted_data, &e.identity, 1024 * 1024)
            .unwrap();
    let mut signed: SignedEnvelope = serde_json::from_slice(&plaintext).unwrap();
    signed.envelope.sequence = Some(200);
    let forged = crypto::encrypt(
        &serde_json::to_vec(&signed).unwrap(),
        &[e.identity.to_public()],
    )
    .unwrap();
    assert!(open(&e.identity, &forged).is_err());
}

#[test]
fn legacy_receiver_can_bridge_to_sequenced_head_after_pruning() {
    let t = Temp::new();
    let mut e = t.engine();
    let path = t.0.join("config");
    add(&mut e, "test", path.clone());
    e.remote = Some(remote(&e, "test", "legacy", "", snapshot("legacy")));
    e.tick("test").unwrap();
    e.remote = Some(sequenced_remote(&e, "new", "pruned", 50, "new"));
    e.tick("test").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "new");
    e.queue("test", snapshot("next")).unwrap();
    assert_eq!(
        e.registry.collections["test"]
            .pending
            .as_ref()
            .unwrap()
            .sequence,
        51
    );
}
