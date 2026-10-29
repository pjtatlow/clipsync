use super::*;

impl Engine {
    /// Discover remote configs independently of existing local registrations.
    /// Automatic enrollment uses the same validation, backups and journal as bootstrap.
    pub(super) fn discover_collections(&mut self, home: &Path, only: &[String]) {
        let Some(remote) = &self.remote else {
            self.discovery.clear();
            self.discovery_snapshots.clear();
            return;
        };
        let heads: Vec<_> = remote
            .heads
            .iter()
            .filter(|h| {
                !h.collection.starts_with(DESTINATION_PREFIX)
                    && !self.registry.collections.contains_key(&h.collection)
                    && !self.registry.ignored.contains(&h.collection)
                    && (only.is_empty() || only.contains(&h.collection))
            })
            .map(|h| (h.collection.clone(), h.revision.clone()))
            .collect();
        let names: BTreeSet<_> = heads.iter().map(|(name, _)| name.clone()).collect();
        self.discovery_snapshots
            .retain(|name, _| names.contains(name));
        let mut notices = BTreeMap::new();
        let mut intents: Vec<(String, PathBuf)> = Vec::new();
        let mut candidates = Vec::new();
        for (name, revision) in heads {
            let inspect = (|| -> Result<PathBuf> {
                let relative = self.destination(&name)?.and_then(|d| d.home_relative)
                    .with_context(|| format!("no saved destination; upgrade a source daemon or use files bootstrap --map {name}=PATH"))?;
                let path = absolute_path(&home.join(relative))?;
                self.validate_new_path(&path)?;
                intents.push((name.clone(), path.clone()));
                if self
                    .discovery_snapshots
                    .get(&name)
                    .is_none_or(|(cached, _)| cached != &revision)
                {
                    let snapshot = self.decode(&name, &revision)?;
                    self.discovery_snapshots
                        .insert(name.clone(), (revision.clone(), snapshot));
                }
                let snapshot = &self.discovery_snapshots[&name].1;
                let local = scan(&path, snapshot.directory)?;
                if !local.entries.is_empty() && local != *snapshot {
                    bail!(
                        "local files differ at {}; use files bootstrap --only {name} --replace to back up and replace them",
                        path.display()
                    );
                }
                validate_install_paths(&path, &local, snapshot)?;
                Ok(path)
            })();
            match inspect {
                Ok(path) => candidates.push((name, path)),
                Err(error) => {
                    notices.insert(name, format!("{error:#}"));
                }
            }
        }
        // Do not choose an arbitrary winner for overlapping unregistered roots,
        // even if one of them has conflicting local contents and cannot enroll.
        let mut overlaps = BTreeSet::new();
        for (index, (name, path)) in intents.iter().enumerate() {
            for (other, other_path) in &intents[index + 1..] {
                if paths_overlap(path, other_path) {
                    overlaps.insert(name.clone());
                    overlaps.insert(other.clone());
                    notices.insert(
                        name.clone(),
                        format!("destination overlaps collection {other}"),
                    );
                    notices.insert(
                        other.clone(),
                        format!("destination overlaps collection {name}"),
                    );
                }
            }
        }
        for (name, path) in candidates {
            if overlaps.contains(&name) {
                continue;
            }
            if !self.auto_add_collections {
                notices.insert(
                    name,
                    format!(
                        "available at {}; automatic enrollment is disabled",
                        path.display()
                    ),
                );
                continue;
            }
            match self.bootstrap(home, false, false, std::slice::from_ref(&name), &[]) {
                Ok(_) => tracing::info!(collection = name, "Automatically enrolled synced config"),
                Err(error) => {
                    notices.insert(name, format!("{error:#}"));
                }
            }
        }
        self.discovery_snapshots
            .retain(|name, _| !self.registry.collections.contains_key(name));
        self.discovery = notices;
    }
}
