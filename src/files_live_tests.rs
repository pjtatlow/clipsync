// Defaults to a disposable database; local-account testing requires explicit opt-in.
#[test]
#[ignore = "requires a local SpacetimeDB with the server module published"]
fn live_two_device_sync_conflict_resolution_and_rollback() {
    use crate::module_bindings::*;
    use spacetimedb_sdk::DbContext;
    use std::sync::Arc;
    fn wait(mut condition: impl FnMut() -> bool) {
        let start = Instant::now();
        while !condition() {
            assert!(
                start.elapsed() < Duration::from_secs(15),
                "live sync timed out"
            );
            std::thread::sleep(Duration::from_millis(30));
        }
    }
    fn connect(
        device: &str,
        server: &str,
        database: &str,
    ) -> (
        crossbeam_channel::Sender<SpacetimeCommand>,
        tokio::sync::mpsc::Receiver<crate::daemon::spacetime::SpacetimeEvent>,
        Arc<DbConnection>,
    ) {
        let use_local = std::env::var("CLIPSYNC_TEST_USE_LOCAL_ACCOUNT").as_deref() == Ok("1");
        let existing_token = if use_local {
            Some(
                config::load_token()
                    .unwrap()
                    .expect("Local account token required"),
            )
        } else {
            None
        };
        let (token_tx, token_rx) = std::sync::mpsc::channel();
        let conn = Arc::new(
            DbConnection::builder()
                .with_uri(server)
                .with_database_name(database)
                .with_token(existing_token)
                .on_connect(move |conn, _, token| {
                    let token = token.to_string();
                    conn.subscription_builder()
                        .on_applied(move |_| {
                            token_tx.send(token).unwrap();
                        })
                        .subscribe_to_all_tables();
                })
                .build()
                .unwrap(),
        );
        let _handle = conn.run_threaded();
        let token = token_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let (auth_tx, auth_rx) = std::sync::mpsc::channel();
        if use_local {
            conn.reducers
                .register_device_then(device.into(), device.into(), move |_, result| {
                    auth_tx.send(result.map_err(|e| format!("{e:?}"))).unwrap();
                })
                .unwrap();
        } else {
            conn.reducers
                .authenticate_then(
                    "file-integration-test".into(),
                    "integration-password-only".into(),
                    vec![],
                    vec![],
                    device.into(),
                    device.into(),
                    String::new(),
                    move |_, result| {
                        auth_tx.send(result.map_err(|e| format!("{e:?}"))).unwrap();
                    },
                )
                .unwrap();
        }
        auth_rx
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap()
            .unwrap();
        let (events, rx) = tokio::sync::mpsc::channel(32);
        let (tx, commands) = crossbeam_channel::unbounded();
        let config = config::Config {
            server_url: server.into(),
            database_name: database.into(),
            ..Default::default()
        };
        crate::daemon::spacetime::spawn_spacetime_thread(&config, Some(token), events, commands)
            .unwrap();
        (tx, rx, conn)
    }
    let server = std::env::var("CLIPSYNC_TEST_SERVER").unwrap_or("http://127.0.0.1:43127".into());
    let database = std::env::var("CLIPSYNC_TEST_DATABASE").unwrap_or("clipsync-file-test".into());
    let temp1 = Temp::new();
    let temp2 = Temp::new();
    let mut a = temp1.engine();
    let mut b = temp2.engine();
    a.device = format!("device-a-{}", uuid::Uuid::new_v4());
    b.device = format!("device-b-{}", uuid::Uuid::new_v4());
    // Both registered devices of an account use the same encryption identity.
    if std::env::var("CLIPSYNC_TEST_USE_LOCAL_ACCOUNT").as_deref() == Ok("1") {
        a.identity =
            crypto::load_private_key().expect("Local account encryption identity required");
    }
    use age::secrecy::ExposeSecret;
    b.identity = a.identity.to_string().expose_secret().parse().unwrap();
    let (tx1, _events1, conn1) = connect(&a.device, &server, &database);
    a.tx = tx1;
    let (tx2, _events2, conn2) = connect(&b.device, &server, &database);
    b.tx = tx2;
    let name = format!("test-{}", uuid::Uuid::new_v4());
    let path_a = temp1.0.join("config");
    let path_b = temp2.0.join("config");
    fs::write(&path_a, "initial\n").unwrap();
    add(&mut a, &name, path_a.clone());
    add(&mut b, &name, path_b.clone());
    a.queue(&name, snapshot("initial\n")).unwrap();
    // Do not let an empty new receiver publish until its initial subscription is ready.
    wait(|| {
        a.refresh();
        a.tick(&name).unwrap();
        !a.registry.collections[&name].base.is_empty()
    });
    wait(|| {
        b.refresh();
        b.tick(&name).unwrap();
        fs::read_to_string(&path_b).ok().as_deref() == Some("initial\n")
    });
    let initial = a
        .git(&name, &["rev-list", "--max-parents=0", "HEAD"])
        .unwrap()
        .trim()
        .to_string();
    let started = Instant::now();
    fs::write(&path_a, "updated\n").unwrap();
    a.queue(&name, snapshot("updated\n")).unwrap();
    wait(|| {
        a.refresh();
        a.tick(&name).unwrap();
        b.refresh();
        b.tick(&name).unwrap();
        fs::read_to_string(&path_b).ok().as_deref() == Some("updated\n")
            && a.registry.collections[&name].pending.is_none()
    });
    eprintln!("Live encrypted update installed in {:?}", started.elapsed());
    assert_eq!(
        a.registry.collections[&name].base,
        b.registry.collections[&name].base
    );
    // Competing publications share a parent; exactly one wins the server CAS.
    fs::write(&path_a, "edit-a\n").unwrap();
    fs::write(&path_b, "edit-b\n").unwrap();
    a.queue(&name, snapshot("edit-a\n")).unwrap();
    b.queue(&name, snapshot("edit-b\n")).unwrap();
    a.refresh();
    b.refresh();
    a.tick(&name).unwrap();
    b.tick(&name).unwrap();
    wait(|| {
        a.refresh();
        a.tick(&name).unwrap();
        b.refresh();
        b.tick(&name).unwrap();
        a.registry.collections[&name].conflict.is_some()
            || b.registry.collections[&name].conflict.is_some()
    });
    assert_eq!(fs::read_to_string(&path_a).unwrap(), "edit-a\n");
    assert_eq!(fs::read_to_string(&path_b).unwrap(), "edit-b\n");
    if a.registry.collections[&name].conflict.is_some() {
        a.command(FileCommand::Resolve {
            name: name.clone(),
            choice: Resolution::Remote,
        })
        .unwrap();
    }
    if b.registry.collections[&name].conflict.is_some() {
        b.command(FileCommand::Resolve {
            name: name.clone(),
            choice: Resolution::Remote,
        })
        .unwrap();
    }
    wait(|| {
        a.refresh();
        a.tick(&name).unwrap();
        b.refresh();
        b.tick(&name).unwrap();
        a.registry.collections[&name].pending.is_none()
            && b.registry.collections[&name].pending.is_none()
            && a.registry.collections[&name].base == b.registry.collections[&name].base
    });
    a.command(FileCommand::Rollback {
        name: name.clone(),
        commit: initial,
    })
    .unwrap();
    wait(|| {
        a.refresh();
        a.tick(&name).unwrap();
        b.refresh();
        b.tick(&name).unwrap();
        a.registry.collections[&name].pending.is_none()
            && fs::read_to_string(&path_b).ok().as_deref() == Some("initial\n")
    });
    wait(|| {
        a.refresh();
        a.tick(&name).unwrap();
        b.refresh();
        b.tick(&name).unwrap();
        a.remote.as_ref().is_some_and(|r| {
            r.applied
                .iter()
                .filter(|r| {
                    r.collection == name && r.revision == a.registry.collections[&name].base
                })
                .count()
                == 2
        })
    });
    assert_eq!(
        a.historical_snapshot(&name, "HEAD").unwrap(),
        snapshot("initial\n")
    );
    for (conn, device) in [(conn1, a.device.clone()), (conn2, b.device.clone())] {
        let (tx, rx) = std::sync::mpsc::channel();
        conn.reducers
            .unregister_device_then(device, move |_, result| {
                let _ = tx.send(result);
            })
            .unwrap();
        rx.recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap()
            .unwrap();
        conn.disconnect().unwrap();
    }
    eprintln!("Verified collection {name}; temporary devices unregistered");
}
