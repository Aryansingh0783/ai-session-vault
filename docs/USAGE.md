# Usage reference

Everything `vault.py` can do. For a walkthrough, start with the [README](../README.md).

Run commands from the folder that contains `vault.py`. On Windows use `py vault.py` (or `python vault.py`) in place of `python3 vault.py`.

## Interactive menu

```
python3 vault.py
```

With no arguments, the tool shows a numbered menu. The launchers (`Run-Windows.bat`, `Run-Mac.command`, `run-linux.sh`) open this menu and pass through any arguments you give them.

## `sync`

```
python3 vault.py sync [--only COMPONENTS] [--map OLD=NEW ...] [--dry-run]
```

Runs `backup` then `restore`, asking for the passphrase once. This is the command to use day to day.

## `backup`

```
python3 vault.py backup [--only COMPONENTS] [--map OLD=NEW ...] [--dry-run]
```

Copies this device's files into the vault when they are newer than the vault's copy.

- Paths inside your home folder in session logs are stored as `~/...`.
- API keys are encrypted; each file written is signed.
- A vault file that fails its signature check is replaced with this device's copy.
- If a vault file has the same content as this device's copy but no signature (from a version before signing), it gets signed.
- MCP servers from `~/.claude.json` are merged into `data/claude-code/mcp_servers.json`. Unsigned entries are dropped.

## `restore`

```
python3 vault.py restore [--only COMPONENTS] [--map OLD=NEW ...] [--dry-run]
```

Copies vault files onto this device when they are newer than the local copy.

- Only files with a valid signature are installed. Rejected files are listed after the run.
- API keys are decrypted. If a key can't be decrypted on this device, the file containing it is skipped (it's never installed with a placeholder) and applies on a later restore.
- `~/...` paths in session logs are rewritten for this device, and `--map` rules are applied.
- New MCP servers are added to `~/.claude.json` and printed with their command. Servers you already have are left alone.
- Any local file about to be replaced is first saved to `~/.ai-vault-backups/<date-time>/`.
- Symlinks inside the vault are ignored.

Close Claude Code, Claude Desktop and Codex before restoring, or they may overwrite restored files.

## Options for `sync`, `backup` and `restore`

| Option | Meaning |
|---|---|
| `--only claude-code,codex,claude-desktop` | Limit to some components (comma-separated). Default: all three. |
| `--map OLD=NEW` | Rewrite a path prefix on restore. Repeatable. `OLD` may be from another OS; `NEW` may start with `~`. Windows paths match case-insensitively. |
| `--dry-run` | Print what would be copied; change nothing. Still asks for the passphrase. |

## `import`

```
python3 vault.py import [PATH ...]
```

Imports claude.ai and ChatGPT data exports into the archive.

- With no paths, imports every `.zip`, `.json` or folder in `data/inbox/` and moves it to `data/inbox/done/` afterwards.
- A path can be the export `.zip`, an extracted folder, or `conversations.json` itself. The service is detected from the file contents.
- Conversations are keyed by ID: new ones are added, changed ones are updated, unchanged ones are skipped.
- Rebuilds `data/archive/archive.html` and `data/archive/markdown/`.

No passphrase needed.

## `context`

```
python3 vault.py context [SEARCH] [--pick N] [--max-chars N]
```

Builds a context pack from an imported chat or a Claude Code session in the vault.

- `SEARCH` matches chat titles, the first message of Claude Code sessions, project paths, or the start of a session ID. Leave it empty to list everything.
- With several matches, you get a numbered list to pick from (or pass `--pick N`).
- `--max-chars` caps the pack's length (default 60000). Long conversations keep the first message plus as many recent messages as fit.
- The pack is saved to `data/context-packs/<date>-<title>.md` and copied to the clipboard (Windows `clip`, macOS `pbcopy`, Linux `wl-copy`, `xclip` or `xsel`).

No passphrase needed.

## `status`

```
python3 vault.py status
```

Shows whether the vault has a passphrase, when each device last backed up or restored, how many projects, sessions and conversations the vault holds, and where the archive page is.

## Environment variables

| Variable | Effect |
|---|---|
| `AI_VAULT_PASSPHRASE` | Use this passphrase instead of prompting. Needed for non-interactive runs. Anything running as your user can read environment variables, so prefer the prompt on shared machines. |
| `CLAUDE_CONFIG_DIR` | Where Claude Code keeps its files, if you've moved them from `~/.claude`. |
| `CODEX_HOME` | Where Codex keeps its files, if you've moved them from `~/.codex`. |
| `APPDATA` | (Windows) Used to find Claude Desktop's config. |

## Vault layout

```
data/
├── vault_key.json            passphrase salt + check value (no key material)
├── signatures.json           HMAC signature of every vault file
├── manifest.json             devices and when they last synced
├── claude-code/
│   ├── config/               CLAUDE.md, settings.json, rules/, commands/, agents/, skills/, output-styles/
│   ├── projects/<project>/   session logs, subagent logs, memory/, _vault_meta.json
│   └── mcp_servers.json      user-scope MCP servers (keys encrypted)
├── claude-desktop/           claude_desktop_config.json (keys encrypted)
├── codex/                    config.toml (keys encrypted), AGENTS.md, prompts/, sessions/, archived_sessions/
├── inbox/                    drop export zips here; imported ones move to inbox/done/
├── archive/                  claude.json, chatgpt.json, archive.html, markdown/
└── context-packs/            generated context packs
```

`data/` is in `.gitignore`. Never commit it: it contains your chat history.

## Scheduled or scripted use

Example (macOS/Linux, cron, every evening at 19:05):

```
5 19 * * * cd /path/to/ai-session-vault && AI_VAULT_PASSPHRASE='…' python3 vault.py backup >> ~/ai-vault.log 2>&1
```

Scheduled runs should usually be `backup`. Run `restore` yourself, with your AI apps closed.
