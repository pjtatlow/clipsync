# Live configuration sync

ClipSync can manage explicitly registered files and directories in addition to the clipboard.
Edit a managed file in place; the daemon snapshots it into Git and sends an encrypted,
account-authenticated revision to the other registered machines. Each receiving machine
installs the revision and reports its installed revision ID.

## Build and server upgrade

Install Git, Rust with the `wasm32-unknown-unknown` target, and the SpacetimeDB CLI 2.0.5.
The server and client SDK are pinned to that version. `cargo build` generates bindings;
changes to the server schema regenerate them automatically.

The server module must be upgraded before using file sync. Publish the server module to
your database using your normal deployment credentials:

```sh
spacetime publish --module-path server YOUR_DATABASE
```

Use a build of this client on every participating machine, run `clipsync setup` for the
same account on each, and restart the daemon. This is an additive schema update; do not
use `--delete-data` on an existing database. Publishing/deployment is separate from building.

## Register collections

Run these commands while the daemon is running:

```sh
clipsync files add zsh ~/.zshrc
clipsync files add neovim ~/.config/nvim
clipsync files add scripts ~/.local/bin
clipsync files add lazygit ~/.config/lazygit/config.yml
```

Use the **same collection name** on every machine, with the local destination appropriate
for that machine. Each collection maps to exactly one file or directory. Existing directories
are detected automatically. To register a directory that does not exist yet:

```sh
clipsync files add neovim ~/.config/nvim --directory
```

An empty destination receives the current remote revision automatically. A nonempty
joining destination with different content produces a conflict, preserving its contents.
To adopt the remote version explicitly:

```sh
clipsync files resolve neovim remote
```

`files add` captures an initial Git backup before sync starts. Register only the files
intended to be shared. Keep caches, generated files, machine credentials and plugin
installations outside these roots. Directory collections include all regular files
recursively except `.git` entries. There is currently no configurable ignore list.
Symlinks and special files are rejected, not followed. Collection roots must not overlap.

For machine-specific shell settings, source an unsynced `.zshrc.local` from the shared
`.zshrc`. Platform-specific destination paths are local registration settings and are
never taken from a remote payload.

## Inspect, resolve and roll back

```sh
clipsync files status
clipsync files remove COLLECTION_TO_STOP_MANAGING
clipsync files history zsh
clipsync files diff zsh
clipsync files resolve zsh local
clipsync files resolve zsh remote
clipsync files rollback zsh COMMIT_HASH
```

Status shows the transport subscription, local pending/error/conflict state, server head,
and each device's last acknowledged installation. An acknowledgement means files were
installed, not that a running application reloaded them. Device IDs are shown directly.

History is a local Git audit log including captured revisions, pre-application backups,
and preserved conflict versions. `diff` lists files changed since the latest local capture;
fast automatic capture means it will often be empty. Each Git commit stores a single
`snapshot.age` containing an encrypted, authenticated snapshot, including relative file
names, bytes, executable modes, and recovery metadata. Plaintext is never passed to Git
or written into its worktree. Ordinary `git show`/`git diff` therefore show ciphertext;
`files rollback` decrypts the chosen commit locally in memory before validation and
installation. `files diff` compares local changes against the captured baseline.

Commit messages, timestamps, revision UUIDs, collection repository names and device IDs
remain visible Git metadata. Local audit commit hashes can differ between machines.
The server retains encrypted published revision history; new clients currently audit the
snapshots they actually receive, rather than importing every old revision into local Git.
There is no automatic Git remote push in this version. Audit repositories use Git's
`fast-import`, bypassing checkout filters and hooks. Back up the account encryption key:
Git history cannot restore configurations without it.

Concurrent edits never use last-writer-wins. Publications include their parent revision;
the server atomically accepts a publication only if that parent is still the head. The
losing device preserves both snapshots in Git and stops the collection. `local` publishes
the current local files on top of the latest remote head; `remote` installs that head after
backing up current local files. Either choice is explicit. Further remote advancement is
reconciled again before publication. Rollback creates a new revision and propagates normally;
it does not rewind the server head. Pending changes/conflicts must be resolved first.

## Reliability and boundaries

- The daemon scans content about every 300 ms, then waits for a stable observation before
  capture. Pending publication and per-device installed state are persisted with atomic,
  fsynced registry writes. Offline edits are retained on disk and captured locally. A
  pending revision is retried unchanged until it appears in immutable server history.
  Edits made while it is pending are captured after it is acknowledged.
- Snapshots preserve file bytes and the executable bit. Destination files are private
  (`0600` or `0700`); ownership, ACLs, other permission bits and empty directories do not
  propagate. Deletions propagate as absence in a full snapshot. Empty parent directories
  can remain. File/directory type transitions require manual handling.
