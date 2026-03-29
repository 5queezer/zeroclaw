# macOS Update and Uninstall Guide

This page documents supported update and uninstall procedures for Hrafn on macOS (OS X).

Last verified: **February 22, 2026**.

## 1) Check current install method

```bash
which hrafn
hrafn --version
```

Typical locations:

- Homebrew: `/opt/homebrew/bin/hrafn` (Apple Silicon) or `/usr/local/bin/hrafn` (Intel)
- Cargo/bootstrap/manual: `~/.cargo/bin/hrafn`

If both exist, your shell `PATH` order decides which one runs.

## 2) Update on macOS

### A) Homebrew install

```bash
brew update
brew upgrade hrafn
hrafn --version
```

### B) Clone + bootstrap install

From your local repository checkout:

```bash
git pull --ff-only
./install.sh --prefer-prebuilt
hrafn --version
```

If you want source-only update:

```bash
git pull --ff-only
cargo install --path . --force --locked
hrafn --version
```

### C) Manual prebuilt binary install

Re-run your download/install flow with the latest release asset, then verify:

```bash
hrafn --version
```

## 3) Uninstall on macOS

### A) Stop and remove background service first

This prevents the daemon from continuing to run after binary removal.

```bash
hrafn service stop || true
hrafn service uninstall || true
```

Service artifacts removed by `service uninstall`:

- `~/Library/LaunchAgents/com.hrafn.daemon.plist`

### B) Remove the binary by install method

Homebrew:

```bash
brew uninstall hrafn
```

Cargo/bootstrap/manual (`~/.cargo/bin/hrafn`):

```bash
cargo uninstall hrafn || true
rm -f ~/.cargo/bin/hrafn
```

### C) Optional: remove local runtime data

Only run this if you want a full cleanup of config, auth profiles, logs, and workspace state.

```bash
rm -rf ~/.hrafn
```

## 4) Verify uninstall completed

```bash
command -v hrafn || echo "hrafn binary not found"
pgrep -fl hrafn || echo "No running hrafn process"
```

If `pgrep` still finds a process, stop it manually and re-check:

```bash
pkill -f hrafn
```

## Related docs

- [One-Click Bootstrap](one-click-bootstrap.md)
- [Commands Reference](../reference/cli/commands-reference.md)
- [Troubleshooting](../ops/troubleshooting.md)
