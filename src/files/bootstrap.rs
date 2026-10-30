use super::*;

pub(super) fn destination_collection(name: &str) -> String {
    format!(
        "{DESTINATION_PREFIX}{:.32}",
        format!("{:x}", Sha256::digest(name.as_bytes()))
    )
}

fn destination_snapshot(name: &str, relative: &str) -> Result<Snapshot> {
    safe_relative(relative)?;
    Ok(Snapshot {
        directory: false,
        entries: BTreeMap::from([(
            "file".into(),
            Entry {
                data: serde_json::to_vec(&Destination {
                    version: 1,
                    collection: name.into(),
                    home_relative: Some(relative.into()),
                })?,
                executable: false,
            },
        )]),
    })
}

impl Engine {
    pub(super) fn validate_new_path(&self, path: &Path) -> Result<()> {
        reject_symlinks(path)?;
        let config = config::config_dir()?;
        if [&self.root, &config]
            .into_iter()
            .any(|state| paths_overlap(path, state))
        {
            bail!("Cannot manage ClipSync's own state");
        }
        if self
            .registry
            .collections
            .values()
            .any(|c| paths_overlap(path, &c.path))
        {
            bail!("Collection paths must not overlap: {}", path.display());
        }
        Ok(())
    }

    pub(super) fn destination(&self, name: &str) -> Result<Option<Destination>> {
        let metadata = destination_collection(name);
        let head = self.head(&metadata);
        if head.is_empty() {
            return Ok(None);
        }
        let snapshot = self.decode(&metadata, &head)?;
        if snapshot.directory || snapshot.entries.len() != 1 {
            bail!("Invalid destination metadata for {name}");
        }
        let entry = snapshot
            .entries
            .get("file")
            .context("Missing destination metadata")?;
        if entry.data.len() > 64 * 1024 || entry.executable {
            bail!("Invalid destination metadata for {name}");
        }
        let destination: Destination = serde_json::from_slice(&entry.data)?;
        if destination.version != 1 || destination.collection != name {
            bail!("Destination metadata does not match {name}");
        }
        if let Some(relative) = &destination.home_relative {
            safe_relative(relative)?;
        }
        Ok(Some(destination))
    }

    pub(super) fn publish_destinations(&mut self, home: &Path) -> Result<()> {
        // Separate immutable metadata streams avoid collisions when two machines add
        // different collections. The first advertised default wins; local path overrides
        // never overwrite the default used to initialize future machines.
        let names: Vec<_> = self.registry.collections.keys().cloned().collect();
        let before = self.registry.destinations.len();
        self.registry
            .destinations
            .retain(|name, _| self.registry.collections.contains_key(name));
        if before != self.registry.destinations.len() {
            self.save()?;
        }
        for name in names {
            if self.destination(&name)?.is_some() {
                if self.registry.destinations.remove(&name).is_some() {
                    self.save()?;
                }
                continue;
            }
            let path = &self.registry.collections[&name].path;
            let relative_path = path
                .strip_prefix(home)
                .ok()
                .map(|path| path.components().collect::<PathBuf>());
            let Some(relative) = relative_path
                .as_ref()
                .and_then(|p| p.to_str())
                .filter(|s| !s.is_empty())
            else {
                if self.registry.destinations.remove(&name).is_some() {
                    self.save()?;
                }
                continue;
            };
            let snapshot = destination_snapshot(&name, relative)?;
            if self
                .registry
                .destinations
                .get(&name)
                .is_none_or(|pending| pending.snapshot != snapshot)
            {
                let id = uuid::Uuid::new_v4().to_string();
                let encrypted = seal(
                    &self.identity,
                    Envelope {
                        version: 2,
                        sequence: Some(1),
                        collection: destination_collection(&name),
                        id: id.clone(),
                        parent: String::new(),
                        snapshot: snapshot.clone(),
                    },
                )?;
                self.registry.destinations.insert(
                    name.clone(),
                    Pending {
                        sequence: 1,
                        id,
                        parent: String::new(),
                        snapshot,
                        encrypted,
                    },
                );
                if let Err(e) = self.save() {
                    self.registry.destinations.remove(&name);
                    return Err(e);
                }
            }
            if self.remote.is_some() {
                let pending = &self.registry.destinations[&name];
                self.tx.send(SpacetimeCommand::PublishFile {
                    collection: destination_collection(&name),
                    expected: String::new(),
                    revision: pending.id.clone(),
                    device_id: self.device.clone(),
                    encrypted_data: pending.encrypted.clone(),
                    sequence: pending.sequence,
                })?;
            }
        }
        Ok(())
    }