- Each collection is limited to 4 MiB of file contents and 2,048 files. Paths must be UTF-8,
  relative inside the collection, free of traversal and control characters, and without
  case-colliding file names. Destinations reject symlinks, including ancestor symlinks.
- Revisions are compressed and encrypted with the existing age account identity. An
  additional HMAC proves possession of the account secret: knowing the public encryption
  key alone cannot authorize config installation. The authenticated payload binds the
  collection, revision ID, parent and content. Decompression is bounded. Clients reject
  server head regressions relative to their durable installed revision.
- Remote `.zshrc` and `*.zsh` files are syntax-checked with `zsh -f -n` before installation.
  Local publications receive the same validation. Other formats have no validators yet.
  There are no automatic reload hooks or remote commands. New shells load new `.zshrc`
  contents; existing shells and editors may need an explicit reload.
- Application uses temporary files in the destination directory, fsync and atomic per-file
  rename. A write-ahead journal covers multi-file application. A crash or partial application
  stops automatic writes for that collection; resolve `local` or `remote` explicitly to
  recover, with another Git backup first. Multi-file updates are not an atomic directory
  switch. Ordinary editors do not participate in a shared lock: preflight and per-file
  content checks reduce races, but an external write racing the final rename cannot be
  made transactional. Git backups and explicit recovery remain available.
- The first implementation sends full snapshots, subscribes to retained encrypted history,
  and uses periodic scans rather than native filesystem notifications. Server history
  currently has no retention/compaction policy. Large histories need a later incremental
  fetch/compaction design. Git contains encrypted snapshots. The local registry still contains plaintext
  installation baselines and pending/recovery snapshots, and the active destination files
  remain plaintext for applications to read. Those inherit the account's local trust
  boundary; registry and history directories are private. This is encrypted Git storage,
  not encryption of every local state file.

## Verification

```sh
cargo test
```

For the opt-in live integration test, start an isolated local database server and publish
a new test database (never point this at production):

```sh
spacetime start --listen-addr 127.0.0.1:43127 --data-dir /tmp/clipsync-test-server --in-memory --non-interactive
# In another terminal:
spacetime publish --server http://127.0.0.1:43127 --no-config --anonymous --module-path server clipsync-file-test -y
cargo test live_two_device -- --ignored --nocapture
```

`CLIPSYNC_TEST_SERVER` and `CLIPSYNC_TEST_DATABASE` override the test target. The test uses
two authenticated device connections and isolated temporary directories; it verifies
publication, encrypted installation, simultaneous edit conflicts, explicit resolution,
rollback, Git content and per-device acknowledgements. The account/database used by the
test must be disposable. The first test account is created without an invite only on a
fresh database, following the normal first-user rule.

## Automatic server deployment

`.github/workflows/deploy-server.yml` builds the server on relevant pull requests.
A push to `main` affecting `server/**`, `spacetime.json`, or the workflow automatically
builds and publishes the module to the existing `clipsync` database on Maincloud.
The workflow can also be run manually from the Actions tab on `main`. Runs on other
branches build only. This is separate from the tagged client release workflow.

One-time credential setup, from a trusted terminal:

1. Run `spacetime login` using an account authorized to publish the existing database.
2. Run `spacetime login show --token` and copy the login token.
3. Run `gh secret set SPACETIME_TOKEN --repo pjtatlow/clipsync` and paste the token at
   the hidden prompt. Do not commit it or put it in a workflow file.

The deployment job authenticates with that secret; pull-request builds never use it.
Publishing uses CLI 2.0.5, the locked server dependency graph, and the exact compiled
Wasm artifact from the build job. Only one publish runs at a time. Data deletion is
forbidden. Because CLI 2.0.5's `--yes` also accepts breaking-client prompts, the workflow
instead supplies one confirmation for the remote server and a refusal for any following
breaking-client prompt. Such a change fails deployment and requires a separately planned
rollout. Workflow configuration changes should preserve this behavior.

To explicitly test an already configured account on its live database, use:

```sh
CLIPSYNC_TEST_USE_LOCAL_ACCOUNT=1 \
CLIPSYNC_TEST_SERVER=https://maincloud.spacetimedb.com \
CLIPSYNC_TEST_DATABASE=clipsync \
cargo test live_two_device -- --ignored --nocapture
```

This reads the local ClipSync login token and encryption identity without changing local
configuration or touching managed destination paths. Two temporary device registrations
and a unique `test-<UUID>` collection exercise encrypted sync, conflicts, resolution and
rollback. On success the test unregisters its devices. The small encrypted test collection
and its acknowledgements remain in server history; existing clients do not manage it
unless explicitly registered. The test does not modify the clipboard.
