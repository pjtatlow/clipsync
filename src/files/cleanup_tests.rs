// Operator cleanup for UUID-named integration-test fixtures only. Never matches
// normal collection names or touches destination files on any computer.
#[test]
#[ignore = "explicitly authorized cloud fixture cleanup; requires local account"]
fn cleanup_test_collections() {
    use crate::module_bindings::*;
    use spacetimedb_sdk::{DbContext, Table};
    use std::sync::Arc;
    assert_eq!(
        std::env::var("CLIPSYNC_CLEANUP_TEST_COLLECTIONS").as_deref(),
        Ok("1")
    );
    let config = config::Config::load().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let conn = Arc::new(
        DbConnection::builder()
            .with_uri(&config.server_url)
            .with_database_name(&config.database_name)
            .with_token(config::load_token().unwrap())
            .on_connect(move |conn, _, _| {
                conn.subscription_builder()
                    .on_applied(move |_| {
                        let _ = tx.send(());
                    })
                    .subscribe_to_all_tables();
            })
            .build()
            .unwrap(),
    );
    let _handle = conn.run_threaded();
    rx.recv_timeout(Duration::from_secs(15)).unwrap();
    assert!(
        conn.db.my_profile().iter().next().is_some(),
        "Log in before cleanup"
    );
    let temp = Temp::new();
    let mut engine = temp.engine();
    engine.identity = crypto::load_private_key().unwrap();
    engine.remote = Some(RemoteState {
        heads: conn.db.my_file_heads().iter().collect(),
        revisions: Arc::new(conn.db.my_file_revisions().iter().collect()),
        applied: conn.db.my_file_applied().iter().collect(),
    });
    fn fixture_name(name: &str) -> bool {
        name.strip_prefix("test-directory-")
            .or_else(|| name.strip_prefix("test-"))
            .is_some_and(|suffix| uuid::Uuid::parse_str(suffix).is_ok())
    }
    let remote = engine.remote.as_ref().unwrap();
    let real_heads: Vec<_> = remote
        .heads
        .iter()
        .filter(|h| !fixture_name(&h.collection) && !h.collection.starts_with(DESTINATION_PREFIX))
        .map(|h| h.collection.clone())
        .collect();
    let real_revisions: Vec<_> = remote
        .revisions
        .iter()
        .filter(|r| !fixture_name(&r.collection) && !r.collection.starts_with(DESTINATION_PREFIX))
        .map(|r| r.id.clone())
        .collect();
    let fixtures: Vec<_> = remote
        .heads
        .iter()
        .filter(|h| fixture_name(&h.collection))
        .cloned()
        .collect();
    // Review the complete candidate set before issuing any deletion.
    for head in &fixtures {
        let snapshot = engine.decode(&head.collection, &head.revision).unwrap();
        let expected_file = if head.collection.starts_with("test-directory-") {
            "init.lua"
        } else {
            "file"
        };
        let entry = snapshot
            .entries
            .get(expected_file)
            .expect("Unexpected test fixture structure; refusing cleanup");
        assert_eq!(snapshot.entries.len(), 1);
        assert!(!entry.executable);
        if snapshot.directory {
            assert!(head.collection.starts_with("test-directory-"));
            assert_eq!(entry.data, b"return {}\n");
        } else {
            assert!(!head.collection.starts_with("test-directory-"));
            assert!(
                [
                    b"initial\n".as_slice(),
                    b"updated\n",
                    b"edit-a\n",
                    b"edit-b\n"
                ]
                .contains(&entry.data.as_slice()),
                "Unexpected contents; refusing cleanup"
            );
        }
    }
    for head in &fixtures {
        let (tx, rx) = std::sync::mpsc::channel();
        conn.reducers
            .delete_file_collection_then(
                head.collection.clone(),
                head.revision.clone(),
                move |_, result| {
                    let _ = tx.send(result);
                },
            )
            .unwrap();
        rx.recv_timeout(Duration::from_secs(15))
            .unwrap()
            .unwrap()
            .unwrap();
        eprintln!("Removed test collection {}", head.collection);
    }
    let start = Instant::now();
    loop {
        if !conn
            .db
            .my_file_heads()
            .iter()
            .any(|h| fixture_name(&h.collection))
        {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(30));
    }
    let remaining: Vec<_> = conn
        .db
        .my_file_heads()
        .iter()
        .map(|h| h.collection)
        .collect();
    assert!(
        real_heads.iter().all(|name| remaining.contains(name)),
        "A real collection was removed"
    );
    let revisions: std::collections::BTreeSet<_> =
        conn.db.my_file_revisions().iter().map(|r| r.id).collect();
    assert!(
        real_revisions.iter().all(|id| revisions.contains(id)),
        "Real history was removed"
    );
    for head in &fixtures {
        assert!(!remaining.contains(&super::bootstrap::destination_collection(&head.collection)));
        assert!(
            !conn
                .db
                .my_file_revisions()
                .iter()
                .any(|r| r.collection == head.collection)
        );
        assert!(
            !conn
                .db
                .my_file_applied()
                .iter()
                .any(|r| r.collection == head.collection)
        );
    }
    eprintln!(
        "Removed {} test collections; preserved real collections: {}",
        fixtures.len(),
        real_heads.join(", ")
    );
    conn.disconnect().unwrap();
}
