// Live mode uses the local admin only to invite a separate, isolated test account.
#[test]
#[ignore = "requires a local SpacetimeDB with the server module published"]
fn live_two_device_sync_conflict_resolution_and_rollback() {
    use crate::module_bindings::*;
    use spacetimedb_sdk::{DbContext, Table};
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
    struct TestLogin {
        username: String,
        password: String,
        invite: String,
        public_key: Vec<u8>,
        encrypted_private_key: Vec<u8>,
    }
    fn invite_isolated_account(server: &str, database: &str) -> String {
        let configured = config::Config::load().unwrap();
        assert_eq!(
            server, configured.server_url,
            "Use the configured service for live tests"
        );
        assert_eq!(database, configured.database_name);
        let (tx, rx) = std::sync::mpsc::channel();
        let owner = Arc::new(
            DbConnection::builder()
                .with_uri(server)
                .with_database_name(database)
                .with_token(config::load_token().unwrap())
                .on_connect(move |conn, _, _| {
                    conn.subscription_builder()
                        .on_applied(move |_| {
                            let _ = tx.send(());
                        })
                        .subscribe(["SELECT * FROM my_profile"]);
                })
                .build()
                .unwrap(),
        );
        let _handle = owner.run_threaded();
        rx.recv_timeout(Duration::from_secs(15)).unwrap();
        assert!(
            owner.db.my_profile().iter().any(|p| p.is_admin),
            "Live tests need a local admin account to invite an isolated account"
        );
        let invite = format!(
            "{}{}-AaBbCcDdEeFfGgHh",
            uuid::Uuid::new_v4(),
            uuid::Uuid::new_v4()
        );
        let (tx, rx) = std::sync::mpsc::channel();
        owner
            .reducers
            .create_invite_code_then(invite.clone(), move |_, result| {
                let _ = tx.send(result);
            })
            .unwrap();
        rx.recv_timeout(Duration::from_secs(15))
            .unwrap()
            .unwrap()
            .unwrap();
        owner.disconnect().unwrap();
        invite
    }
    fn connect(
        device: &str,
        server: &str,
        database: &str,
        login: &TestLogin,
    ) -> (
        crossbeam_channel::Sender<SpacetimeCommand>,
        tokio::sync::mpsc::Receiver<crate::daemon::spacetime::SpacetimeEvent>,
        Arc<DbConnection>,
    ) {
        let (token_tx, token_rx) = std::sync::mpsc::channel();
        let conn = Arc::new(
            DbConnection::builder()
                .with_uri(server)
                .with_database_name(database)
                .with_token(None::<String>)
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
        assert!(
            conn.db.my_profile().iter().next().is_none(),
            "Fixture connection must start anonymous; refusing to change an existing account identity"
        );
        let (auth_tx, auth_rx) = std::sync::mpsc::channel();
        conn.reducers
            .authenticate_then(
                login.username.clone(),
                login.password.clone(),
                login.encrypted_private_key.clone(),
                login.public_key.clone(),
                device.into(),
                device.into(),
                login.invite.clone(),
                move |_, result| {
                    let _ = auth_tx.send(result.map_err(|e| format!("{e:?}")));
                },
            )
            .unwrap();
        auth_rx
            .recv_timeout(Duration::from_secs(10))
            .unwrap()
            .unwrap()
            .unwrap();
        wait(|| {
            conn.db
                .my_profile()
                .iter()
                .any(|profile| profile.username == login.username)
        });
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
    // All fixture data stays in a separate account in live mode. Real clients
    // therefore cannot automatically enroll these temporary collections.
    use age::secrecy::ExposeSecret;
    let live = std::env::var("CLIPSYNC_TEST_USE_LOCAL_ACCOUNT").as_deref() == Ok("1");
    let password = if live {
        format!("{}{}", uuid::Uuid::new_v4(), uuid::Uuid::new_v4())
    } else {
        "integration-password-only".into()
    };
    let login = TestLogin {
        username: if live {
            format!("clipsync-test-{}", uuid::Uuid::new_v4())
        } else {
            "file-integration-test".into()
        },
        encrypted_private_key: crypto::encrypt_with_passphrase(
            a.identity.to_string().expose_secret().as_bytes(),
            &password,
        )
        .unwrap(),
        public_key: crypto::public_key_bytes(&a.identity.to_public()),
        password,
        invite: if live {
            invite_isolated_account(&server, &database)
        } else {
            String::new()
        },
    };
    b.identity = a.identity.to_string().expose_secret().parse().unwrap();
    let (tx1, _events1, conn1) = connect(&a.device, &server, &database, &login);
    a.tx = tx1;
    let (tx2, _events2, conn2) = connect(&b.device, &server, &database, &login);
    b.tx = tx2;
    let name = format!("test-{}", uuid::Uuid::new_v4());
    let path_a = temp1.0.join("config");
    let path_b = temp2.0.join("config");
    fs::write(&path_a, "initial\n").unwrap();
    add(&mut a, &name, path_a.clone());
    add(&mut b, &name, path_b.clone());
    // Seed a legacy revision, then verify the first v2 write enables retention.
    let legacy_id = uuid::Uuid::new_v4().to_string();
    let legacy = seal(
        &a.identity,
        Envelope {
            version: 1,
            sequence: None,
            collection: name.clone(),
            id: legacy_id.clone(),
            parent: String::new(),
            snapshot: snapshot("initial\n"),
        },
    )
    .unwrap();
    let (seed_tx, seed_rx) = std::sync::mpsc::channel();
    conn1
        .reducers
        .publish_file_revision_then(
            name.clone(),
            String::new(),
            legacy_id,
            a.device.clone(),
            legacy,
            move |_, result| {
                let _ = seed_tx.send(result);
            },
        )
        .unwrap();
    seed_rx
        .recv_timeout(Duration::from_secs(15))
        .unwrap()
        .unwrap()
        .unwrap();
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
        commit: initial.clone(),
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
    // Join an existing directory collection at an absent destination without
    // --directory: the authenticated remote snapshot must establish its type.
    let directory_name = format!("test-directory-{}", uuid::Uuid::new_v4());
    let directory_a = temp1.0.join("rex");
    let directory_b = temp2.0.join("rex");
    fs::create_dir(&directory_a).unwrap();
    fs::write(directory_a.join("init.lua"), "return {}\n").unwrap();
    add(&mut a, &directory_name, directory_a.clone());
    a.queue(&directory_name, scan(&directory_a, true).unwrap())
        .unwrap();
    wait(|| {
        a.refresh();
        a.tick(&directory_name).unwrap();
        !a.registry.collections[&directory_name].base.is_empty()
    });
    add(&mut b, &directory_name, directory_b.clone());
    wait(|| {
        b.refresh();
        b.tick(&directory_name).unwrap();
        fs::read_to_string(directory_b.join("init.lua"))
            .ok()
            .as_deref()
            == Some("return {}\n")
    });
    assert!(b.registry.collections[&directory_name].directory);
    assert!(b.registry.collections[&directory_name].error.is_none());
    // An upgraded source advertises encrypted, home-relative destinations. A fresh
    // third device can discover and initialize both collections in one operation.
    wait(|| {
        a.refresh();
        a.publish_destinations(&temp1.0).unwrap();
        !a.head(&super::bootstrap::destination_collection(&name))
            .is_empty()
            && !a
                .head(&super::bootstrap::destination_collection(&directory_name))
                .is_empty()
    });
    let temp3 = Temp::new();
    let mut c = temp3.engine();
    c.identity = a.identity.to_string().expose_secret().parse().unwrap();
    c.device = format!("device-bootstrap-{}", uuid::Uuid::new_v4());
    let (tx3, _events3, conn3) = connect(&c.device, &server, &database, &login);
    c.tx = tx3;
    wait(|| {
        c.refresh();
        c.remote.is_some()
    });
    let home = temp3.0.join("new-home");
    let mut selected = vec![name.clone(), directory_name.clone()];
    let preview = c.bootstrap(&home, true, false, &selected, &[]).unwrap();
    assert!(preview.contains("Preview only"));
    assert!(!home.exists());
    let result = c.bootstrap(&home, false, false, &selected, &[]).unwrap();
    assert!(result.contains("Installed/adopted 2"));
    assert_eq!(c.registry.collections.len(), 2);
    assert_eq!(
        fs::read_to_string(home.join("config")).unwrap(),
        "initial\n"
    );
    assert_eq!(
        fs::read_to_string(home.join("rex/init.lua")).unwrap(),
        "return {}\n"
    );
    wait(|| {
        c.refresh();
        for collection in &selected {
            c.tick(collection).unwrap();
        }
        a.refresh();
        a.remote.as_ref().is_some_and(|r| {
            selected.iter().all(|collection| {
                r.applied.iter().any(|ack| {
                    ack.device_id == c.device
                        && ack.collection == *collection
                        && ack.revision == c.registry.collections[collection].base
                })
            })
        })
    });
    // A newly registered collection appears on the other device automatically,
    // without another bootstrap. A different local file is left untouched.
    for (relative, contents) in [
        ("new-config", "new shared config"),
        ("conflicting-config", "cloud config"),
    ] {
        let new_name = format!("test-{}", uuid::Uuid::new_v4());
        let source = temp1.0.join(relative);
        fs::write(&source, contents).unwrap();
        add(&mut a, &new_name, source);
        if relative == "conflicting-config" {
            fs::write(home.join(relative), "keep my local config").unwrap();
        }
        a.queue(&new_name, snapshot(contents)).unwrap();
        wait(|| {
            a.refresh();
            a.publish_destinations(&temp1.0).unwrap();
            a.tick(&new_name).unwrap();
            !a.registry.collections[&new_name].base.is_empty()
                && !a
                    .head(&super::bootstrap::destination_collection(&new_name))
                    .is_empty()
        });
        wait(|| {
            c.refresh();
            c.discover_collections(&home, &[]);
            if relative == "conflicting-config" {
                c.discovery.contains_key(&new_name)
            } else {
                c.registry.collections.contains_key(&new_name)
            }
        });
        if relative == "conflicting-config" {
            assert!(!c.registry.collections.contains_key(&new_name));
            assert_eq!(
                fs::read_to_string(home.join(relative)).unwrap(),
                "keep my local config"
            );
        } else {
            assert_eq!(fs::read_to_string(home.join(relative)).unwrap(), contents);
        }
        selected.push(new_name);
    }
    // Leave B and C offline past the cloud window, including one local edit.
    let offline_base = b.registry.collections[&name].base.clone();
    fs::write(home.join("config"), "offline conflicting edit").unwrap();
    for index in 0..45 {
        let text = format!("retained-{index}\n");
        fs::write(&path_a, &text).unwrap();
        a.queue(&name, snapshot(&text)).unwrap();
        wait(|| {
            a.refresh();
            a.tick(&name).unwrap();
            a.registry.collections[&name].pending.is_none()
        });
        let rows: Vec<_> = a
            .remote
            .as_ref()
            .unwrap()
            .revisions
            .iter()
            .filter(|r| r.collection == name)
            .collect();
        assert!(rows.len() <= 20);
    }
    assert!(
        !a.remote
            .as_ref()
            .unwrap()
            .revisions
            .iter()
            .any(|r| r.id == offline_base)
    );
    wait(|| {
        b.refresh();
        b.tick(&name).unwrap();
        fs::read_to_string(&path_b).unwrap() == "retained-44\n"
    });
    c.refresh();
    c.tick(&name).unwrap();
    assert_eq!(
        fs::read_to_string(home.join("config")).unwrap(),
        "offline conflicting edit"
    );
    assert!(c.registry.collections[&name].conflict.is_some());
    assert!(
        a.git(&name, &["rev-list", "--count", "HEAD"])
            .unwrap()
            .trim()
            .parse::<usize>()
            .unwrap()
            > 45
    );
    assert_eq!(
        a.historical_snapshot(&name, &initial).unwrap(),
        snapshot("initial\n")
    );
    // A legacy writer must not bypass retention after a collection opts in.
    let (legacy_tx, legacy_rx) = std::sync::mpsc::channel();
    conn1
        .reducers
        .publish_file_revision_then(
            name.clone(),
            a.head(&name),
            uuid::Uuid::new_v4().to_string(),
            a.device.clone(),
            vec![1],
            move |_, result| {
                let _ = legacy_tx.send(result);
            },
        )
        .unwrap();
    assert!(
        legacy_rx
            .recv_timeout(Duration::from_secs(15))
            .unwrap()
            .unwrap()
            .is_err()
    );
    let before = a.head(&name);
    let (bad_tx, bad_rx) = std::sync::mpsc::channel();
    conn1
        .reducers
        .publish_file_revision_v_2_then(
            name.clone(),
            before.clone(),
            uuid::Uuid::new_v4().to_string(),
            a.device.clone(),
            vec![1],
            a.registry.collections[&name].sequence,
            move |_, result| {
                let _ = bad_tx.send(result);
            },
        )
        .unwrap();
    assert!(
        bad_rx
            .recv_timeout(Duration::from_secs(15))
            .unwrap()
            .unwrap()
            .is_err()
    );
    a.refresh();
    assert_eq!(a.head(&name), before);
    eprintln!(
        "Cloud history stayed at 20 snapshots; offline catchup, conflicts and local history verified"
    );
    // Delete fixture heads, encrypted history, acknowledgements and destinations,
    // rather than leaving test collections visible in bootstrap previews.
    // A stale reviewed head must not delete a collection.
    let (guard_tx, guard_rx) = std::sync::mpsc::channel();
    conn1
        .reducers
        .delete_file_collection_then(name.clone(), "stale-review".into(), move |_, result| {
            let _ = guard_tx.send(result);
        })
        .unwrap();
    assert!(
        guard_rx
            .recv_timeout(Duration::from_secs(15))
            .unwrap()
            .unwrap()
            .is_err()
    );
    a.refresh();
    assert!(!a.head(&name).is_empty());
    for collection in &selected {
        a.refresh();
        let expected = a.head(collection);
        let (tx, rx) = std::sync::mpsc::channel();
        conn1
            .reducers
            .delete_file_collection_then(collection.clone(), expected, move |_, result| {
                let _ = tx.send(result);
            })
            .unwrap();
        rx.recv_timeout(Duration::from_secs(15))
            .unwrap()
            .unwrap()
            .unwrap();
    }
    wait(|| {
        a.refresh();
        selected.iter().all(|collection| {
            a.head(collection).is_empty()
                && a.head(&super::bootstrap::destination_collection(collection))
                    .is_empty()
                && !a
                    .remote
                    .as_ref()
                    .unwrap()
                    .revisions
                    .iter()
                    .any(|r| r.collection == *collection)
        })
    });
    eprintln!("Removed temporary test collections and their destination metadata");
    for (conn, device) in [
        (conn1, a.device.clone()),
        (conn2, b.device.clone()),
        (conn3, c.device.clone()),
    ] {
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
