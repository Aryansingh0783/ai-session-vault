# AI Session Vault (Rust rewrite)

Carry your Claude Code, Codex and Claude Desktop sessions between computers. Everything in the vault is
encrypted, and a plain file-sync tool (OneDrive, Google Drive, Dropbox, Syncthing, a USB stick) can't corrupt it.

Status: **core + CLI working** (`avault`), 31 automated tests. The desktop UI is waiting on designs.

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
avault sync --block-secrets                    # or refuse to upload logs that contain them
avault verify --deep                           # integrity check of every vault file
avault exclude "~/.claude-mem/observer-sessions"  # never upload a noisy project (plugin-generated sessions)
avault enroll                                  # one-time code so another device can join without the passphrase
avault join --vault "<folder>" --code <code>   # on the new device
avault remember                                # keep the passphrase in the OS credential store (daemon runs unattended)
avault overview --json                         # devices / projects / totals as JSON for a UI
avault devices                                 # who uses this vault (--remove <id> to forget one)
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
| `settings.json` replaced wholesale | Merged **key by key**; machine-specific keys (`statusLine`, `apiKeyHelper`, `awsAuthRefresh`, ... editable via `local_keys` in `~/.ai-vault/config.json`) never leave their device |
| Text edited on two devices: silent loser | `CLAUDE.md`, memory notes, rules, TOML are **merged three-way** when the edits touch different lines; overlapping edits fall back to newest-wins with a safety copy |
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

## Tested on real data
A read-only backup of a real `~/.claude` + `~/.codex` (3,177 files, ~1.3 GB of sessions) produced a 227 MB encrypted vault in 21 s; the next run took 0.4 s; `verify --deep` passed. 1,077 of 1,099 session logs came from a plugin's observer sessions, hence `avault exclude`.

## Roadmap (from the pain-point research)

Done: device enrollment with revocable access (`enroll` / `join` / `devices --revoke`), three-way merge of text files, project exclusion, OS keychain storage (`remember`), UI view-model API (`vault_core::overview`), key-level `settings.json` merge with per-machine keys, `--block-secrets`, consistent snapshots of live SQLite databases (Codex history) via SQLite's backup API, `verify`, `devices`, chunked streaming storage, always-on engine, `--redact`, Python-vault import, portable project ids, log merge, conflict detection, encryption of everything, recovery key, atomic writes,
partial-line guard, MSIX desktop path, secret scan in `doctor`, retention warning.

Next:
1. **Desktop UI** (Tauri, designs pending) on top of `vault-core` (`--json` already gives it a stable surface).
2. Streaming merge for logs over 2 GiB (smaller logs merge in memory; larger ones are skipped and reported). Content-defined chunking if edits-in-the-middle ever matter.
4. Run the engine as an OS service / tray item (the engine itself exists: `avault daemon`).
6. Per-OS layers for `settings.json` (key-level merge and per-machine keys already exist).
7. Full key rotation (re-encrypt) so a revoked device that already copied the key is locked out too.
8. Full-text index (tantivy) and neutral session schema for Codex rollouts.
9. `relocate` command and a per-Claude-version compatibility self-test for the project-folder encoding.
10. Import v1 settings/MCP (their encrypted fields) - needs scrypt + AES-GCM decrypt.
