# Security model

AI Session Vault is built to be kept somewhere other people or services might reach: a USB drive, a shared computer, a cloud-synced folder. This page explains what it protects, how, and what it doesn't.

## What's at stake

| Asset | Where it could hurt you |
|---|---|
| API keys in MCP / app configs | Anyone reading the vault could use your accounts. |
| Settings, hooks, commands, MCP servers | These make your machine run programs. A planted one runs on every device you restore to. |
| Auto-memory, CLAUDE.md, rules, session logs | Loaded into your AI agent's context. Planted text can steer an agent that has shell access. |
| Chat transcripts | Private conversations and anything you pasted into them. |

## Protections

### Passphrase and keys

- On the first backup you create a passphrase (12+ characters). The tool stores only a random salt and a check value in `data/vault_key.json`. Neither can be used to recover the passphrase or the keys.
- Two 256-bit keys are derived with **scrypt** (N=2^17, r=8, p=1, the OWASP-recommended setting): one for encryption, one for signing.
- A wrong passphrase is detected before anything is read or written.

### Encryption of API keys

- Secret values are encrypted with **AES-256-GCM** (random 96-bit nonce per value, from the `cryptography` package) and stored as `vault-enc:v1:…`.
- Secrets detected:
  - everything inside `env`, `headers`, `http_headers` or `env_http_headers` blocks
  - fields whose name contains key, token, secret, passw…, auth, credential, bearer or cookie
  - values that look like known key formats: `sk-…`, `ghp_…`/`github_pat_…`, `xox?-…`, `AKIA…`, `AIza…`, JWTs
- Files covered: `settings.json`, `claude_desktop_config.json`, Codex `config.toml`, and the vault's `mcp_servers.json`.
- **Never plaintext:** if `cryptography` isn't installed, secrets are left out of the vault, or an existing encrypted value is kept. Restore never installs a config with an undecryptable placeholder; it skips it until the key can be decrypted.
- An unchanged secret keeps its existing ciphertext, so repeated backups don't rewrite files.

### Signing (integrity)

- Every file backup writes into the vault is signed with **HMAC-SHA256** over its vault path and exact contents. Signatures are kept in `data/signatures.json`.
- Restore reads each file once (refusing symlinks), verifies the signature over those same bytes, and only then installs it. Unsigned or modified files are listed and never installed.
- Signed metadata (`_vault_meta.json`) decides which local folder a project's sessions and memory go into, so it can't be redirected.
- A backup from a device holding the real file replaces a tampered vault copy. Planted MCP servers are removed from the vault.

### Other safeguards

- Login credentials (`.credentials.json`, Codex `auth.json`) and account state are never copied.
- Folder names from the vault are sanitized, so restore can't write outside `~/.claude/projects/`.
- Symlinks inside the vault are ignored on restore.
- Restore prints each MCP server it adds, with its command. Secret-looking arguments are masked.
- Before restore overwrites a local file, a copy goes to `~/.ai-vault-backups/`.
- The archive page escapes all chat content, and embeds it in a way that chat text can't break out of.

## What it does not protect

- **Chat transcripts are not encrypted.** Session logs and the imported archive are readable by anyone who can read the vault folder. They are signed, so they can't be altered without detection.
- **Rollback.** Someone with write access to the vault could restore an older, genuinely signed copy of a file. It's your own content, but it could undo a recent change.
- **Context packs** are built from vault files without checking signatures (this command doesn't ask for the passphrase). Read a pack before pasting it into a chat.
- **Secret detection is heuristic.** A key in an unusual place (for example a plain command-line argument without a recognizable format) may not be detected. Prefer passing keys through `env`.
- **A compromised device** can read everything it syncs and can sign what it writes, just as you can.
- **Passphrase strength:** the vault's security rests on your passphrase. Use a long, unique one.

## Recommendations

- Keep the vault in a folder only you can read and write.
- Install `cryptography` on every device (`pip install cryptography`).
- Use a password-manager-generated passphrase.
- Prefer typing the passphrase over `AI_VAULT_PASSPHRASE` on shared machines.
- Never commit the `data/` folder (it's already in `.gitignore`).

## Reporting a vulnerability

Please open a GitHub issue with the label `security`, or contact the repository owner privately through GitHub if the details are sensitive. Include steps to reproduce and the version (commit) you tested.
