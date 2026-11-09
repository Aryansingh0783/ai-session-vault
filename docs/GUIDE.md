# AI Session Vault: guide

## What is this, in plain words?

Claude Code, Codex and Claude Desktop keep your conversations as files on **one computer**. Switch to a laptop and
that history isn't there. AI Session Vault copies those sessions into an **encrypted folder** that you put inside
OneDrive, Google Drive, Dropbox, Syncthing or on a USB stick. Every computer that uses the same folder and passphrase
then sees the same sessions, so you can pick up a conversation on another machine.

It does **not** run in the cloud, has no account, and nobody but you can read the folder. It only talks to the
folders of your AI tools and to the vault folder you choose.

```
 Computer A                      vault folder (encrypted)                 Computer B
 ~/.claude, ~/.codex  ---->  OneDrive / Dropbox / USB stick  ---->  ~/.claude, ~/.codex
        ^                                                                  |
        +--------------------------  and back again  ---------------------+
```

## Quick start (5 minutes)

1. **Install** the Windows installer from the Releases page and open **AI Session Vault** from the Start Menu.
   (Windows SmartScreen may warn about an unknown publisher because the installer is not code-signed yet:
   *More info*, then *Run anyway*.)
2. **First computer:** choose *This is my first computer*. Pick a folder inside a cloud-synced folder
   (OneDrive, Dropbox, Google Drive; the app suggests them), choose a passphrase of 12+ characters, and
   **save the recovery key** it shows. It is the only way back in if you forget the passphrase.
3. The app opens. Press **Sync now**; the dashboard shows what was uploaded. From now on it syncs by itself
   a few seconds after a session goes quiet, and keeps running in the tray when you close the window.
4. **Another computer:** install the app there too, choose *I already have a vault*, pick the same (synced)
   folder and enter the passphrase. Or on the first computer open Devices, Add a device, and type the one-time
   code on the new one; the passphrase never has to be typed or shared.
5. Tick *Start when I sign in* in the installer (or later: `avault autostart enable`) for hands-off use.

Prefer a terminal? `avault init --vault "D:\OneDrive\AI Vault"` creates a vault, `avault join --vault "<folder>" --code <code>`
joins one, `avault sync` syncs once, and `avault app` / `avault ui` open the app.

## How to read the app

| Page | What it is for |
|---|---|
| **Dashboard** | Overview, a 7-day activity chart, storage savings, health checks and the live engine (Up to date / Syncing / Paused / Waiting). |
| **Projects** | Every project seen on any device. A dim card means the project's folder isn't on this computer. Click a card to search its sessions. |
| **Devices** | Computers sharing this vault. Add a device with a one-time code, or revoke one. |
| **Archive** | Search and read every session, plus imported claude.ai / ChatGPT exports. "Copy as context pack" makes a paste-ready summary for a new chat. |
| **Health** | `doctor` checks, integrity check (Verify) and cleanup. A ring shows how many checks pass. Grey dashed rows are information, not problems. |
| **Settings** | Appearance (themes), which projects are excluded, and which settings keys stay on each device. |

**Themes:** Settings, then Appearance. Match system, Light, Dark, Sand and Ocean. The choice is saved on this device only.

The tray icon (with `avault app`): left click opens the window, right click gives Sync now, Pause, Resume, Quit.
Closing the window only hides it; syncing continues. Choose **Quit** to stop.

## What happens when you sync

A sync does **restore, then backup**: it first pulls what other computers published, then publishes yours.

- **Session logs (`.jsonl`)** are merged line by line. Continue the same session on two computers and you keep
  both sides; nothing is overwritten.
- **Settings** are merged key by key. Machine-specific keys (for example `statusLine`, `apiKeyHelper`) stay on their device.
- **Notes and rules (`CLAUDE.md`, memory files, `.toml`)** are merged three-way when edits touch different lines.
  If both computers changed the same line, the newer one wins and the other is saved in
  `~/.ai-vault/safety-copies`. The Dashboard reports these as conflicts.
- **Deletions never sync.** Claude purges old sessions after `cleanupPeriodDays` (30 by default). The vault keeps them.
- Files are stored as 1 MiB encrypted chunks, so a growing log uploads only its new end.

## Tips

- **Sync before you switch computers** and again when you sit down. With the engine running (`avault daemon` or
  `avault app`) this happens by itself a few seconds after a session goes quiet.
- **Close Claude Code/Desktop before restoring** onto a computer, so they can't overwrite restored files.
- **Raise `cleanupPeriodDays`** in `~/.claude/settings.json` if you want old sessions to stay on the computer too.
- **Exclude noisy folders** (plugin-generated sessions) with `avault exclude "<path>"` or on the Settings page.
- **Keys in logs:** `avault sync --redact` removes API keys and tokens from logs before upload;
  `--block-secrets` refuses to upload logs that contain them. Run `avault doctor` to see whether any were found.
- **Different folder layouts:** projects are matched by portable ids, so `C:\Users\you\code\app` on Windows and
  `/home/you/code/app` on Linux are the same project. If you keep code in different places, remap with `--map "OLD=NEW"` (for example `--map "D:\work=~/work"`).
- **Preview first:** `avault sync --dry-run` shows what would change without changing anything.
- **Name your computers:** devices show their hostname. Set `AI_VAULT_DEVICE_NAME="Work laptop"` to use a friendlier name.
- **Back up the recovery key** separately from the vault folder.

## Troubleshooting

| Symptom | What to do |
|---|---|
| The app shows a setup screen | That is the first run (or the vault can't be opened yet). Create a vault, connect to one, or enter the passphrase. |
| "no vault yet" (terminal) | Run `avault init --vault <folder>` (first computer) or `avault join` (another one), or just open the app. |
| Wrong passphrase | Any command accepts the **recovery key** where it asks for the passphrase. Then run `avault passwd` to set a new passphrase. |
| Sync says the vault isn't ready / waits | The cloud tool hasn't finished downloading the folder. The engine retries with backoff; check that the folder is set to "always keep on this device". |
| A project shows as dim | Its folder isn't on this computer. Clone or copy it onto this computer, or add a `--map "OLD=NEW"`. |
| Nothing happens when I open the app | It may already be running in the tray (opening it again brings the window forward). If the system webview can't start (WebView2 runtime missing), the app opens in your browser instead; install the WebView2 runtime from Microsoft to get the native window. |
| Uninstalling | Settings > Apps > AI Session Vault. It removes the program only; your vault folder and `~/.ai-vault` stay. |
| Something looks wrong | `avault doctor`, then `avault verify --deep`. Add `--json` to any command for scripts. |
| Lost a computer | Devices page, Revoke. (Full key rotation is on the roadmap; change the passphrase with `avault passwd` as well.) |

## Safety model in one paragraph

Everything in the vault (logs, settings, names) is compressed and encrypted with XChaCha20-Poly1305 under a random
master key. The master key is wrapped twice: by your passphrase (Argon2id) and by the recovery key. Objects are
write-once and each device writes only its own manifest, so a file-sync tool never sees two computers editing one
file. The app listens on `127.0.0.1` only, with a per-run token and Host/Origin checks.

## Limits to know about

- Revoking a device stops it receiving new access, but it still holds the key it already had. Change the passphrase
  and treat the old data as exposed if a device was stolen.
- Logs larger than 2 GiB aren't merged in memory.
- The tray menu has no status badge yet.
