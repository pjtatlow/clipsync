//! Durable, snapshot-based configuration sync. Only this actor writes its registry/Git repos.
use crate::{config, crypto, daemon::spacetime::SpacetimeCommand};
use age::secrecy::ExposeSecret;
use anyhow::{Context, Result, bail};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

mod bootstrap;

const DESTINATION_PREFIX: &str = "__clipsync_destination_";

const MAX_BYTES: usize = 4 * 1024 * 1024;
const MAX_FILES: usize = 2048;

#[derive(Debug, Clone, Serialize, Deserialize, clap::Subcommand)]
pub enum FileCommand {
    /// Install synced collections on this computer using their saved destinations.
    Bootstrap {
        /// Preview destinations and actions without registering or installing anything.
        #[arg(long)]
        dry_run: bool,
        /// Replace differing local configs after saving encrypted Git backups.
        #[arg(long)]
        replace: bool,
        /// Limit setup to a collection (repeat for multiple collections).
        #[arg(long, value_name = "NAME")]
        only: Vec<String>,
        /// Override a local destination (repeat for multiple collections).
        #[arg(long = "map", value_name = "NAME=PATH")]
        mappings: Vec<String>,
    },
    /// Register a file or directory. Existing remote content is never silently adopted.
    Add {
        name: String,
        path: PathBuf,
        /// Treat a missing path as a directory immediately instead of waiting to infer its type.
        #[arg(long)]
        directory: bool,
    },
    /// Stop managing a collection, keeping destination files and Git backups.
    Remove { name: String },
    /// Show local progress and each device's last installed revision.
    Status,
    /// Show the collection's Git audit history.
    History { name: String },
    /// Show changes since the last captured snapshot.
    Diff { name: String },
    /// Resolve a divergence, preserving both versions in Git.
    Resolve {
        name: String,
        #[arg(value_enum)]
        choice: Resolution,
    },
    /// Publish a historical Git snapshot as a new revision.
    Rollback { name: String, commit: String },
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, clap::ValueEnum)]
pub enum Resolution {
    Local,
    Remote,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Entry {
    data: Vec<u8>,
    executable: bool,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
struct Snapshot {
    directory: bool,
    entries: BTreeMap<String, Entry>,
}
impl Snapshot {
    fn validate(&self) -> Result<()> {
        if self.entries.len() > MAX_FILES {
            bail!("Too many managed files (max {MAX_FILES})");
        }
        let mut size = 0usize;
        let mut portable_paths = std::collections::BTreeMap::new();
        for (path, entry) in &self.entries {
            safe_relative(path)?;
            let folded = path.to_lowercase();
            if portable_paths.insert(folded, path).is_some() {
                bail!("Case-colliding paths cannot sync portably");
            }
            if !self.directory && path != "file" {
                bail!("Invalid single-file snapshot");
            }
            size = size
                .checked_add(entry.data.len())
                .context("Snapshot size overflow")?;
            // A file cannot also be an ancestor of another file.
            let mut parent = Path::new(path).parent();
            while let Some(p) = parent {
                if let Some(p) = p.to_str()
                    && self.entries.contains_key(p)
                {
                    bail!("Conflicting file paths");
                }
                parent = p.parent();
            }
        }
        if size > MAX_BYTES {
            bail!("Collection exceeds {MAX_BYTES} bytes");
        }
        Ok(())
    }
    fn hash(&self) -> Result<String> {
        Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(self)?)))
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Envelope {
    version: u32,
    collection: String,
    id: String,
    parent: String,
    snapshot: Snapshot,
}
#[derive(Serialize, Deserialize)]
struct SignedEnvelope {
    envelope: Envelope,
    mac: Vec<u8>,
}

// age authenticates ciphertext integrity but does not authenticate its sender:
// anyone with the public recipient key can encrypt. File revisions additionally
// need proof of the account secret before they can install executable config.
fn envelope_mac(identity: &age::x25519::Identity, envelope: &Envelope) -> Result<Hmac<Sha256>> {
    let secret = identity.to_string();
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.expose_secret().as_bytes())?;
    mac.update(b"clipsync:file-revision:v1\0");
    mac.update(&serde_json::to_vec(envelope)?);
    Ok(mac)
}
fn seal(identity: &age::x25519::Identity, envelope: Envelope) -> Result<Vec<u8>> {
    let mac = envelope_mac(identity, &envelope)?
        .finalize()
        .into_bytes()
        .to_vec();
    crypto::encrypt(
        &serde_json::to_vec(&SignedEnvelope { envelope, mac })?,
        &[identity.to_public()],
    )
}
fn open(identity: &age::x25519::Identity, encrypted: &[u8]) -> Result<Envelope> {
    let plaintext = crypto::decrypt_limited(encrypted, identity, 24 * 1024 * 1024)?;
    let signed: SignedEnvelope = serde_json::from_slice(&plaintext)?;
    envelope_mac(identity, &signed.envelope)?
        .verify_slice(&signed.mac)
        .map_err(|_| anyhow::anyhow!("File revision sender authentication failed"))?;
    Ok(signed.envelope)
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Pending {
    id: String,
    parent: String,
    snapshot: Snapshot,
    encrypted: Vec<u8>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Applying {
    revision: String,
    snapshot: Snapshot,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Collection {
    path: PathBuf,
    directory: bool,
    #[serde(default)]
    base: String,
    #[serde(default)]
    installed: Snapshot,
    #[serde(default)]
    pending: Option<Pending>,
    #[serde(default)]
    applying: Option<Applying>,
    #[serde(default)]
    conflict: Option<String>,
    #[serde(default)]
    error: Option<String>,
}
#[derive(Default, Serialize, Deserialize)]
struct Registry {
    collections: BTreeMap<String, Collection>,
    #[serde(default)]
    destinations: BTreeMap<String, Pending>,
}

#[derive(Serialize, Deserialize)]
struct Destination {
    version: u32,
    collection: String,
    home_relative: Option<String>,
}

pub struct RemoteState {
    pub heads: Vec<crate::module_bindings::FileHead>,
    pub revisions: std::sync::Arc<Vec<crate::module_bindings::FileRevision>>,
    pub applied: Vec<crate::module_bindings::FileApplied>,
}
pub struct FileRequest {
    pub command: FileCommand,
    pub reply: tokio::sync::oneshot::Sender<Result<String, String>>,
}
struct Engine {
    root: PathBuf,
    registry: Registry,
    device: String,
    identity: age::x25519::Identity,
    tx: crossbeam_channel::Sender<SpacetimeCommand>,
    remote: Option<RemoteState>,
    observed: BTreeMap<String, (String, Instant)>,
}

pub fn spawn(
    device: String,
    tx: crossbeam_channel::Sender<SpacetimeCommand>,
) -> Result<std::sync::mpsc::Sender<FileRequest>> {
    let root = config::ensure_config_dir()?.join("files");
    private_dir(&root)?;
    let registry = if root.join("registry.json").exists() {
        serde_json::from_slice(&fs::read(root.join("registry.json"))?)
            .context("Invalid file-sync registry; refusing to reset it")?
    } else {
        Registry::default()
    };
    let identity = crypto::load_private_key()?;
    let (requests, rx) = std::sync::mpsc::channel::<FileRequest>();
    let mut engine = Engine {
        root,
        registry,
        device,
        identity,
        tx,
        remote: None,
        observed: BTreeMap::new(),
    };
    std::thread::Builder::new()
        .name("file-sync".into())
        .spawn(move || {
            let mut last_tick = Instant::now() - Duration::from_secs(1);
            let mut destination_error = None;
            loop {
                match rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(req) => {
                        let result = engine.command(req.command).map_err(|e| format!("{e:#}"));
                        let _ = req.reply.send(result);
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                }
                if last_tick.elapsed() >= Duration::from_millis(300) {
                    engine.refresh();
                    if let Some(home) = dirs::home_dir() {
                        let error = engine
                            .publish_destinations(&home)
                            .err()
                            .map(|e| format!("{e:#}"));
                        if error != destination_error {
                            if let Some(message) = &error {
                                tracing::warn!("Destination sync: {message}");
                            }
                            destination_error = error;
                        }
                    }
                    let names: Vec<_> = engine.registry.collections.keys().cloned().collect();
                    for name in names {
                        if let Err(e) = engine.tick(&name) {
                            let message = format!("{e:#}");
                            let c = engine.registry.collections.get_mut(&name).unwrap();
                            if c.error.as_ref() != Some(&message) {
                                tracing::warn!(collection = name, "File sync: {message}");
                                c.error = Some(message);
                                let _ = engine.save();
                            }
                        }
                    }
                    last_tick = Instant::now();
                }
            }
        })?;
    Ok(requests)
}

impl Engine {
    fn save(&self) -> Result<()> {
        atomic_write(
            &self.root.join("registry.json"),
            &serde_json::to_vec_pretty(&self.registry)?,
            false,
        )
    }
    fn repo(&self, name: &str) -> PathBuf {
        self.root.join("history").join(name)
    }
    fn git_bytes(&self, name: &str, args: &[&str]) -> Result<Vec<u8>> {
        let output = Command::new("git")
            .arg("-C")
            .arg(self.repo(name))
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
                "-c",
                "core.autocrlf=false",
                "-c",
                "core.fsync=committed",
            ])
            .args(args)
            .output()
            .context("Git is required for file sync")?;
        if !output.status.success() {
            bail!("Git: {}", String::from_utf8_lossy(&output.stderr));
        }
        Ok(output.stdout)
    }
    fn git(&self, name: &str, args: &[&str]) -> Result<String> {
        Ok(String::from_utf8(self.git_bytes(name, args)?)?)
    }
    fn historical_snapshot(&self, name: &str, commit: &str) -> Result<Snapshot> {
        let encrypted = self.git_bytes(name, &["show", &format!("{commit}:snapshot.age")])?;
        let env = open(&self.identity, &encrypted)?;
        if env.version != 1 || env.collection != name {
            bail!("Invalid encrypted history metadata");
        }
        env.snapshot.validate()?;
        Ok(env.snapshot)
    }
    fn record(&self, name: &str, snapshot: &Snapshot, message: &str) -> Result<()> {
        let repo = self.repo(name);
        private_dir(&repo)?;
        if !repo.join(".git").exists() {
            self.git(name, &["init", "--quiet"])?;
        }
        snapshot.validate()?;
        let encrypted = seal(
            &self.identity,
            Envelope {
                version: 1,
                collection: name.into(),
                id: uuid::Uuid::new_v4().to_string(),
                parent: String::new(),
                snapshot: snapshot.clone(),
            },
        )?;
        // Only ciphertext enters Git's object database; no plaintext temporary files,
        // checkout filters, hooks, file names, file modes or recovery metadata.
        let mut stream = Vec::new();
        writeln!(stream, "commit refs/heads/clipsync")?;
        writeln!(stream, "committer ClipSync <clipsync@localhost> now")?;
        writeln!(stream, "data {}", message.len())?;
        writeln!(stream, "{message}")?;
        if let Ok(parent) = self.git(name, &["rev-parse", "--verify", "refs/heads/clipsync"]) {
            writeln!(stream, "from {}", parent.trim())?;
        }
        writeln!(stream, "deleteall")?;
        fn blob(stream: &mut Vec<u8>, path: &str, bytes: &[u8], executable: bool) -> Result<()> {
            writeln!(
                stream,
                "M {} inline {}",
                if executable { "100755" } else { "100644" },
                serde_json::to_string(path)?
            )?;
            writeln!(stream, "data {}", bytes.len())?;
            stream.extend_from_slice(bytes);
            stream.push(b'\n');
            Ok(())
        }
        blob(&mut stream, "snapshot.age", &encrypted, false)?;
        stream.extend_from_slice(b"\ndone\n");
        use std::process::Stdio;
        let mut child = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args([
                "-c",
                "core.fsync=committed",
                "fast-import",
                "--quiet",
                "--date-format=now",
                "--done",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()?;
        child.stdin.take().unwrap().write_all(&stream)?;
        let output = child.wait_with_output()?;
        if !output.status.success() {
            bail!("Git import: {}", String::from_utf8_lossy(&output.stderr));
        }
        self.git(name, &["symbolic-ref", "HEAD", "refs/heads/clipsync"])?;
        Ok(())
    }
    fn refresh(&mut self) {
        let (reply, rx) = std::sync::mpsc::channel();
        self.remote = if self
            .tx
            .send(SpacetimeCommand::GetFileState { reply })
            .is_ok()
        {
            rx.recv_timeout(Duration::from_secs(1)).ok().flatten()
        } else {
            None
        };
    }
    fn head(&self, name: &str) -> String {
        self.remote
            .as_ref()
            .and_then(|r| r.heads.iter().find(|h| h.collection == name))
            .map(|h| h.revision.clone())
            .unwrap_or_default()
    }
    fn decode(&self, name: &str, id: &str) -> Result<Snapshot> {
        let row = self
            .remote
            .as_ref()
            .and_then(|r| r.revisions.iter().find(|r| r.id == id))
            .context("Revision has not arrived yet")?;
        let env = open(&self.identity, &row.encrypted_data)?;
        if env.version != 1
            || env.collection != name
            || env.id != id
            || env.parent != row.parent
            || row.collection != name
        {
            bail!("Revision metadata mismatch");
        }
        env.snapshot.validate()?;
        Ok(env.snapshot)
    }
    fn verify_descendant(&self, name: &str, head: &str, base: &str) -> Result<()> {
        let mut current = head.to_string();
        let mut visited = std::collections::BTreeSet::new();
        while current != base {
            if current.is_empty() || !visited.insert(current.clone()) {
                bail!("Server head is not descended from the installed revision");
            }
            // Authenticate every link: parent metadata on the server alone is untrusted.
            self.decode(name, &current)?;
            let row = self
                .remote
                .as_ref()
                .unwrap()
                .revisions
                .iter()
                .find(|r| r.id == current)
                .context("Missing revision ancestry")?;
            current = row.parent.clone();
        }
        Ok(())
    }
    fn queue(&mut self, name: &str, snapshot: Snapshot) -> Result<()> {
        snapshot.validate()?;
        validate_shell(&snapshot, &self.registry.collections[name].path)?;
        let id = uuid::Uuid::new_v4().to_string();
        let parent = self.registry.collections[name].base.clone();
        let env = Envelope {
            version: 1,
            collection: name.into(),
            id: id.clone(),
            parent: parent.clone(),
            snapshot: snapshot.clone(),
        };
        let encrypted = seal(&self.identity, env)?;
        if encrypted.len() > 8 * 1024 * 1024 {
            bail!("Encrypted revision too large");
        }
        self.record(
            name,
            &snapshot,
            &format!("capture {id} parent {parent} device {}", self.device),
        )?;
        let previous = self.registry.collections[name].clone();
        let c = self.registry.collections.get_mut(name).unwrap();
        c.pending = Some(Pending {
            id,
            parent,
            snapshot,
            encrypted,
        });
        c.error = None;
        if let Err(error) = self.save() {
            self.registry.collections.insert(name.into(), previous);
            return Err(error);
        }
        Ok(())
    }
    fn ack(&self, name: &str) {
        let c = &self.registry.collections[name];
        if c.base.is_empty() {
            return;
        }
        let already = self.remote.as_ref().is_some_and(|r| {
            r.applied
                .iter()
                .any(|a| a.collection == name && a.device_id == self.device && a.revision == c.base)
        });
        if !already {
            let _ = self.tx.send(SpacetimeCommand::AcknowledgeFile {
                collection: name.into(),
                device_id: self.device.clone(),
                revision: c.base.clone(),
            });
        }
    }
    /// An empty, unpublished default-file registration has no established type.
    /// Infer it from authenticated remote content or the first local path that appears.
    fn reconcile_collection_type(&mut self, name: &str) -> Result<bool> {
        let c = &self.registry.collections[name];
        if c.directory
            || !c.base.is_empty()
            || !c.installed.entries.is_empty()
            || c.pending
                .as_ref()
                .is_some_and(|p| !p.snapshot.entries.is_empty())
            || c.applying.is_some()
        {
            return Ok(true);
        }
        reject_symlinks(&c.path)?;
        let head = self.head(name);
        let directory = if !head.is_empty() {
            let incoming = self.decode(name, &head)?;
            if (incoming.directory && c.path.is_file()) || (!incoming.directory && c.path.is_dir())
            {
                bail!(
                    "Local path type differs from the remote collection at {}",
                    c.path.display()
                );
            }
            incoming.directory
        } else if c.path.is_dir() {
            true
        } else if c.path.try_exists()? {
            false
        } else {
            // Do not invent and publish an empty file while the type is unknown.
            return Ok(false);
        };
        if directory {
            let previous = c.clone();
            let c = self.registry.collections.get_mut(name).unwrap();
            c.directory = true;
            c.installed.directory = true;
            // An empty pending snapshot from an unresolved registration contains no
            // edits to preserve; capture the correctly typed snapshot instead.
            c.pending = None;
            c.conflict = None;
            c.error = None;
            if let Err(error) = self.save() {
                self.registry.collections.insert(name.into(), previous);
                return Err(error);
            }
            self.observed.remove(name);
        }
        Ok(true)
    }
    fn tick(&mut self, name: &str) -> Result<()> {
        if self.registry.collections[name].applying.is_some() {
            // A crash can interrupt a multi-file deployment. Keep the journal and require
            // explicit recovery so post-crash user edits are never overwritten automatically.
            bail!(
                "Interrupted application; preserved journal. Use files resolve {name} remote or local"
            );
        }
        if !self.reconcile_collection_type(name)? {
            let c = self.registry.collections.get_mut(name).unwrap();
            if c.error.take().is_some() {
                self.save()?;
            }
            return Ok(());
        }
        let c = self.registry.collections[name].clone();
        let local = scan(&c.path, c.directory)?;
        let head = self.head(name);
        let online = self.remote.is_some();
        if online {
            if let Some(p) = &c.pending {
                // Acknowledgement can be lost while the head advances again. Search immutable history.
                let accepted = self.remote.as_ref().unwrap().revisions.iter().any(|r| {
                    r.id == p.id
                        && r.collection == name
                        && r.parent == p.parent
                        && r.device_id == self.device
                        && r.encrypted_data == p.encrypted
                });
                if accepted {
                    let c = self.registry.collections.get_mut(name).unwrap();
                    c.base = p.id.clone();
                    c.installed = p.snapshot.clone();
                    c.pending = None;
                    c.error = None;
                    self.save()?;
                    return Ok(());
                }
            }
            if head != c.base {
                self.verify_descendant(name, &head, &c.base)?;
                if c.conflict.is_some() {
                    return Ok(());
                }
                let incoming = self.decode(name, &head)?;
                if c.pending.is_none()
                    && local == incoming
                    && (!incoming.directory || c.path.is_dir())
                {
                    // Joining with exactly the same bytes needs no deployment or conflict.
                    self.record(name, &incoming, &format!("adopt matching {head}"))?;
                    let c = self.registry.collections.get_mut(name).unwrap();
                    c.base = head;
                    c.installed = incoming;
                    c.error = None;
                    self.save()?;
                    self.ack(name);
                    return Ok(());
                }
                if c.pending.is_some() || local != c.installed {
                    self.record(name, &local, "preserve local divergence")?;
                    self.record(name, &incoming, &format!("preserve remote {head}"))?;
                    self.registry.collections.get_mut(name).unwrap().conflict = Some(head);
                    self.save()?;
                    return Ok(());
                }
                self.apply(name, &head, incoming)?;
                return Ok(());
            }
        }
        if c.conflict.is_some() {
            return Ok(());
        }
        if let Some(p) = &c.pending {
            if online {
                self.tx.send(SpacetimeCommand::PublishFile {
                    collection: name.into(),
                    expected: p.parent.clone(),
                    revision: p.id.clone(),
                    device_id: self.device.clone(),
                    encrypted_data: p.encrypted.clone(),
                })?;
            }
            // Further edits stay on disk; capture immediately after this revision is acknowledged.
            return Ok(());
        }
        if local != c.installed || c.base.is_empty() {
            let hash = local.hash()?;
            let observation = self
                .observed
                .entry(name.into())
                .or_insert((hash.clone(), Instant::now()));
            if observation.0 != hash {
                *observation = (hash, Instant::now());
            }
            if observation.1.elapsed() >= Duration::from_millis(300) {
                self.queue(name, local)?;
            }
        } else {
            self.ack(name);
        }
        if self.registry.collections[name].error.is_some() {
            self.registry.collections.get_mut(name).unwrap().error = None;
            self.save()?;
        }
        Ok(())
    }
    fn apply(&mut self, name: &str, revision: &str, snapshot: Snapshot) -> Result<()> {
        let c = self.registry.collections[name].clone();
        if snapshot.directory != c.directory {
            bail!("Collection type differs between devices");
        }
        snapshot.validate()?;
        let before = scan(&c.path, c.directory)?;
        validate_shell(&snapshot, &c.path)?;
        self.record(name, &before, "backup before application")?;
        self.record(name, &snapshot, &format!("receive {revision}"))?;
        self.registry.collections.get_mut(name).unwrap().applying = Some(Applying {
            revision: revision.into(),
            snapshot: snapshot.clone(),
        });
        self.save()?; // Write-ahead journal before any destination mutations.
        install(&c.path, &before, &snapshot)?;
        let after = scan(&c.path, c.directory)?;
        if after != snapshot {
            bail!("Destination changed during application; recovery required");
        }
        let c = self.registry.collections.get_mut(name).unwrap();
        c.base = revision.into();
        c.installed = snapshot;
        c.applying = None;
        c.pending = None;
        c.conflict = None;
        c.error = None;
        self.save()?;
        self.ack(name);
        Ok(())
    }
    fn command(&mut self, command: FileCommand) -> Result<String> {
        match command {
            FileCommand::Bootstrap {
                dry_run,
                replace,
                only,
                mappings,
            } => {
                self.refresh();
                let home = dirs::home_dir().context("Could not determine home directory")?;
                self.bootstrap(&home, dry_run, replace, &only, &mappings)
            }
            FileCommand::Add {
                name,
                path,
                directory,
            } => {
                if name.starts_with(DESTINATION_PREFIX) {
                    bail!("This collection name is reserved for destination metadata");
                }
                if name.is_empty()
                    || name.len() > 64
                    || !name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                {
                    bail!("Use a collection name of 1–64 letters, digits, '-' or '_'");
                }
                if self.registry.collections.contains_key(&name) {
                    bail!("Collection already registered");
                }
                let path = absolute_path(&path)?;
                self.validate_new_path(&path)?;
                let directory = directory || path.is_dir();
                let snapshot = scan(&path, directory)?;
                self.record(&name, &snapshot, "initial local backup")?;
                // Empty baseline makes a joining device with existing differing data conflict safely.
                self.registry.collections.insert(
                    name.clone(),
                    Collection {
                        path,
                        directory,
                        base: String::new(),
                        installed: Snapshot {
                            directory,
                            ..Default::default()
                        },
                        pending: None,
                        applying: None,
                        conflict: None,
                        error: None,
                    },
                );
                if let Err(error) = self.save() {
                    self.registry.collections.remove(&name);
                    return Err(error);
                }
                Ok(format!(
                    "Registered {name}. Sync starts automatically; existing remote differences require resolution."
                ))
            }
            FileCommand::Remove { name } => {
                let previous = self
                    .registry
                    .collections
                    .remove(&name)
                    .context("Unknown collection")?;
                if let Err(error) = self.save() {
                    self.registry.collections.insert(name, previous);
                    return Err(error);
                }
                self.observed.remove(&name);
                Ok(format!(
                    "Stopped managing {name}. Destination files, Git backups and server history are retained."
                ))
            }
            FileCommand::Status => {
                let mut out = format!(
                    "Transport: {}\n",
                    if self.remote.is_some() {
                        "subscribed"
                    } else {
                        "offline (local changes are retained)"
                    }
                );
                if !self.registry.destinations.is_empty() {
                    out.push_str(&format!(
                        "Destination metadata awaiting sync: {}\n",
                        self.registry.destinations.len()
                    ));
                }
                for (name, c) in &self.registry.collections {
                    out.push_str(&format!(
                        "{name}: {}\n  applied: {}\n  path: {}\n",
                        if c.applying.is_some() {
                            "interrupted application"
                        } else if c.conflict.is_some() {
                            "CONFLICT"
                        } else if !c.directory
                            && c.base.is_empty()
                            && c.installed.entries.is_empty()
                            && !c.path.exists()
                            && self.head(name).is_empty()
                        {
                            "waiting for local path or remote collection"
                        } else if c.pending.is_some() {
                            "pending publication"
                        } else {
                            "watching"
                        },
                        c.base,
                        c.path.display()
                    ));
                    if let Some(e) = &c.error {
                        out.push_str(&format!("  error: {e}\n"));
                    }
                    if let Some(remote) = &self.remote {
                        out.push_str(&format!("  server head: {}\n", self.head(name)));
                        for a in remote.applied.iter().filter(|a| a.collection == *name) {
                            out.push_str(&format!(
                                "  device {} applied {}\n",
                                a.device_id, a.revision
                            ));
                        }
                    }
                }
                Ok(out)
            }
            FileCommand::History { name } => {
                self.collection(&name)?;
                self.git(&name, &["log", "--format=%h %ai %s", "-50"])
            }
            FileCommand::Diff { name } => {
                self.collection(&name)?;
                self.reconcile_collection_type(&name)?;
                let c = self.collection(&name)?;
                let local = scan(&c.path, c.directory)?;
                let baseline = c
                    .pending
                    .as_ref()
                    .map(|p| &p.snapshot)
                    .unwrap_or(&c.installed);
                let mut out = String::new();
                for path in baseline
                    .entries
                    .keys()
                    .chain(local.entries.keys())
                    .collect::<std::collections::BTreeSet<_>>()
                {
                    if baseline.entries.get(path) != local.entries.get(path) {
                        out.push_str(&format!(
                            "{} {path}\n",
                            if !baseline.entries.contains_key(path) {
                                "added"
                            } else if !local.entries.contains_key(path) {
                                "deleted"
                            } else {
                                "modified"
                            }
                        ));
                    }
                }
                if out.is_empty() {
                    out = "No uncaptured changes. See history for captured revisions.\n".into();
                }
                Ok(out)
            }
            FileCommand::Resolve { name, choice } => {
                self.collection(&name)?;
                self.refresh();
                if self.remote.is_none() {
                    bail!("Connect before resolving");
                }
                let head = self.head(&name);
                if head.is_empty() {
                    bail!("No remote revision to resolve against");
                }
                self.reconcile_collection_type(&name)?;
                let c = self.registry.collections[&name].clone();
                let local = scan(&c.path, c.directory)?;
                self.record(&name, &local, "backup before explicit resolution")?;
                match choice {
                    Resolution::Remote => {
                        let snapshot = self.decode(&name, &head)?;
                        self.apply(&name, &head, snapshot)?;
                    }
                    Resolution::Local => {
                        let remote = self.decode(&name, &head)?;
                        self.record(&name, &remote, &format!("preserve rejected remote {head}"))?;
                        let state = self.registry.collections.get_mut(&name).unwrap();
                        state.base = head;
                        state.pending = None;
                        state.conflict = None;
                        state.applying = None;
                        // installed remains the last known installed content until our publication is accepted.
                        if let Err(error) = self.queue(&name, local) {
                            self.registry.collections.insert(name, c);
                            return Err(error);
                        }
                    }
                }
                Ok("Resolution recorded; propagation continues automatically.".into())
            }
            FileCommand::Rollback { name, commit } => {
                let c = self.collection(&name)?.clone();
                if c.pending.is_some() || c.conflict.is_some() || c.applying.is_some() {
                    bail!("Resolve pending/conflicting work before rollback");
                }
                if commit.starts_with('-')
                    || !commit.bytes().all(|b| b.is_ascii_hexdigit())
                    || commit.len() < 4
                {
                    bail!("Use a Git commit hash from files history");
                }
                let snapshot = self.historical_snapshot(&name, &commit)?;
                snapshot.validate()?;
                if snapshot.directory != c.directory {
                    bail!("Snapshot type mismatch");
                }
                let before = scan(&c.path, c.directory)?;
                if before != c.installed {
                    bail!("Wait for local edits to sync before rollback");
                }
                validate_shell(&snapshot, &c.path)?;
                self.record(&name, &before, "backup before rollback")?;
                // A rollback is an ordinary new revision; apply with a journal without acknowledging it as the old head.
                self.registry.collections.get_mut(&name).unwrap().applying = Some(Applying {
                    revision: c.base.clone(),
                    snapshot: snapshot.clone(),
                });
                self.save()?;
                install(&c.path, &before, &snapshot)?;
                if scan(&c.path, c.directory)? != snapshot {
                    bail!("Destination changed during rollback");
                }
                self.registry.collections.get_mut(&name).unwrap().applying = None;
                self.queue(&name, snapshot)?;
                Ok("Rollback captured as a new pending revision.".into())
            }
        }
    }
    fn collection(&self, name: &str) -> Result<&Collection> {
        self.registry
            .collections
            .get(name)
            .context("Unknown collection")
    }
}

/// Resolve overrides in the CLI's working directory before sending them to the daemon.
pub fn normalize_mappings(mappings: Vec<String>) -> Result<Vec<String>> {
    mappings
        .into_iter()
        .map(|mapping| {
            let (name, value) = mapping.split_once('=').context("Use --map NAME=PATH")?;
            if name.is_empty() || value.is_empty() {
                bail!("Use --map NAME=PATH");
            }
            let path = if let Some(relative) = value.strip_prefix("~/") {
                dirs::home_dir()
                    .context("Could not determine home directory")?
                    .join(relative)
            } else {
                PathBuf::from(value)
            };
            let path = absolute_path(&path)?;
            Ok(format!(
                "{name}={}",
                path.to_str().context("Destination must be UTF-8")?
            ))
        })
        .collect()
}

fn safe_relative(path: &str) -> Result<()> {
    if path.is_empty()
        || path.contains('\\')
        || path.chars().any(char::is_control)
        || path
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == ".." || p.eq_ignore_ascii_case(".git"))
        || Path::new(path)
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        bail!("Unsafe managed path: {path:?}");
    }
    Ok(())
}
fn absolute_path(path: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    if path.components().any(|p| matches!(p, Component::ParentDir)) {
        bail!("Use a path without '..'");
    }
    Ok(path)
}
fn reject_symlinks(path: &Path) -> Result<()> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(m) if m.file_type().is_symlink() => {
                bail!("Symlinks are not managed: {}", ancestor.display())
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
fn private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
fn scan(root: &Path, directory: bool) -> Result<Snapshot> {
    reject_symlinks(root)?;
    let mut snapshot = Snapshot {
        directory,
        ..Default::default()
    };
    fn walk(root: &Path, current: &Path, snapshot: &mut Snapshot, bytes: &mut usize) -> Result<()> {
        let meta = match fs::symlink_metadata(current) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        if meta.file_type().is_symlink() {
            bail!("Symlinks are not managed: {}", current.display());
        }
        if meta.is_dir() {
            if !snapshot.directory {
                bail!("Expected a file at {}", current.display());
            }
            for entry in fs::read_dir(current)? {
                let entry = entry?;
                if entry.file_name() == ".git" {
                    continue;
                }
                walk(root, &entry.path(), snapshot, bytes)?;
            }
        } else if meta.is_file() {
            if snapshot.directory && current == root {
                bail!("Expected a directory at {}", root.display());
            }
            if meta.len() > MAX_BYTES as u64 || snapshot.entries.len() >= MAX_FILES {
                bail!("Collection size limit exceeded");
            }
            let key = if snapshot.directory {
                current
                    .strip_prefix(root)?
                    .to_str()
                    .context("Paths must be UTF-8")?
                    .to_string()
            } else {
                "file".into()
            };
            safe_relative(&key)?;
            // Bound reads even if a writer grows the file after metadata was read.
            use std::io::Read;
            let mut data = Vec::new();
            fs::File::open(current)?
                .take((MAX_BYTES + 1) as u64)
                .read_to_end(&mut data)?;
            *bytes += data.len();
            if *bytes > MAX_BYTES {
                bail!("Collection exceeds {MAX_BYTES} bytes");
            }
            #[cfg(unix)]
            let executable = {
                use std::os::unix::fs::PermissionsExt;
                meta.permissions().mode() & 0o111 != 0
            };
            #[cfg(not(unix))]
            let executable = false;
            snapshot.entries.insert(key, Entry { data, executable });
        } else {
            bail!("Only regular files are managed: {}", current.display());
        }
        Ok(())
    }
    walk(root, root, &mut snapshot, &mut 0)?;
    snapshot.validate()?;
    Ok(snapshot)
}
fn destination(root: &Path, directory: bool, relative: &str) -> PathBuf {
    if directory {
        root.join(relative)
    } else {
        root.into()
    }
}
fn atomic_write(path: &Path, data: &[u8], executable: bool) -> Result<()> {
    reject_symlinks(path)?;
    let parent = path.parent().context("Missing parent directory")?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".clipsync-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(if executable { 0o700 } else { 0o600 });
        }
        let mut file = options.open(&temp)?;
        file.write_all(data)?;
        file.sync_all()?;
        // Recheck after preparing the temporary file.
        reject_symlinks(path)?;
        fs::rename(&temp, path)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}
fn install(root: &Path, before: &Snapshot, after: &Snapshot) -> Result<()> {
    if before.directory != after.directory {
        bail!("Snapshot type mismatch");
    }
    after.validate()?;
    if scan(root, before.directory)? != *before {
        bail!("Local files changed before application");
    }
    // Preflight path/type collisions before writing anything. File/directory transitions
    // deliberately require manual handling in v1.
    for key in before.entries.keys().chain(after.entries.keys()) {
        let path = destination(root, after.directory, key);
        reject_symlinks(&path)?;
        if path.is_dir() {
            bail!("File/directory collision at {}", path.display());
        }
        for parent in path.parent().into_iter().flat_map(|p| p.ancestors()) {
            if parent.is_file() {
                bail!("File/directory collision at {}", parent.display());
            }
        }
    }
    if after.directory && !root.exists() {
        reject_symlinks(root)?;
        fs::create_dir_all(root)?;
        fs::File::open(root.parent().context("Missing parent directory")?)?.sync_all()?;
    }
    for (key, entry) in &after.entries {
        if before.entries.get(key) != Some(entry) {
            // Check this destination once more before replacing it.
            let path = destination(root, after.directory, key);
            if read_entry(&path)? != before.entries.get(key).cloned() {
                bail!("Concurrent edit at {}", path.display());
            }
            atomic_write(&path, &entry.data, entry.executable)?;
        }
    }
    for (key, entry) in &before.entries {
        if !after.entries.contains_key(key) {
            let path = destination(root, after.directory, key);
            reject_symlinks(&path)?;
            if read_entry(&path)? != Some(entry.clone()) {
                bail!("Concurrent edit at {}", path.display());
            }
            fs::remove_file(&path)?;
            fs::File::open(path.parent().unwrap())?.sync_all()?;
        }
    }
    Ok(())
}
fn read_entry(path: &Path) -> Result<Option<Entry>> {
    if !path.exists() {
        return Ok(None);
    }
    let s = scan(path, false)?;
    Ok(s.entries.get("file").cloned())
}
fn validate_shell(snapshot: &Snapshot, root: &Path) -> Result<()> {
    for (key, entry) in &snapshot.entries {
        let path = destination(root, snapshot.directory, key);
        let file = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if file == ".zshrc" || file.ends_with(".zsh") {
            use std::process::Stdio;
            let mut child = Command::new("zsh")
                .args(["-f", "-n"])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .context("zsh is required to validate zsh configuration")?;
            child.stdin.take().unwrap().write_all(&entry.data)?;
            let output = child.wait_with_output()?;
            if !output.status.success() {
                bail!(
                    "zsh validation failed for {key}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .canonicalize()
                .unwrap()
                .join(format!("clipsync-test-{}", uuid::Uuid::new_v4()));
            private_dir(&path).unwrap();
            Self(path)
        }
        fn engine(&self) -> Engine {
            let (tx, _) = crossbeam_channel::unbounded();
            let root = self.0.join("state");
            private_dir(&root).unwrap();
            Engine {
                root,
                registry: Registry::default(),
                device: "test-device".into(),
                identity: crypto::generate_keypair().0,
                tx,
                remote: None,
                observed: BTreeMap::new(),
            }
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn snapshot(text: &str) -> Snapshot {
        Snapshot {
            directory: false,
            entries: BTreeMap::from([(
                "file".into(),
                Entry {
                    data: text.as_bytes().to_vec(),
                    executable: false,
                },
            )]),
        }
    }
    fn add(e: &mut Engine, name: &str, path: PathBuf) {
        e.command(FileCommand::Add {
            name: name.into(),
            path,
            directory: false,
        })
        .unwrap();
    }
    fn remote(e: &Engine, name: &str, id: &str, parent: &str, snapshot: Snapshot) -> RemoteState {
        let encrypted_data = seal(
            &e.identity,
            Envelope {
                version: 1,
                collection: name.into(),
                id: id.into(),
                parent: parent.into(),
                snapshot,
            },
        )
        .unwrap();
        RemoteState {
            heads: vec![crate::module_bindings::FileHead {
                key: "1:test".into(),
                user_id: 1,
                collection: name.into(),
                revision: id.into(),
            }],
            revisions: std::sync::Arc::new(vec![crate::module_bindings::FileRevision {
                id: id.into(),
                user_id: 1,
                collection: name.into(),
                parent: parent.into(),
                device_id: "other".into(),
                encrypted_data,
                created_at: spacetimedb_sdk::Timestamp::from_micros_since_unix_epoch(0),
            }]),
            applied: vec![],
        }
    }
    #[test]
    fn unsafe_paths_and_collisions_are_rejected() {
        for p in [
            "../x",
            "/x",
            "a/../b",
            "a//b",
            ".git/config",
            "a\\b",
            "a/./b",
        ] {
            assert!(safe_relative(p).is_err(), "{p}");
        }
        let mut s = Snapshot {
            directory: true,
            ..Default::default()
        };
        s.entries.insert(
            "a".into(),
            Entry {
                data: vec![],
                executable: false,
            },
        );
        s.entries.insert(
            "a/b".into(),
            Entry {
                data: vec![],
                executable: false,
            },
        );
        assert!(s.validate().is_err());
    }
    #[test]
    fn installation_preserves_executable_bits_and_deletions() {
        let t = Temp::new();
        let path = t.0.join("bin");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("old"), "old").unwrap();
        let before = scan(&path, true).unwrap();
        let after = Snapshot {
            directory: true,
            entries: BTreeMap::from([(
                "nested/script".into(),
                Entry {
                    data: b"#!/bin/sh\necho hi\n".to_vec(),
                    executable: true,
                },
            )]),
        };
        install(&path, &before, &after).unwrap();
        assert_eq!(scan(&path, true).unwrap(), after);
        assert!(!path.join("old").exists());
    }
    #[test]
    fn concurrent_edits_are_not_overwritten() {
        let t = Temp::new();
        let path = t.0.join("config");
        fs::write(&path, "before").unwrap();
        let before = scan(&path, false).unwrap();
        fs::write(&path, "user edit").unwrap();
        assert!(install(&path, &before, &snapshot("remote")).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "user edit");
    }
    #[test]
    fn offline_pending_revision_survives_restart() {
        let t = Temp::new();
        let mut e = t.engine();
        let path = t.0.join("config");
        fs::write(&path, "local").unwrap();
        add(&mut e, "test", path);
        e.queue("test", snapshot("local")).unwrap();
        let saved: Registry =
            serde_json::from_slice(&fs::read(e.root.join("registry.json")).unwrap()).unwrap();
        let p = saved.collections["test"].pending.as_ref().unwrap();
        assert_eq!(p.snapshot, snapshot("local"));
        let env = open(&e.identity, &p.encrypted).unwrap();
        assert_eq!(env.id, p.id);
        assert!(
            e.git("test", &["log", "--oneline"])
                .unwrap()
                .contains(&p.id)
        );
    }
    #[test]
    fn joining_with_existing_files_preserves_both_versions() {
        let t = Temp::new();
        let mut e = t.engine();
        let path = t.0.join("config");
        fs::write(&path, "mine").unwrap();
        add(&mut e, "test", path.clone());
        e.remote = Some(remote(&e, "test", "remote-id", "", snapshot("theirs")));
        e.tick("test").unwrap();
        assert_eq!(
            e.registry.collections["test"].conflict.as_deref(),
            Some("remote-id")
        );
        assert_eq!(fs::read_to_string(path).unwrap(), "mine");
        let history = e.git("test", &["log", "--oneline"]).unwrap();
        assert!(history.contains("preserve remote"));
        assert!(history.contains("preserve local"));
    }
    #[test]
    fn clean_receiver_applies_and_records_revision() {
        let t = Temp::new();
        let mut e = t.engine();
        let path = t.0.join("config");
        add(&mut e, "test", path.clone());
        e.remote = Some(remote(&e, "test", "remote-id", "", snapshot("theirs")));
        e.tick("test").unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), "theirs");
        assert_eq!(e.registry.collections["test"].base, "remote-id");
        assert!(e.registry.collections["test"].applying.is_none());
    }
    #[test]
    fn lost_acknowledgement_does_not_create_false_conflict() {
        let t = Temp::new();
        let mut e = t.engine();
        let path = t.0.join("config");
        fs::write(&path, "mine").unwrap();
        add(&mut e, "test", path.clone());
        e.queue("test", snapshot("mine")).unwrap();
        let pending = e.registry.collections["test"].pending.clone().unwrap();
        let mut r = remote(&e, "test", "newer", &pending.id, snapshot("newer"));
        std::sync::Arc::make_mut(&mut r.revisions).push(crate::module_bindings::FileRevision {
            id: pending.id.clone(),
            user_id: 1,
            collection: "test".into(),
            parent: String::new(),
            device_id: e.device.clone(),
            encrypted_data: pending.encrypted,
            created_at: spacetimedb_sdk::Timestamp::from_micros_since_unix_epoch(0),
        });
        e.remote = Some(r);
        e.tick("test").unwrap();
        assert_eq!(e.registry.collections["test"].base, pending.id);
        assert!(e.registry.collections["test"].pending.is_none());
        e.tick("test").unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), "newer");
    }
    #[test]
    fn interrupted_application_requires_explicit_recovery() {
        let t = Temp::new();
        let mut e = t.engine();
        let path = t.0.join("config");
        add(&mut e, "test", path.clone());
        e.registry.collections.get_mut("test").unwrap().applying = Some(Applying {
            revision: "r".into(),
            snapshot: snapshot("remote"),
        });
        e.save().unwrap();
        fs::write(&path, "post-crash edit").unwrap();
        assert!(e.tick("test").is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "post-crash edit");
    }
    #[cfg(unix)]
    #[test]
    fn symlinks_cannot_escape_collection() {
        let t = Temp::new();
        let outside = t.0.join("outside");
        fs::write(&outside, "secret").unwrap();
        let root = t.0.join("root");
        fs::create_dir(&root).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        assert!(scan(&root, true).is_err());
        assert!(atomic_write(&root.join("link"), b"overwrite", false).is_err());
        assert_eq!(fs::read_to_string(outside).unwrap(), "secret");
    }
    #[test]
    fn shell_validation_blocks_invalid_remote_configuration() {
        let t = Temp::new();
        let root = t.0.join(".zshrc");
        assert!(validate_shell(&snapshot("if then\n"), &root).is_err());
        assert!(validate_shell(&snapshot("export EDITOR=nvim\n"), &root).is_ok());
    }
    #[test]
    fn decryption_expansion_is_bounded() {
        let (id, pubkey) = crypto::generate_keypair();
        let encrypted = crypto::encrypt(&vec![0; 20000], &[pubkey]).unwrap();
        assert!(crypto::decrypt_limited(&encrypted, &id, 1000).is_err());
    }
    #[test]
    fn public_key_encryption_alone_cannot_authorize_file_installation() {
        let (identity, recipient) = crypto::generate_keypair();
        let envelope = Envelope {
            version: 1,
            collection: "test".into(),
            id: "forged".into(),
            parent: String::new(),
            snapshot: snapshot("malicious"),
        };
        let forged = crypto::encrypt(
            &serde_json::to_vec(&SignedEnvelope {
                envelope,
                mac: vec![0; 32],
            })
            .unwrap(),
            &[recipient],
        )
        .unwrap();
        assert!(open(&identity, &forged).is_err());
    }
    #[test]
    fn replayed_older_head_cannot_roll_back_installed_files() {
        let t = Temp::new();
        let mut e = t.engine();
        let path = t.0.join("config");
        fs::write(&path, "current").unwrap();
        add(&mut e, "test", path.clone());
        let c = e.registry.collections.get_mut("test").unwrap();
        c.base = "newer".into();
        c.installed = snapshot("current");
        e.remote = Some(remote(&e, "test", "older", "", snapshot("old")));
        assert!(e.tick("test").is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "current");
    }
    #[test]
    fn matching_join_needs_no_overwrite_or_conflict() {
        let t = Temp::new();
        let mut e = t.engine();
        let path = t.0.join("config");
        fs::write(&path, "same").unwrap();
        add(&mut e, "test", path.clone());
        e.remote = Some(remote(&e, "test", "r", "", snapshot("same")));
        e.tick("test").unwrap();
        assert_eq!(e.registry.collections["test"].base, "r");
        assert!(e.registry.collections["test"].conflict.is_none());
        assert_eq!(fs::read_to_string(path).unwrap(), "same");
    }
    #[test]
    fn stable_offline_edit_is_automatically_captured() {
        let t = Temp::new();
        let mut e = t.engine();
        let path = t.0.join("config");
        fs::write(&path, "offline").unwrap();
        add(&mut e, "test", path);
        let hash = snapshot("offline").hash().unwrap();
        e.observed.insert(
            "test".into(),
            (hash, Instant::now() - Duration::from_secs(1)),
        );
        e.tick("test").unwrap();
        assert_eq!(
            e.registry.collections["test"]
                .pending
                .as_ref()
                .unwrap()
                .snapshot,
            snapshot("offline")
        );
    }
    #[test]
    fn failed_registry_write_cannot_publish_an_undurable_revision() {
        let t = Temp::new();
        let mut e = t.engine();
        let path = t.0.join("config");
        fs::write(&path, "local").unwrap();
        add(&mut e, "test", path);
        // A directory at the registry path makes atomic replacement fail even as root.
        fs::remove_file(e.root.join("registry.json")).unwrap();
        fs::create_dir(e.root.join("registry.json")).unwrap();
        assert!(e.queue("test", snapshot("local")).is_err());
        assert!(e.registry.collections["test"].pending.is_none());
    }
    #[test]
    fn git_contains_only_ciphertext_and_can_restore_original_bytes_and_modes() {
        let t = Temp::new();
        let e = t.engine();
        let secret = "a-unique-config-secret-that-must-not-appear-in-git";
        let original = Snapshot {
            directory: true,
            entries: BTreeMap::from([
                (
                    "private-script-name".into(),
                    Entry {
                        data: secret.as_bytes().to_vec(),
                        executable: true,
                    },
                ),
                (
                    "nested/config".into(),
                    Entry {
                        data: vec![0, 255, 10],
                        executable: false,
                    },
                ),
            ]),
        };
        e.record("test", &original, "encrypted capture").unwrap();
        assert_eq!(
            e.git("test", &["ls-tree", "-r", "--name-only", "HEAD"])
                .unwrap(),
            "snapshot.age\n"
        );
        let objects = e
            .git_bytes("test", &["cat-file", "--batch-all-objects", "--batch"])
            .unwrap();
        for plaintext in [
            secret.as_bytes(),
            b"private-script-name",
            b"nested/config",
            b"snapshot.json",
        ] {
            assert!(
                !objects
                    .windows(plaintext.len())
                    .any(|bytes| bytes == plaintext)
            );
        }
        assert_eq!(e.historical_snapshot("test", "HEAD").unwrap(), original);
        let encrypted = e.git_bytes("test", &["show", "HEAD:snapshot.age"]).unwrap();
        assert!(open(&crypto::generate_keypair().0, &encrypted).is_err());
    }
    #[test]
    fn missing_path_waits_instead_of_publishing_an_empty_file() {
        let t = Temp::new();
        let mut e = t.engine();
        add(&mut e, "test", t.0.join("missing"));
        e.observed.insert(
            "test".into(),
            (
                Snapshot::default().hash().unwrap(),
                Instant::now() - Duration::from_secs(1),
            ),
        );
        e.tick("test").unwrap();
        assert!(e.registry.collections["test"].pending.is_none());
        assert!(
            e.command(FileCommand::Status)
                .unwrap()
                .contains("waiting for local path")
        );
    }
    #[test]
    fn later_created_directory_is_inferred_and_captured_without_reregistering() {
        let t = Temp::new();
        let mut e = t.engine();
        let path = t.0.join("missing");
        add(&mut e, "test", path.clone());
        e.tick("test").unwrap();
        fs::create_dir(&path).unwrap();
        fs::write(path.join("config.lua"), "return {}").unwrap();
        e.registry.collections.get_mut("test").unwrap().error = Some("Expected a file".into());
        e.tick("test").unwrap();
        assert!(e.registry.collections["test"].directory);
        assert!(e.registry.collections["test"].error.is_none());
        let saved: Registry =
            serde_json::from_slice(&fs::read(e.root.join("registry.json")).unwrap()).unwrap();
        assert!(saved.collections["test"].directory);
        let snapshot = scan(&path, true).unwrap();
        e.observed.insert(
            "test".into(),
            (
                snapshot.hash().unwrap(),
                Instant::now() - Duration::from_secs(1),
            ),
        );
        e.tick("test").unwrap();
        assert_eq!(
            e.registry.collections["test"]
                .pending
                .as_ref()
                .unwrap()
                .snapshot,
            snapshot
        );
    }
    #[test]
    fn missing_destination_infers_remote_directory_and_recovers_empty_pending_state() {
        let t = Temp::new();
        let mut e = t.engine();
        let path = t.0.join("missing");
        add(&mut e, "test", path.clone());
        // Reproduce the old failure state: a missing path captured as an empty file.
        e.queue("test", Snapshot::default()).unwrap();
        e.registry.collections.get_mut("test").unwrap().error =
            Some("Collection type differs between devices".into());
        let incoming = Snapshot {
            directory: true,
            entries: BTreeMap::from([(
                "nested/init.lua".into(),
                Entry {
                    data: b"return {}".to_vec(),
                    executable: false,
                },
            )]),
        };
        e.remote = Some(remote(
            &e,
            "test",
            "directory-revision",
            "",
            incoming.clone(),
        ));
        e.tick("test").unwrap();
        assert_eq!(scan(&path, true).unwrap(), incoming);
        let c = &e.registry.collections["test"];
        assert!(c.directory && c.pending.is_none() && c.error.is_none());
        assert_eq!(c.base, "directory-revision");
    }
    #[test]
    fn empty_remote_directory_is_created_at_missing_destination() {
        let t = Temp::new();
        let mut e = t.engine();
        let path = t.0.join("missing");
        add(&mut e, "test", path.clone());
        e.remote = Some(remote(
            &e,
            "test",
            "empty-directory",
            "",
            Snapshot {
                directory: true,
                ..Default::default()
            },
        ));
        e.tick("test").unwrap();
        assert!(path.is_dir());
        assert_eq!(e.registry.collections["test"].base, "empty-directory");
    }
    #[test]
    fn established_file_type_is_not_changed_when_replaced_by_a_directory() {
        let t = Temp::new();
        let mut e = t.engine();
        let path = t.0.join("config");
        fs::write(&path, "").unwrap();
        add(&mut e, "test", path.clone());
        e.registry.collections.get_mut("test").unwrap().base = "published-file".into();
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(e.tick("test").is_err());
        assert!(!e.registry.collections["test"].directory);
    }
    #[test]
    fn existing_file_is_not_reinterpreted_as_a_remote_directory() {
        let t = Temp::new();
        let mut e = t.engine();
        let path = t.0.join("config");
        fs::write(&path, "").unwrap();
        add(&mut e, "test", path.clone());
        e.remote = Some(remote(
            &e,
            "test",
            "directory",
            "",
            Snapshot {
                directory: true,
                ..Default::default()
            },
        ));
        assert!(e.tick("test").is_err());
        assert!(path.is_file());
    }
    include!("files/cleanup_tests.rs");
    include!("files/bootstrap_tests.rs");
    include!("files_live_tests.rs");
}