    pub(super) fn bootstrap(
        &mut self,
        home: &Path,
        dry_run: bool,
        replace: bool,
        only: &[String],
        mappings: &[String],
    ) -> Result<String> {
        let remote = self
            .remote
            .as_ref()
            .context("Connect to the sync server before bootstrapping")?;
        let heads: BTreeMap<_, _> = remote
            .heads
            .iter()
            .filter(|h| !h.collection.starts_with(DESTINATION_PREFIX))
            .map(|h| (h.collection.clone(), h.revision.clone()))
            .collect();
        let mut overrides = BTreeMap::new();
        for mapping in mappings {
            let (name, path) = mapping.split_once('=').context("Use --map NAME=PATH")?;
            if name.is_empty()
                || path.is_empty()
                || overrides
                    .insert(name.to_string(), absolute_path(Path::new(path))?)
                    .is_some()
            {
                bail!("Invalid or duplicate destination override: {mapping}");
            }
            if !heads.contains_key(name) {
                bail!("Unknown remote collection: {name}");
            }
            if !only.is_empty() && !only.iter().any(|selected| selected == name) {
                bail!("Override {name} is excluded by --only");
            }
        }
        for name in only {
            if !heads.contains_key(name) {
                bail!("Unknown remote collection: {name}");
            }
        }
        struct Plan {
            name: String,
            path: PathBuf,
            revision: String,
            snapshot: Snapshot,
            local: Snapshot,
            registered: bool,
        }
        let mut plan: Vec<Plan> = Vec::new();
        let mut output = String::new();
        // Preflight all destinations, authenticated snapshots and path overlaps before
        // registering anything. Existing registrations retain their local mappings.
        for (name, revision) in heads {
            if !only.is_empty() && !only.contains(&name) {
                continue;
            }
            if self.registry.ignored.contains(&name)
                && only.is_empty()
                && !overrides.contains_key(&name)
            {
                output.push_str(&format!(
                    "skip {name}: locally excluded; use --only {name} to re-enable\n"
                ));
                continue;
            }
            let registered = self.registry.collections.get(&name);
            let path = if let Some(path) = overrides.remove(&name) {
                if registered.is_some_and(|c| c.path != path) {
                    bail!(
                        "{name} is already registered at another path; remove it before remapping"
                    );
                }
                path
            } else if let Some(c) = registered {
                c.path.clone()
            } else if let Some(relative) = self.destination(&name)?.and_then(|d| d.home_relative) {
                home.join(relative)
            } else {
                output.push_str(&format!("skip {name}: no saved destination; upgrade an existing source daemon or use --map {name}=PATH\n"));
                continue;
            };
            let path = absolute_path(&path)?;
            if registered.is_some() && !replace {
                output.push_str(&format!(
                    "already registered {name} -> {}\n",
                    path.display()
                ));
                continue;
            }
            if registered.is_none() {
                self.validate_new_path(&path)?;
            }
            if plan.iter().any(|other| paths_overlap(&path, &other.path)) {
                bail!("Bootstrap destinations overlap at {}", path.display());
            }
            self.verify_descendant(
                &name,
                &revision,
                registered.map(|c| c.base.as_str()).unwrap_or(""),
            )?;
            let snapshot = self.decode(&name, &revision)?;
            if let Some(existing) = registered
                && (!existing.base.is_empty() || !existing.installed.entries.is_empty())
                && existing.directory != snapshot.directory
            {
                bail!("Collection type differs between devices for {name}");
            }
            let local = scan(&path, snapshot.directory)?;
            validate_shell(&snapshot, &path)?;
            validate_install_paths(&path, &local, &snapshot)?;
            let conflict = !replace && !local.entries.is_empty() && local != snapshot;
            output.push_str(&format!(
                "{} {name} -> {}\n",
                if conflict {
                    "preserve local / conflict"
                } else if local == snapshot {
                    "adopt"
                } else {
                    "install"
                },
                path.display()
            ));
            plan.push(Plan {
                name,
                path,
                revision,
                snapshot,
                local,
                registered: registered.is_some(),
            });
        }
        if dry_run {
            output.push_str("Preview only; no registrations or files changed.\n");
            return Ok(output);
        }
        let mut installed = 0;
        let mut conflicts = 0;
        for item in plan {
            if scan(&item.path, item.snapshot.directory)? != item.local {
                bail!(
                    "Local files changed during bootstrap at {}; retry to review the new state",
                    item.path.display()
                );
            }
            if !item.registered {
                self.command(FileCommand::Add {
                    name: item.name.clone(),
                    path: item.path.clone(),
                    directory: item.snapshot.directory,
                })?;
            }
            if !replace && !item.local.entries.is_empty() && item.local != item.snapshot {
                self.record(
                    &item.name,
                    &item.snapshot,
                    &format!("preserve bootstrap remote {}", item.revision),
                )?;
                self.registry
                    .collections
                    .get_mut(&item.name)
                    .unwrap()
                    .conflict = Some(item.revision);
                self.save()?;
                conflicts += 1;
            } else {
                if scan(&item.path, item.snapshot.directory)? != item.local {
                    bail!(
                        "Local files changed during bootstrap at {}",
                        item.path.display()
                    );
                }
                if item.registered {
                    self.reconcile_collection_type(&item.name)?;
                }
                self.apply(&item.name, &item.revision, item.snapshot)?;
                installed += 1;
            }
        }
        output.push_str(&format!(
            "Installed/adopted {installed}; preserved {conflicts} local conflicts.\n"
        ));
        if conflicts > 0 {
            output.push_str("Resolve individual conflicts with files resolve NAME remote, or rerun bootstrap --replace to back up and replace selected configs.\n");
        }
        Ok(output)
    }
}
