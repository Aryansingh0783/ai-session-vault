# AI Session Vault (Rust rewrite)

Carry your Claude Code, Codex and Claude Desktop sessions between computers. Everything in the vault is
encrypted, and a plain file-sync tool (OneDrive, Google Drive, Dropbox, Syncthing, a USB stick) can't corrupt it.

Status: **core + CLI working** (`avault`), 23 automated tests. The desktop UI is waiting on designs.

```
crates/vault-core   library: crypto, vault store, sync engine, archive/search, doctor (no UI)
crates/vault-cli    `avault` command line, with --json output for a future desktop app
```

## Quick start

```
avault init --vault "D:\OneDrive\AI Vault"     # once; prints a recovery key - keep it
avault sync                                    # on every device: pulls others' changes, then publishes yours
avault doctor                                  # checks for the common gotchas
avault sync --redact                           # strip API keys/tokens from logs before they leave this device
avault daemon                                  # always-on engine: file watching, syncs only what changed
avault import-v1 "<old python vault>\data"     # bring over a vault made by the Python version
avault import export.zip                       # claude.ai / ChatGPT data exports -> searchable archive
avault search "rust lifetimes"                 # across archive + Claude Code sessions
avault context "rust lifetimes" --pick 1       # paste-ready context pack for a new chat
```

## What changed from the Python version

| Python v1 | Rust v2 |
|---|---|
| Session logs stored as plaintext in the vault; only config secrets encrypted | **Everything** compressed + encrypted (XChaCha20-Poly1305); object ids are keyed hashes |
| Passphrase lost = vault lost | Random master key wrapped by passphrase **and** a recovery key; `passwd` re-wraps without re-encrypting |
| Newest-wins by mtime: a session continued on two devices loses one side | JSONL logs are **merged by line union**; nothing is overwritten |
| One shared `signatures.json` / `manifest.json` (cloud-sync conflict magnet) | Write-once objects + one manifest per device: **no file is ever written by two machines** |
| Config edited on two devices: silent loser | Conflict detected via last-synced hash, reported, loser saved to `~/.ai-vault/safety-copies` |
| Re-read every file each run, whole files in memory | Size+mtime hash cache; files stream through 1 MiB encrypted chunks, so a growing session log uploads only its new tail, memory stays bounded, plain files have no size limit, and an interrupted run resumes (checkpoints) |
| One-shot script, polling | **Always-on engine** (`vault_core::engine`): watches your tools' folders and the vault folder, waits for a session to go quiet, then does an incremental sync; periodic full sync as a safety net; exponential backoff when the cloud folder isn't ready; pause / resume / cancel; typed event stream (status, progress, finished, error) for a UI |
| Skipped a half-written last line? No | Trailing partial lines of live logs are never shipped |
| No guard against Claude's 30-day purge | Deletions are never synced; `doctor` warns about `cleanupPeriodDays` |
| Windows Store (MSIX) Claude Desktop config missed | Probes both config locations |
| Archive was a separate plaintext folder | Archive conversations sync (encrypted) like everything else |
| Python + PyInstaller (SmartScreen warnings) | Single static binary |

## How the vault works

```
vault/
  vault.json                   KDF params + wrapped master key slots (passphrase, recovery)
  objects/ab/<id>              immutable encrypted file versions (write-once)
  devices/<id>/manifest        encrypted; only that device writes it
```
`sync` = restore (merge others in) then backup (publish yours), like `git pull && git push`.
Project folders are matched by portable path (`~/code/app`), so `C:\Users\you\code\app` and
`/home/you/code/app` line up; `--map OLD=NEW` handles anything outside your home folder.
Only structured `cwd` fields in logs are rewritten; message text is never touched.
Logins (`.credentials.json`, `auth.json`) and `settings.local.json` never leave the device.

## Roadmap (from the pain-point research)

Done: chunked streaming storage, always-on engine, `--redact`, Python-vault import, portable project ids, log merge, conflict detection, encryption of everything, recovery key, atomic writes,
partial-line guard, MSIX desktop path, secret scan in `doctor`, retention warning.

Next:
1. **Desktop UI** (Tauri, designs pending) on top of `vault-core` (`--json` already gives it a stable surface).
2. Streaming merge for logs over 2 GiB (smaller logs merge in memory; larger ones are skipped and reported). Content-defined chunking if edits-in-the-middle ever matter.
3. SQLite online backup for Codex `state_*.sqlite` instead of skipping while a `-wal` is open.
4. Run the engine as an OS service / tray item (the engine itself exists: `avault daemon`).
5. A `block` mode for secrets (refuse to upload logs containing them).
6. Layered `settings.json` overlay (base + per-OS + per-machine) instead of whole-file replacement.
7. Device enrollment code + revoke; OS keychain storage for the passphrase.
8. Full-text index (tantivy) and neutral session schema for Codex rollouts.
9. `relocate` command and a per-Claude-version compatibility self-test for the project-folder encoding.
10. Import v1 settings/MCP (their encrypted fields) - needs scrypt + AES-GCM decrypt.
