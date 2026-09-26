#!/usr/bin/env python3
"""
AI Session Vault - carry Claude / ChatGPT / Codex sessions between devices.

Keep this folder on a USB drive or inside a synced folder (Google Drive, OneDrive,
Dropbox, iCloud). Run it on each device:  backup -> move/sync folder -> restore.

Python 3.8+, standard library only. Works on Windows, macOS and Linux.
"""
from __future__ import annotations

import argparse
import filecmp
import base64
import datetime as dt
import getpass
import hashlib
import hmac
import json
import os
import platform
import re
import shutil
import socket
import subprocess
import sys
import zipfile
from pathlib import Path

# A packaged EXE unpacks itself into a temporary folder, so there the vault lives next to the EXE instead.
FROZEN = getattr(sys, "frozen", False)
ROOT = Path(sys.executable).resolve().parent if FROZEN else Path(__file__).resolve().parent
VAULT = ROOT / "data"
TOL = 2.0  # seconds of mtime slack (FAT/exFAT/cloud drives round timestamps)
STAMP = dt.datetime.now().strftime("%Y%m%d-%H%M%S")
EXCLUDE = {".credentials.json", "auth.json", ".DS_Store", "Thumbs.db", "desktop.ini", "_vault_meta.json"}
CLAUDE_ITEMS = ["CLAUDE.md", "settings.json", "rules", "commands", "agents", "skills", "output-styles"]
CODEX_ITEMS = ["config.toml", "AGENTS.md", "prompts", "sessions", "archived_sessions",
               # Codex desktop app: its history index, sidebar state, memory and skills
               "session_index.jsonl", "state_*.sqlite", "thread_history_*.sqlite", "memories_*.sqlite",
               "memories", "skills", ".codex-global-state.json"]
COMPONENTS = ("claude-code", "claude-desktop", "codex")

try:
    sys.stdout.reconfigure(errors="replace")  # type: ignore[attr-defined]
except Exception:
    pass


def log(*a):
    print(*a, flush=True)


def read_json(path: Path, default=None):
    try:
        with open(path, encoding="utf-8-sig") as f:
            return json.load(f)
    except (OSError, ValueError):
        return default


def write_json(path: Path, obj, indent=2):
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(path.name + ".vaulttmp")
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(obj, f, ensure_ascii=False, indent=indent)
    os.replace(tmp, path)


def now_iso():
    return dt.datetime.now().strftime("%Y-%m-%d %H:%M")


# --------------------------------------------------------------------------- env
class Env:
    """Where things live on *this* device."""

    def __init__(self, home: str | None = None):
        test = home is not None
        self.home = Path(home).expanduser().resolve() if test else Path.home()
        cfg = os.environ.get("CLAUDE_CONFIG_DIR")
        if cfg and not test:
            self.claude_dir = Path(cfg).expanduser()
            self.claude_json = self.claude_dir / ".claude.json"
        else:
            self.claude_dir = self.home / ".claude"
            self.claude_json = self.home / ".claude.json"
        cx = os.environ.get("CODEX_HOME")
        self.codex_dir = Path(cx).expanduser() if cx and not test else self.home / ".codex"
        system = platform.system()
        if system == "Darwin":
            self.desktop_cfg = self.home / "Library/Application Support/Claude/claude_desktop_config.json"
        elif system == "Windows":
            appdata = os.environ.get("APPDATA") if not test else None
            self.desktop_cfg = Path(appdata or self.home / "AppData/Roaming") / "Claude/claude_desktop_config.json"
        else:
            self.desktop_cfg = self.home / ".config/Claude/claude_desktop_config.json"


# ------------------------------------------------------------------ path mapping
def _norm(p: str) -> str:
    return p.replace("\\", "/")


def _is_win(p: str) -> bool:
    return bool(re.match(r"^[A-Za-z]:[\\/]", p)) or p.startswith("\\\\")


def _prefix_match(p: str, prefix: str):
    """Return the remainder (posix style) if p is inside prefix, '' if equal, else None."""
    a, b = _norm(p), _norm(prefix).rstrip("/")
    if not b:
        return None
    ac, bc = (a.lower(), b.lower()) if (_is_win(p) or _is_win(prefix)) else (a, b)
    if ac == bc:
        return ""
    if ac.startswith(bc + "/"):
        return a[len(b) + 1:]
    return None


def _join(base: str, rest: str) -> str:
    if base.startswith("~"):
        return base.rstrip("/\\") + ("/" + rest if rest else "")
    return os.path.join(base, *rest.split("/")) if rest else base


def to_portable(p: str, home) -> str:
    """Absolute path on this device -> '~/...' when under home."""
    rest = _prefix_match(p, str(home))
    if rest is None:
        return p
    return "~" if rest == "" else "~/" + rest


def to_local(p: str, home, maps=()) -> str:
    """Vault path ('~/...' or foreign absolute) -> absolute path on this device."""
    for old, new in maps:
        rest = _prefix_match(p, old)
        if rest is not None:
            p = _join(new, rest)
            break
    if p == "~" or p.startswith("~/"):
        p = _join(str(home), p[2:])
    return p


def encode_project(path: str) -> str:
    """Claude Code names ~/.claude/projects/<dir> by replacing non-alphanumerics with '-'."""
    return re.sub(r"[^A-Za-z0-9]", "-", path)


def project_key(portable: str) -> str:
    slug = re.sub(r"[^A-Za-z0-9]+", "-", portable.replace("~", "HOME", 1)).strip("-")[:80]
    return f"{slug}-{hashlib.sha1(portable.encode()).hexdigest()[:8]}"


def parse_maps(items, home) -> list:
    maps = []
    for m in items or []:
        if "=" not in m:
            raise SystemExit(f"--map must look like OLD=NEW, got: {m}")
        old, new = m.split("=", 1)
        maps.append((old.strip(), os.path.expanduser(new.strip()) if not new.strip().startswith("~") else new.strip()))
    return maps


# ------------------------------------------------------------- jsonl rewriting
def _rewrite_cwd(obj, fn) -> bool:
    changed = False
    if isinstance(obj, dict):
        for k, v in obj.items():
            if k == "cwd" and isinstance(v, str):
                nv = fn(v)
                if nv != v:
                    obj[k] = nv
                    changed = True
            elif isinstance(v, (dict, list)):
                changed |= _rewrite_cwd(v, fn)
    elif isinstance(obj, list):
        for v in obj:
            changed |= _rewrite_cwd(v, fn)
    return changed


def transform_jsonl(data: bytes, fn) -> bytes:
    """Rewrite every "cwd" field; lines that don't change are kept byte-for-byte."""
    out = []
    for line in data.decode("utf-8", errors="surrogateescape").splitlines(keepends=True):
        s = line.strip()
        if s.startswith("{") and '"cwd"' in s:
            try:
                obj = json.loads(s)
                if _rewrite_cwd(obj, fn):
                    nl = "\r\n" if line.endswith("\r\n") else ("\n" if line.endswith("\n") else "")
                    line = json.dumps(obj, ensure_ascii=False, separators=(",", ":")) + nl
            except ValueError:
                pass
        out.append(line)
    return "".join(out).encode("utf-8", errors="surrogateescape")


# ------------------------------------------------------------ vault security
KEYFILE, SIGFILE = "vault_key.json", "signatures.json"
ENC, RED = "vault-enc:v1:", "vault-redacted"
SECRET_JSON, SECRET_TOML = {"settings.json", "claude_desktop_config.json", ".codex-global-state.json"}, {"config.toml"}
SECRET_BAGS = {"env", "headers", "http_headers", "env_http_headers"}
SECRET_NAME = re.compile(r"(key|token|secret|passw|auth|credential|bearer|cookie)", re.I)
SECRET_VALUE = re.compile(r"(sk-[A-Za-z0-9_\-]{16,}|gh[pousr]_[A-Za-z0-9]{20,}|github_pat_\w{20,}|xox[abprs]-[A-Za-z0-9\-]{10,}"
                          r"|AKIA[0-9A-Z]{16}|AIza[0-9A-Za-z_\-]{30,}|eyJ[\w\-]{10,}\.[\w\-]{10,}\.[\w\-]+)")
TOML_KEY = r"""(?:"[^"]*"|'[^']*'|[A-Za-z0-9_\-]+)"""
# key = "basic string" | 'literal string'  (bare, quoted or dotted keys). Values are kept raw, so round trips are exact.
TOML_PAIR = re.compile(rf"""((?<![\w"'])({TOML_KEY}(?:\s*\.\s*{TOML_KEY})*)\s*=\s*)("((?:[^"\\]|\\.)*)"|'([^']*)')""")


BIG, CHUNK = 32 * 1024 ** 2, 4 * 1024 ** 2  # files above BIG are copied and signed in CHUNK-sized pieces


def plain(name: str) -> bool:
    """Files stored in the vault byte-for-byte (no path rewriting or secret encryption)."""
    return not name.endswith(".jsonl") and name not in SECRET_JSON and name not in SECRET_TOML


KDF_N_NEW, KDF_N_ALLOWED = 2 ** 17, (2 ** 15, 2 ** 16, 2 ** 17, 2 ** 18)  # scrypt cost (OWASP: N=2^17, r=8, p=1)
MIN_PASSPHRASE = 12


def read_nofollow(p: Path):
    """Read a vault file once, refusing symlinks (no swap between the check and the read)."""
    try:
        fd = os.open(p, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0) | getattr(os, "O_BINARY", 0))
    except OSError:
        return None
    with os.fdopen(fd, "rb") as fh:
        return fh.read()


class Keys:
    """Passphrase-derived keys: AES-256-GCM for secrets, HMAC-SHA256 for signing config files."""

    def __init__(self, passphrase: str, salt: bytes, n: int = KDF_N_NEW):
        k = hashlib.scrypt(passphrase.encode(), salt=salt, n=n, r=8, p=1, maxmem=512 * 1024 * 1024, dklen=64)
        self.mac_key = k[32:]
        try:
            from cryptography.hazmat.primitives.ciphers.aead import AESGCM
            self.aead = AESGCM(k[:32])
        except ImportError:
            self.aead = None

    def check(self) -> str:
        return hmac.new(self.mac_key, b"ai-session-vault:check", hashlib.sha256).hexdigest()

    def sign(self, rel: str, data: bytes) -> str:
        return hmac.new(self.mac_key, rel.encode() + b"\0" + data, hashlib.sha256).hexdigest()

    def signer(self, rel: str):
        """Incremental version of sign() for large files: update() with the contents, then hexdigest()."""
        return hmac.new(self.mac_key, rel.encode() + b"\0", hashlib.sha256)

    def seal(self, value: str, prev=None) -> str:
        if not self.aead:  # can't encrypt here: never store plaintext; keep another device's ciphertext
            return prev if isinstance(prev, str) and prev.startswith(ENC) else RED
        if self.unseal(prev) == value:
            return prev  # unchanged secret keeps its ciphertext (no needless sync churn)
        nonce = os.urandom(12)
        return ENC + base64.urlsafe_b64encode(nonce + self.aead.encrypt(nonce, value.encode(), b"v1")).decode()

    def unseal(self, token):
        if not (self.aead and isinstance(token, str) and token.startswith(ENC)):
            return None
        try:
            raw = base64.urlsafe_b64decode(token[len(ENC):])
            return self.aead.decrypt(raw[:12], raw[12:], b"v1").decode()
        except Exception:
            return None


def no_tty_help() -> str:
    if os.name == "nt" and (os.environ.get("MSYSTEM") or os.environ.get("TERM")):
        run, click = ("winpty ./ai-session-vault.exe", "ai-session-vault.exe") if FROZEN \
            else ("winpty py vault.py", "Run-Windows.bat")
        return (f"Git Bash can't show a hidden passphrase prompt. Run `{run}`, double-click {click},"
                " or set AI_VAULT_PASSPHRASE.")
    return "Set AI_VAULT_PASSPHRASE or run this in a terminal to enter the vault passphrase."


def load_keys(dry=False, create=True) -> Keys:
    info = read_json(VAULT / KEYFILE, None)
    pw = os.environ.get("AI_VAULT_PASSPHRASE") or None  # empty (e.g. cleared with setx "") = not set
    tty = sys.stdin.isatty()
    if isinstance(info, dict) and info.get("salt"):
        if pw is None:
            if not tty:
                raise SystemExit(no_tty_help())
            pw = getpass.getpass("Vault passphrase: ")
        n = info.get("n", 2 ** 15)  # vaults created before N was stored used 2^15
        if n not in KDF_N_ALLOWED:
            raise SystemExit(f"Invalid {KEYFILE} (unexpected scrypt cost).")
        keys = Keys(pw, base64.b64decode(info["salt"]), n)
        if not hmac.compare_digest(keys.check(), str(info.get("check", ""))):
            raise SystemExit("Wrong vault passphrase.")
        return keys
    if (VAULT / SIGFILE).exists():  # vault content exists but its key file is missing: don't fork the vault
        raise SystemExit(f"{KEYFILE} is missing from this vault but other vault files are here. If the folder is"
                         " still syncing, wait for it to finish. Otherwise restore the file from your other device.")
    if not create:
        raise SystemExit("There's no vault here yet. Run Sync or Backup on the device that has your sessions"
                         " first (and let this folder finish syncing).")
    if pw is None:
        if not tty:
            raise SystemExit(no_tty_help())
        log("Create a vault passphrase. It encrypts your API keys and signs your settings so a")
        log("tampered vault can't install anything. Use the same one on every device. It can't be recovered.")
        log("Already made a vault on another device? Press Ctrl+C and let this folder finish syncing first.")
        while True:
            pw = getpass.getpass(f"New passphrase ({MIN_PASSPHRASE}+ characters): ")
            if len(pw) < MIN_PASSPHRASE:
                log("Too short.")
            elif getpass.getpass("Repeat: ") == pw:
                break
            else:
                log("Didn't match.")
    elif len(pw) < MIN_PASSPHRASE:
        raise SystemExit(f"AI_VAULT_PASSPHRASE must be at least {MIN_PASSPHRASE} characters for a new vault.")
    salt = os.urandom(16)
    keys = Keys(pw, salt)
    if not dry:
        write_json(VAULT / KEYFILE, {"v": 1, "kdf": "scrypt", "n": KDF_N_NEW, "r": 8, "p": 1,
                                     "salt": base64.b64encode(salt).decode(), "check": keys.check()})
    return keys


def get_path(obj, path):
    for p in path:
        if isinstance(obj, dict) and p in obj:
            obj = obj[p]
        elif isinstance(obj, list) and isinstance(p, int) and p < len(obj):
            obj = obj[p]
        else:
            return None
    return obj


def map_json_strings(obj, fn, sealing, path=(), bag=False):
    """Apply fn(path, value) to secret-looking strings (sealing) or to every string (opening)."""
    items = list(obj.items()) if isinstance(obj, dict) else list(enumerate(obj)) if isinstance(obj, list) else []
    for k, v in items:
        p = path + (k,)
        if isinstance(v, str):
            if not sealing or bag or (isinstance(k, str) and SECRET_NAME.search(k)) or SECRET_VALUE.search(v):
                obj[k] = fn(p, v)
        elif isinstance(v, (dict, list)):
            map_json_strings(v, fn, sealing, p, bag or (isinstance(k, str) and k in SECRET_BAGS))
    return obj


def map_toml_strings(text: str, fn, sealing) -> str:
    """Line-based: apply fn((section, inline_table, key), value) to quoted TOML values."""
    out, section = [], ""
    for line in text.splitlines(keepends=True):
        m = re.match(r"\s*\[+\s*([^\]]+?)\s*\]+\s*(#.*)?$", line)
        if m:
            section = m.group(1)
            out.append(line)
            continue
        bag = section.split(".")[-1].strip("\"'") in SECRET_BAGS
        im = re.match(r"\s*([A-Za-z0-9_\-]+)\s*=\s*\{", line)
        inline = im.group(1) if im and im.group(1) in SECRET_BAGS else ""

        def rep(mm):
            key = mm.group(2)
            parts = [x.strip("\"'") for x in re.findall(TOML_KEY, key)]
            quote, val = ('"', mm.group(4)) if mm.group(4) is not None else ("'", mm.group(5))
            if sealing and not (bag or inline or any(x in SECRET_BAGS for x in parts[:-1])
                                or SECRET_NAME.search(parts[-1]) or SECRET_VALUE.search(val)):
                return mm.group(0)
            return mm.group(1) + quote + fn((section, inline, key), val) + quote
        out.append(TOML_PAIR.sub(rep, line))
    return "".join(out)


def toml_pairs(text: str) -> dict:
    found: dict = {}
    map_toml_strings(text, lambda p, v: found.setdefault(p, v), False)
    return found


# ------------------------------------------------------------------ file ops
def walk(root: Path):
    for dp, dns, fns in os.walk(root):
        dns[:] = [d for d in dns if d not in (".git", "node_modules")]
        for f in fns:
            if f in EXCLUDE or f.endswith(".vaulttmp"):
                continue
            yield Path(dp) / f


class Op:
    def __init__(self, env: Env, direction: str, keys: Keys, dry=False, maps=()):
        self.env, self.keys, self.dry, self.maps = env, keys, dry, maps
        self.push = direction == "backup"
        self.copied = self.skipped = self.saved = self.redacted = 0
        self.unsigned: list = []
        self.failed: list = []
        self.busy: list = []
        self.unresolved: set = set()
        self.sigs = read_json(VAULT / SIGFILE, {}) or {}
        self.new_sigs: dict = {}

    # --- signatures
    def sig_ok_file(self, rel: str, path: Path) -> bool:
        h = self.keys.signer(rel)
        with open(path, "rb") as fh:
            for chunk in iter(lambda: fh.read(CHUNK), b""):
                h.update(chunk)
        return hmac.compare_digest(str(self.sigs.get(rel, "")), h.hexdigest())

    def sig_ok(self, rel: str, data: bytes) -> bool:
        return hmac.compare_digest(str(self.sigs.get(rel, "")), self.keys.sign(rel, data))

    def set_sig(self, rel: str, data: bytes):
        self.sigs[rel] = self.new_sigs[rel] = self.keys.sign(rel, data)

    def save_sigs(self):
        if self.new_sigs and not self.dry:
            cur = read_json(VAULT / SIGFILE, {}) or {}
            cur.update(self.new_sigs)
            write_json(VAULT / SIGFILE, cur)

    # --- secrets
    def secret(self, path, value: str, other):
        if self.push:
            if not self.keys.aead:
                self.redacted += 1
            return self.keys.seal(value, other)
        if not (value.startswith(ENC) or value == RED):
            return value
        plain = self.keys.unseal(value)
        if plain is not None:
            return plain
        if isinstance(other, str) and not (other.startswith(ENC) or other == RED):
            return other  # couldn't decrypt: keep this device's own value
        self.unresolved.add("/".join(map(str, path)))
        return value

    def convert(self, data: bytes, name: str, other: Path, jsonl_fn=None):
        """push: encrypt secrets (other = previous vault copy). pull: decrypt (other = this device's copy)."""
        if name.endswith(".jsonl"):
            return transform_jsonl(data, jsonl_fn) if jsonl_fn else data
        if name not in SECRET_JSON and name not in SECRET_TOML:
            return data
        try:
            text = data.decode("utf-8-sig")
            before = len(self.unresolved)
            if name in SECRET_JSON:
                prev = read_json(other, None) if other.exists() else None
                obj = map_json_strings(json.loads(text), lambda p, v: self.secret(p, v, get_path(prev, p)), self.push)
                out = (json.dumps(obj, ensure_ascii=False, indent=2) + "\n").encode()
            else:
                prev = toml_pairs(other.read_text(encoding="utf-8-sig", errors="replace")) if other.exists() else {}
                out = map_toml_strings(text, lambda p, v: self.secret(p, v, prev.get(p)), self.push).encode()
            if len(self.unresolved) > before:
                return None  # never install placeholders; a later restore (with the key) will apply it
            return out
        except (ValueError, UnicodeDecodeError):
            log(f"  ! {name}: couldn't parse it, so it wasn't copied (to be sure no secret leaks)")
            return None

    def matches_local(self, local: Path, vfile: Path) -> bool:
        """Does the vault copy hold exactly this device's content? Used to sign files from before signing existed."""
        if plain(local.name):  # stored byte-for-byte: compare in chunks (works for very large files)
            try:
                return filecmp.cmp(local, vfile, shallow=False)
            except OSError:
                return False
        push, unresolved = self.push, set(self.unresolved)
        self.push = False
        try:
            opened = self.convert(vfile.read_bytes(), local.name, local)
        finally:
            self.push, self.unresolved = push, unresolved
        cur = local.read_bytes()
        if opened is None:
            return False
        if local.name in SECRET_JSON:
            try:
                return json.loads(opened) == json.loads(cur.decode("utf-8-sig"))
            except ValueError:
                return False
        return opened == cur

    def _backup_path(self, dst: Path) -> Path:
        root = self.env.home / ".ai-vault-backups" / STAMP
        try:
            rel = dst.resolve().relative_to(self.env.home)
        except ValueError:
            rel = Path(*[re.sub(r"[:\\/]", "", p) or "root" for p in dst.resolve().parts])
        return root / rel

    def untrusted(self, rel: str, vfile: Path, local_mtime: float) -> bool:
        """Push-side check of the vault copy. Big session logs whose timestamp still matches this device's
        copy are trusted without re-reading; anything else is fully verified (restore always verifies)."""
        if rel not in self.sigs:
            return True
        if rel.endswith(".jsonl") and abs(vfile.stat().st_mtime - local_mtime) <= TOL:
            return False
        if vfile.stat().st_size > BIG:
            return not self.sig_ok_file(rel, vfile)
        return not self.sig_ok(rel, vfile.read_bytes())

    def db_busy(self, src: Path, dst: Path) -> bool:
        """An SQLite database with a non-empty -wal file is open or not cleanly closed: copying it, or
        replacing it, could corrupt it. Skip it and ask the user to close the app."""
        for p in (src, dst):
            wal = p.with_name(p.name + "-wal")
            try:
                if wal.exists() and wal.stat().st_size > 0:
                    return True
            except OSError:
                return True
        return False

    def copy(self, src: Path, dst: Path, jsonl_fn=None) -> bool:
        """Newest-wins copy. Everything restored must carry a valid signature made with your passphrase."""
        if src.suffix == ".sqlite" and self.db_busy(src, dst):
            self.busy.append(src.name)
            return False
        try:
            if not self.push and os.path.islink(src):  # restore: never follow links planted in the vault
                return False
            sm = src.stat().st_mtime
        except OSError:
            return False
        rel = (dst if self.push else src).relative_to(VAULT).as_posix()
        if dst.exists():
            fresh = dst.stat().st_mtime >= sm - TOL
            if self.push and self.untrusted(rel, dst, sm):
                if self.matches_local(src, dst):  # same content as here (e.g. from before signing): sign it
                    if not self.dry:
                        self.set_sig(rel, dst.read_bytes())
                    self.skipped += 1
                    return False
                fresh = False  # untrusted vault copy: this device's version replaces it
            if fresh:
                self.skipped += 1
                return False
        if plain(src.name) and src.stat().st_size > BIG:
            return self.copy_stream(src, dst, rel, sm)
        raw = src.read_bytes() if self.push else read_nofollow(src)
        if raw is None:
            return False
        if not self.push and not self.sig_ok(rel, raw):  # verify the exact bytes we are about to install
            self.unsigned.append(rel)
            return False
        data = self.convert(raw, src.name, dst, jsonl_fn)
        if data is None:
            return False
        self.copied += 1
        if self.dry:
            log(f"    would copy: {dst}")
            return True
        tmp = dst.with_name(dst.name + ".vaulttmp")
        try:
            if dst.exists() and not self.push:
                b = self._backup_path(dst)
                b.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(dst, b)
                self.saved += 1
            dst.parent.mkdir(parents=True, exist_ok=True)
            tmp.write_bytes(data)
            os.replace(tmp, dst)
            os.utime(dst, (sm, sm))
        except OSError as ex:  # e.g. file open in another app (Windows), path too long, disk full
            self.copied -= 1
            self.failed.append(f"{dst}: {ex.strerror or ex}")
            try:
                tmp.unlink()
            except OSError:
                pass
            return False
        if self.push:
            self.set_sig(rel, data)
        return True

    def copy_stream(self, src: Path, dst: Path, rel: str, sm: float) -> bool:
        """copy() for large files that are stored as-is: copied and signed/verified in chunks, so memory use
        stays small. Restore verifies the signature of exactly the bytes it wrote before installing them."""
        self.copied += 1
        if self.dry:
            log(f"    would copy: {dst}")
            return True
        tmp = dst.with_name(dst.name + ".vaulttmp")
        h = self.keys.signer(rel)
        try:
            dst.parent.mkdir(parents=True, exist_ok=True)
            fd = os.open(src, os.O_RDONLY | getattr(os, "O_BINARY", 0) | (0 if self.push else getattr(os, "O_NOFOLLOW", 0)))
            with os.fdopen(fd, "rb") as fi, open(tmp, "wb") as fo:
                for chunk in iter(lambda: fi.read(CHUNK), b""):
                    h.update(chunk)
                    fo.write(chunk)
            if not self.push and not hmac.compare_digest(str(self.sigs.get(rel, "")), h.hexdigest()):
                tmp.unlink()
                self.copied -= 1
                self.unsigned.append(rel)
                return False
            if dst.exists() and not self.push:
                b = self._backup_path(dst)
                b.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(dst, b)
                self.saved += 1
            os.replace(tmp, dst)
            os.utime(dst, (sm, sm))
        except OSError as ex:
            self.copied -= 1
            self.failed.append(f"{dst}: {ex.strerror or ex}")
            try:
                tmp.unlink()
            except OSError:
                pass
            return False
        if self.push:
            self.sigs[rel] = self.new_sigs[rel] = h.hexdigest()
        return True

    def items(self, src_base: Path, dst_base: Path, names, jsonl_fn=None):
        for name in names:
            matches = sorted(src_base.glob(name)) if any(c in name for c in "*?[") else [src_base / name]
            for s in matches:
                if s.is_file():
                    self.copy(s, dst_base / s.relative_to(src_base), jsonl_fn)
                elif s.is_dir():
                    for f in walk(s):
                        self.copy(f, dst_base / f.relative_to(src_base), jsonl_fn)


# --------------------------------------------------------------- Claude Code
def detect_cwd(pdir: Path):
    """Find the project's working dir from its session logs (prefer the one matching the dir name)."""
    seen: dict = {}
    files = sorted(pdir.glob("*.jsonl"), key=lambda f: f.stat().st_mtime, reverse=True)[:25]
    for f in files:
        try:
            with open(f, encoding="utf-8", errors="replace") as fh:
                for i, line in enumerate(fh):
                    if i > 300:
                        break
                    if '"cwd"' not in line:
                        continue
                    try:
                        o = json.loads(line)
                    except ValueError:
                        continue
                    c = o.get("cwd") if isinstance(o, dict) else None
                    if isinstance(c, str) and c:
                        if encode_project(c) == pdir.name:
                            return c
                        seen[c] = seen.get(c, 0) + 1
        except OSError:
            continue
    return max(seen, key=seen.get) if seen else None


MCP_REL = "claude-code/mcp_servers.json"


def claude_code_push(op: Op):
    e = op.env
    if not e.claude_dir.is_dir():
        log("  Claude Code: not installed here, skipped")
        return
    vbase = VAULT / "claude-code"
    to_p = lambda p: to_portable(p, e.home)  # noqa: E731
    op.items(e.claude_dir, vbase / "config", CLAUDE_ITEMS)
    n = 0
    proot = e.claude_dir / "projects"
    if proot.is_dir():
        for pdir in sorted(p for p in proot.iterdir() if p.is_dir()):
            cwd = detect_cwd(pdir)
            portable = to_p(cwd) if cwd else None
            vdir = vbase / "projects" / (project_key(portable) if portable else "raw-" + pdir.name)
            meta_path = vdir / "_vault_meta.json"
            mrel = meta_path.relative_to(VAULT).as_posix()
            meta = {"portable_path": portable, "encoded_name": pdir.name}
            if not op.dry and (read_json(meta_path, None) != meta or not op.sig_ok(mrel, meta_path.read_bytes())):
                write_json(meta_path, meta)  # signed: it decides which local folder the project lands in
                op.set_sig(mrel, meta_path.read_bytes())
            for f in walk(pdir):
                op.copy(f, vdir / f.relative_to(pdir), to_p)
            n += 1
    # user-scope MCP servers live in ~/.claude.json (rest of that file is account state: not synced)
    local = (read_json(e.claude_json, {}) or {}).get("mcpServers") or {}
    vpath = vbase / "mcp_servers.json"
    vault = read_json(vpath, {}) or {}
    sealed = map_json_strings(json.loads(json.dumps(local)), lambda p, v: op.secret(p, v, get_path(vault, p)), True)
    trusted = not vpath.exists() or op.sig_ok(MCP_REL, vpath.read_bytes())
    merged = {**(vault if trusted else {}), **sealed}  # unsigned vault entries are dropped, not merged
    dropped = sorted(set(vault) - set(merged))
    if dropped:
        log(f"  ! Removed unsigned MCP server(s) from the vault: {', '.join(dropped)}")
    if not op.dry and (merged != vault or not trusted):
        if merged != vault:
            write_json(vpath, merged)
        op.set_sig(MCP_REL, vpath.read_bytes())
    log(f"  Claude Code: {n} project(s), {len(local)} MCP server(s)")


def claude_code_pull(op: Op):
    e = op.env
    vbase = VAULT / "claude-code"
    if not vbase.is_dir():
        log("  Claude Code: nothing in vault")
        return
    to_l = lambda p: to_local(p, e.home, op.maps)  # noqa: E731
    op.items(vbase / "config", e.claude_dir, CLAUDE_ITEMS)
    missing, n = set(), 0
    vroot = vbase / "projects"
    if vroot.is_dir():
        for vdir in sorted(p for p in vroot.iterdir() if p.is_dir()):
            mrel = f"claude-code/projects/{vdir.name}/_vault_meta.json"
            mraw = read_nofollow(vdir / "_vault_meta.json")
            if mraw is not None and not op.sig_ok(mrel, mraw):
                op.unsigned.append(mrel)
                continue
            try:
                meta = json.loads(mraw.decode("utf-8-sig")) if mraw is not None else {}
            except ValueError:
                meta = {}
            meta = meta if isinstance(meta, dict) else {}
            portable = meta.get("portable_path")
            if portable:
                local = to_l(portable)
                enc = encode_project(local)
                if not os.path.isdir(local):
                    missing.add(local)
            else:
                enc = meta.get("encoded_name") or vdir.name[4:]
            enc = encode_project(str(enc))  # vault data is untrusted: no separators or '..' in the dir name
            ldir = e.claude_dir / "projects" / enc
            for f in walk(vdir):
                op.copy(f, ldir / f.relative_to(vdir), to_l)
            n += 1
    servers: dict = {}
    vpath = vbase / "mcp_servers.json"
    raw = read_nofollow(vpath) if vpath.exists() else None
    if raw is not None:
        if op.sig_ok(MCP_REL, raw):
            try:
                servers = json.loads(raw.decode("utf-8-sig"))
            except ValueError:
                servers = {}
            servers = servers if isinstance(servers, dict) else {}
        else:
            op.unsigned.append(MCP_REL)
    added = 0
    if servers:
        if e.claude_json.exists():
            data = read_json(e.claude_json, None)
            if isinstance(data, dict):
                cur = data.get("mcpServers") or {}
                new = {k: v for k, v in servers.items() if k not in cur}
                if new:
                    new = map_json_strings(new, lambda p, v: op.secret(p, v, get_path(cur, p)), False)
                    # skip servers whose keys couldn't be decrypted here; they're added on a later restore
                    new = {k: v for k, v in new.items() if ENC not in json.dumps(v) and RED not in json.dumps(v)}
                    added = len(new)
                    for k, v in new.items():  # MCP servers run commands: show exactly what is being added
                        v = v if isinstance(v, dict) else {}
                        cmd = " ".join(str(x) for x in [v.get("command") or v.get("url") or "?"] + list(v.get("args") or []))
                        log(f"    + MCP server '{k}': {SECRET_VALUE.sub('***', cmd)[:120]}")
                    if not op.dry:
                        try:
                            b = op._backup_path(e.claude_json)
                            b.parent.mkdir(parents=True, exist_ok=True)
                            shutil.copy2(e.claude_json, b)
                            data["mcpServers"] = {**cur, **new}
                            write_json(e.claude_json, data)
                        except OSError as ex:
                            op.failed.append(f"{e.claude_json}: {ex.strerror or ex}")
                            added = 0
        else:
            log("  Claude Code: run `claude` once on this device, then restore again to add MCP servers")
    log(f"  Claude Code: {n} project(s), {added} MCP server(s) added")
    if missing:
        log("  ! These project folders don't exist on this device (sessions restored anyway):")
        for m in sorted(missing):
            log(f"      {m}")
        log("    Clone/copy them there, or re-run restore with --map OLD=NEW to point elsewhere.")


# --------------------------------------------------------- Desktop + Codex
def desktop_push(op: Op):
    src = op.env.desktop_cfg
    if src.exists():
        op.copy(src, VAULT / "claude-desktop" / src.name)
        log("  Claude Desktop: config (MCP servers, prefs)")
    else:
        log("  Claude Desktop: no config here, skipped")


def desktop_pull(op: Op):
    src = VAULT / "claude-desktop" / "claude_desktop_config.json"
    if not src.exists():
        log("  Claude Desktop: nothing in vault")
        return
    op.copy(src, op.env.desktop_cfg)
    log("  Claude Desktop: config")


def codex_push(op: Op):
    e = op.env
    if not e.codex_dir.is_dir():
        log("  Codex CLI: not installed here, skipped")
        return
    op.items(e.codex_dir, VAULT / "codex", CODEX_ITEMS, lambda p: to_portable(p, e.home))
    log("  Codex CLI: config + sessions")


def codex_pull(op: Op):
    e = op.env
    if not (VAULT / "codex").is_dir():
        log("  Codex CLI: nothing in vault")
        return
    op.items(VAULT / "codex", e.codex_dir, CODEX_ITEMS, lambda p: to_local(p, e.home, op.maps))
    log("  Codex CLI: config + sessions")


PUSH = {"claude-code": claude_code_push, "claude-desktop": desktop_push, "codex": codex_push}
PULL = {"claude-code": claude_code_pull, "claude-desktop": desktop_pull, "codex": codex_pull}


def touch_manifest(env: Env, action: str):
    path = VAULT / "manifest.json"
    m = read_json(path, {}) or {}
    d = m.setdefault("devices", {}).setdefault(socket.gethostname(), {})
    d.update({"os": platform.system(), "home": str(env.home), f"last_{action}": now_iso()})
    write_json(path, m)


KEEP_BACKUPS = 10
MAX_BACKUP_BYTES = 3 * 1024 ** 3


def _dir_size(d: Path) -> int:
    total = 0
    for dp, _, fns in os.walk(d):
        for f in fns:
            try:
                total += os.path.getsize(os.path.join(dp, f))
            except OSError:
                pass
    return total


def prune_backups(env: Env):
    """Keep the newest safety-copy folders in ~/.ai-vault-backups: at most 10 runs and about 3 GB in total
    (the newest run is always kept)."""
    root = env.home / ".ai-vault-backups"
    try:
        runs = sorted((d for d in root.iterdir() if d.is_dir() and re.fullmatch(r"\d{8}-\d{6}", d.name)),
                      key=lambda d: d.name)
    except OSError:
        return
    for d in runs[:-KEEP_BACKUPS]:
        shutil.rmtree(d, ignore_errors=True)
    runs = runs[-KEEP_BACKUPS:]
    total = 0
    for i, d in enumerate(reversed(runs)):  # newest first
        total += _dir_size(d)
        if i > 0 and total > MAX_BACKUP_BYTES:
            shutil.rmtree(d, ignore_errors=True)


def run(direction: str, args, keys: Keys | None = None):
    env = Env(args.home)
    only = [c.strip() for c in (args.only or ",".join(COMPONENTS)).split(",") if c.strip()]
    bad = [c for c in only if c not in COMPONENTS]
    if bad:
        raise SystemExit(f"Unknown component(s): {bad}. Choose from {COMPONENTS}")
    VAULT.mkdir(parents=True, exist_ok=True)
    keys = keys or load_keys(args.dry_run, create=direction == "backup")
    op = Op(env, direction, keys, dry=args.dry_run, maps=parse_maps(args.map, env.home))
    table = PUSH if op.push else PULL
    arrow = "this device -> vault" if op.push else "vault -> this device"
    log(f"\n{direction.upper()} ({arrow}){'  [dry run]' if op.dry else ''}")
    if not op.push and not op.dry:
        log("  (close Claude Code, Claude Desktop and Codex first so they don't overwrite restored files)")
    try:
        for c in only:
            table[c](op)
    finally:
        op.save_sigs()  # keep signatures for everything already written, even if a step failed
    if not op.dry:
        touch_manifest(env, direction)
        if not op.push:
            prune_backups(env)
    log(f"  -> {op.copied} file(s) updated, {op.skipped} already up to date"
        + (f", {op.saved} replaced file(s) saved to ~/.ai-vault-backups/{STAMP}" if op.saved else ""))
    if op.busy:
        log(f"  ! Codex app history not synced ({', '.join(sorted(set(op.busy)))} in use). Quit the Codex app"
            " completely (also from the system tray, near the clock) and run Sync again.")
    if op.failed:
        log(f"  ! {len(op.failed)} file(s) couldn't be written (open in another app? close it and run again):")
        for r in op.failed[:8]:
            log(f"      {r}")
    if op.unsigned:
        log(f"  ! NOT installed: {len(op.unsigned)} file(s) in the vault aren't signed with your passphrase."
            " Someone else may have written them, or they predate signing. A backup from the device that has"
            " them replaces them; if you don't recognize one, delete it from the vault folder:")
        for r in op.unsigned[:8]:
            log(f"      {r}")
    if op.unresolved:
        log(f"  ! Not applied yet: {len(op.unresolved)} secret(s) couldn't be decrypted here, so the"
            " config holding them was skipped (it will apply on a later restore):")
        for r in sorted(op.unresolved)[:8]:
            log(f"      {r}")
    if (op.redacted or op.unresolved) and not keys.aead:
        log("  ! API keys can't be encrypted/decrypted on this device yet. Run: pip install cryptography"
            + (f"  ({op.redacted} secret(s) were not stored in the vault)" if op.redacted else ""))


# ------------------------------------------------------- export importer
def fmt_ts(v) -> str:
    if v in (None, ""):
        return ""
    try:
        if isinstance(v, (int, float)):
            d = dt.datetime.fromtimestamp(float(v))
        else:
            d = dt.datetime.fromisoformat(str(v).replace("Z", "+00:00"))
            if d.tzinfo:
                d = d.astimezone().replace(tzinfo=None)
        return d.strftime("%Y-%m-%d %H:%M")
    except (ValueError, OSError, OverflowError):
        return str(v)[:16]


def _gpt_text(content: dict) -> str:
    ctype = content.get("content_type")
    if ctype in ("thoughts", "reasoning_recap", "model_editable_context", "user_editable_context"):
        return ""
    out = []
    for p in content.get("parts") or []:
        if isinstance(p, str):
            out.append(p)
        elif isinstance(p, dict):
            if isinstance(p.get("text"), str):
                out.append(p["text"])
            elif "image" in str(p.get("content_type", "")):
                out.append("[image]")
    if not out and isinstance(content.get("text"), str):
        out.append(content["text"])
    return "\n".join(out).strip()


def parse_chatgpt(c: dict) -> dict:
    mapping = c.get("mapping") or {}
    chain, node_id, seen = [], c.get("current_node"), set()
    while node_id and node_id in mapping and node_id not in seen:
        seen.add(node_id)
        chain.append(mapping[node_id])
        node_id = mapping[node_id].get("parent")
    chain.reverse()
    if not chain:
        chain = sorted(mapping.values(), key=lambda n: ((n.get("message") or {}).get("create_time") or 0))
    msgs = []
    for node in chain:
        m = node.get("message") or {}
        role = (m.get("author") or {}).get("role")
        if role not in ("user", "assistant"):
            continue
        if (m.get("metadata") or {}).get("is_visually_hidden_from_conversation"):
            continue
        if m.get("recipient") not in (None, "all"):
            continue
        text = _gpt_text(m.get("content") or {})
        if text:
            msgs.append({"role": role, "text": text, "time": fmt_ts(m.get("create_time"))})
    return {
        "id": str(c.get("conversation_id") or c.get("id") or hashlib.sha1(json.dumps(c.get("title")).encode()).hexdigest()),
        "source": "chatgpt",
        "title": c.get("title") or "Untitled",
        "created": fmt_ts(c.get("create_time")),
        "updated": fmt_ts(c.get("update_time") or c.get("create_time")),
        "messages": msgs,
    }


def parse_claude(c: dict) -> dict:
    msgs = []
    for m in c.get("chat_messages") or []:
        role = "user" if m.get("sender") == "human" else "assistant"
        parts = [b["text"] for b in (m.get("content") or [])
                 if isinstance(b, dict) and b.get("type") == "text" and isinstance(b.get("text"), str)]
        text = "\n\n".join(parts).strip() or (m.get("text") or "").strip()
        names = [a.get("file_name") for a in (m.get("attachments") or []) + (m.get("files") or [])
                 if isinstance(a, dict) and a.get("file_name")]
        if names:
            text += ("\n\n" if text else "") + "[attached: " + ", ".join(names) + "]"
        if text:
            msgs.append({"role": role, "text": text, "time": fmt_ts(m.get("created_at"))})
    return {
        "id": str(c.get("uuid") or c.get("id")),
        "source": "claude",
        "title": c.get("name") or "Untitled",
        "created": fmt_ts(c.get("created_at")),
        "updated": fmt_ts(c.get("updated_at") or c.get("created_at")),
        "messages": msgs,
    }


def load_export(p: Path):
    pat = re.compile(r"(^|/)conversations(-\d+)?\.json$")
    raws = []
    if p.is_dir():
        raws = [f.read_bytes() for f in sorted(p.rglob("*.json")) if pat.search(f.as_posix())]
    elif zipfile.is_zipfile(p):
        with zipfile.ZipFile(p) as z:
            raws = [z.read(n) for n in sorted(z.namelist()) if pat.search(n)]
    elif p.suffix.lower() == ".json":
        raws = [p.read_bytes()]
    if not raws:
        raise ValueError("no conversations.json found (is this a Claude or ChatGPT data export?)")
    convs = []
    for raw in raws:
        data = json.loads(raw.decode("utf-8-sig"))
        if isinstance(data, dict):
            data = data.get("conversations") or [data]
        convs.extend(c for c in data if isinstance(c, dict))
    if not convs:
        return None, []
    if any("mapping" in c for c in convs[:5]):
        return "chatgpt", convs
    if any("chat_messages" in c for c in convs[:5]):
        return "claude", convs
    raise ValueError("unrecognized export format")


def cmd_import(args):
    inbox = VAULT / "inbox"
    inbox.mkdir(parents=True, exist_ok=True)
    paths = [Path(p).expanduser() for p in args.paths] if args.paths else \
        sorted(x for x in inbox.iterdir() if x.suffix.lower() in (".zip", ".json") or x.is_dir() and x.name != "done")
    if not paths:
        log(f"Nothing to import. Put your Claude/ChatGPT export .zip files in:\n  {inbox}")
        return
    total = 0
    for p in paths:
        try:
            source, convs = load_export(p)
        except (ValueError, OSError, zipfile.BadZipFile) as ex:
            log(f"  x {p.name}: {ex}")
            continue
        if not source:
            log(f"  - {p.name}: empty export")
            continue
        dbp = VAULT / "archive" / f"{source}.json"
        db = read_json(dbp, {}) or {}
        parse = parse_chatgpt if source == "chatgpt" else parse_claude
        added = updated = 0
        for c in convs:
            try:
                conv = parse(c)
            except Exception as ex:  # one malformed conversation shouldn't abort the import
                log(f"    skipped one conversation: {ex}")
                continue
            old = db.get(conv["id"])
            if old is None:
                added += 1
            elif conv["updated"] >= old.get("updated", "") and conv != old:
                updated += 1
            else:
                continue
            db[conv["id"]] = conv
        write_json(dbp, db, indent=None)
        total += len(convs)
        log(f"  + {p.name}: {source}, {len(convs)} conversations ({added} new, {updated} updated)")
        if not args.paths and p.parent == inbox:
            (inbox / "done").mkdir(exist_ok=True)
            shutil.move(str(p), str(inbox / "done" / p.name))
    if total:
        rebuild_archive()


def all_convs() -> list:
    out = []
    for src in ("claude", "chatgpt"):
        out.extend((read_json(VAULT / "archive" / f"{src}.json", {}) or {}).values())
    out.sort(key=lambda c: c.get("updated", ""), reverse=True)
    return out


def slugify(s: str, n=60) -> str:
    return (re.sub(r"[^A-Za-z0-9]+", "-", s).strip("-")[:n] or "untitled").lower()


def conv_markdown(c: dict) -> str:
    label = "Claude" if c["source"] == "claude" else "ChatGPT"
    lines = [f"# {c['title']}", "", f"_{label} · created {c['created']} · updated {c['updated']} · id {c['id']}_", ""]
    for m in c["messages"]:
        who = "User" if m["role"] == "user" else "Assistant"
        lines += [f"## {who}" + (f" · {m['time']}" if m.get("time") else ""), "", m["text"], ""]
    return "\n".join(lines)


def rebuild_archive():
    convs = all_convs()
    md_root = VAULT / "archive" / "markdown"
    if md_root.exists():
        shutil.rmtree(md_root)
    for c in convs:
        d = md_root / c["source"]
        d.mkdir(parents=True, exist_ok=True)
        name = f"{(c['created'] or '0000')[:10]}-{slugify(c['title'])}-{c['id'][:8]}.md"
        (d / name).write_text(conv_markdown(c), encoding="utf-8")
    # escape every '<' so chat text like "<!--<script>" or "</script>" can't alter how the page parses
    payload = json.dumps(convs, ensure_ascii=False, separators=(",", ":")).replace("<", "\\u003c")
    (VAULT / "archive" / "archive.html").write_text(HTML.replace("__DATA__", payload), encoding="utf-8")
    log(f"  Archive rebuilt: {len(convs)} conversations -> {VAULT / 'archive' / 'archive.html'}")


# ---------------------------------------------------------- context packs
def parse_cc_session(path: Path) -> dict:
    msgs = []
    with open(path, encoding="utf-8", errors="replace") as fh:
        for line in fh:
            try:
                o = json.loads(line)
            except ValueError:
                continue
            if not isinstance(o, dict) or o.get("type") not in ("user", "assistant"):
                continue
            if o.get("isSidechain") or o.get("isMeta"):
                continue
            m = o.get("message") or {}
            role = m.get("role") or o["type"]
            c = m.get("content")
            parts = []
            if isinstance(c, str):
                parts.append(c)
            elif isinstance(c, list):
                for b in c:
                    if not isinstance(b, dict):
                        continue
                    if b.get("type") == "text" and isinstance(b.get("text"), str):
                        parts.append(b["text"])
                    elif b.get("type") == "tool_use":
                        parts.append(f"[used tool: {b.get('name')}]")
            text = "\n".join(parts).strip()
            if not text or text.startswith(("<command-", "<local-command-", "<system-reminder>")):
                continue
            if msgs and msgs[-1]["role"] == role:
                msgs[-1]["text"] += "\n" + text
            else:
                msgs.append({"role": role, "text": text, "time": fmt_ts(o.get("timestamp"))})
    first = next((m["text"] for m in msgs if m["role"] == "user"), path.stem)
    meta = read_json(path.parent / "_vault_meta.json", {}) or {}
    return {"id": path.stem, "source": "claude-code", "title": first.splitlines()[0][:80],
            "updated": fmt_ts(path.stat().st_mtime), "project": meta.get("portable_path") or "", "messages": msgs}


def build_pack(c: dict, max_chars: int) -> str:
    label = {"claude": "Claude.ai chat", "chatgpt": "ChatGPT chat", "claude-code": "Claude Code session"}[c["source"]]
    header = (f"# Context pack: {c['title']}\n\n_Source: {label}"
              + (f" in {c['project']}" if c.get("project") else "")
              + f" · last updated {c.get('updated', '')} · {len(c['messages'])} messages_\n\n"
              "> **Note to the assistant:** Below is the transcript of an earlier conversation I had with an AI "
              "assistant on another device. Treat it as shared context and continue from where it left off. "
              "Don't summarize it back to me unless I ask.\n\n---\n\n")
    blocks = [f"**{'User' if m['role'] == 'user' else 'Assistant'}**" + (f" ({m['time']})" if m.get("time") else "")
              + f":\n\n{m['text']}\n\n" for m in c["messages"]]
    budget = max(max_chars - len(header), 2000)
    if sum(map(len, blocks)) <= budget:
        body = "".join(blocks)
    else:
        first = blocks[0][: budget // 4]
        rem = budget - len(first) - 120
        tail, used = [], 0
        for b in reversed(blocks[1:]):
            if used + len(b) > rem:
                break
            tail.append(b)
            used += len(b)
        tail.reverse()
        if not tail:
            tail = ["..." + blocks[-1][-rem:]]
        omitted = len(blocks) - 1 - len(tail)
        body = first + f"\n_[... {omitted} earlier message(s) omitted for length ...]_\n\n" + "".join(tail)
    return header + body + "---\n\n_End of prior transcript. My next message follows._\n"


def to_clipboard(text: str) -> bool:
    s = platform.system()
    if s == "Windows":
        cmds = [(["clip"], ("\ufeff" + text).encode("utf-16le"))]
    elif s == "Darwin":
        cmds = [(["pbcopy"], text.encode())]
    else:
        cmds = [(["wl-copy"], text.encode()), (["xclip", "-selection", "clipboard"], text.encode()),
                (["xsel", "-b", "-i"], text.encode())]
    for cmd, data in cmds:
        try:
            subprocess.run(cmd, input=data, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            return True
        except (OSError, subprocess.CalledProcessError):
            continue
    return False


def cmd_context(args):
    q = (args.query or "").lower().strip()
    cands = []
    for c in all_convs():
        if not q or q in c["title"].lower() or c["id"].lower().startswith(q):
            cands.append(c)
    for f in sorted((VAULT / "claude-code" / "projects").glob("*/*.jsonl"), key=lambda f: f.stat().st_mtime, reverse=True):
        if q and f.stem.lower().startswith(q):
            cands.insert(0, parse_cc_session(f))
            continue
        s = parse_cc_session(f)
        if s["messages"] and (not q or q in s["title"].lower() or q in s["project"].lower()):
            cands.append(s)
    if not cands:
        log("No match. Try a word from the chat title, a Claude Code project folder name, or a session id.")
        return
    pick = args.pick
    if pick is None and len(cands) > 1:
        for i, c in enumerate(cands[:25], 1):
            log(f"  {i:>2}. [{c['source']}] {c['title'][:70]}  ({c.get('updated', '')})")
        if not sys.stdin.isatty():
            log("Re-run with --pick N")
            return
        try:
            pick = int(input("Pick number: ").strip())
        except (ValueError, EOFError):
            return
    if not 1 <= (pick or 1) <= len(cands):
        log(f"Pick a number from 1 to {min(len(cands), 25)}.")
        return
    c = cands[(pick or 1) - 1]
    pack = build_pack(c, args.max_chars)
    out = VAULT / "context-packs" / f"{dt.date.today()}-{slugify(c['title'], 50)}.md"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(pack, encoding="utf-8")
    log(f"Context pack saved: {out}  ({len(pack):,} chars)")
    if to_clipboard(pack):
        log("Copied to clipboard - paste it as the first message of a new Claude/ChatGPT chat.")


# ------------------------------------------------------------------- status
def cmd_status(args):
    m = read_json(VAULT / "manifest.json", {}) or {}
    log(f"Vault: {VAULT}")
    log(f"  Protected by passphrase: {'yes' if (VAULT / KEYFILE).exists() else 'not yet (set on first backup)'}")
    for host, d in (m.get("devices") or {}).items():
        log(f"  device {host} ({d.get('os')}): backup {d.get('last_backup', '-')}, restore {d.get('last_restore', '-')}")
    pr = VAULT / "claude-code" / "projects"
    projects = [p for p in pr.iterdir() if p.is_dir()] if pr.is_dir() else []
    sessions = sum(1 for p in projects for _ in p.glob("*.jsonl"))
    log(f"  Claude Code: {len(projects)} projects, {sessions} sessions")
    codex = sum(1 for _ in (VAULT / "codex").rglob("*.jsonl")) if (VAULT / "codex").is_dir() else 0
    log(f"  Codex CLI: {codex} session files")
    log(f"  Claude Desktop config: {'yes' if (VAULT / 'claude-desktop/claude_desktop_config.json').exists() else 'no'}")
    for src in ("claude", "chatgpt"):
        n = len(read_json(VAULT / "archive" / f"{src}.json", {}) or {})
        log(f"  {src} archive: {n} conversations")
    if (VAULT / "archive/archive.html").exists():
        log(f"  Browse: {VAULT / 'archive/archive.html'}")


# --------------------------------------------------------------------- menu
def menu(parser):
    items = [
        ("Sync this device (backup, then restore newest from vault)", ["sync"]),
        ("Backup only   (this device -> vault)", ["backup"]),
        ("Restore only  (vault -> this device)", ["restore"]),
        ("Import Claude / ChatGPT export zips from data/inbox", ["import"]),
        ("Make a context pack (continue a chat anywhere)", ["context"]),
        ("Status", ["status"]),
    ]
    log("\nAI Session Vault")
    for i, (label, _) in enumerate(items, 1):
        log(f"  {i}. {label}")
    try:
        choice = int(input("Choose: ").strip())
        if not 1 <= choice <= len(items):
            raise ValueError
        argv = items[choice - 1][1]
    except (ValueError, EOFError):
        log("No option chosen.")
        return
    if argv == ["context"]:
        argv = ["context", input("Search (title / project / session id, blank = all): ").strip()]
    dispatch(parser.parse_args(argv))


def dispatch(args):
    if args.cmd == "sync":
        VAULT.mkdir(parents=True, exist_ok=True)
        keys = load_keys(args.dry_run)  # ask for the passphrase once
        run("backup", args, keys)
        run("restore", args, keys)
    elif args.cmd in ("backup", "restore"):
        run(args.cmd, args)
    elif args.cmd == "import":
        cmd_import(args)
    elif args.cmd == "context":
        cmd_context(args)
    elif args.cmd == "status":
        cmd_status(args)


def main():
    p = argparse.ArgumentParser(description="Carry Claude / ChatGPT / Codex sessions between devices.")
    sub = p.add_subparsers(dest="cmd")
    for name, hlp in (("backup", "copy this device's sessions/config into the vault"),
                      ("restore", "copy newer vault sessions/config onto this device"),
                      ("sync", "backup then restore")):
        s = sub.add_parser(name, help=hlp)
        s.add_argument("--only", help="comma list: claude-code,claude-desktop,codex")
        s.add_argument("--map", action="append", metavar="OLD=NEW",
                       help="remap a project path, e.g. --map \"D:\\work=~/work\" (repeatable)")
        s.add_argument("--dry-run", action="store_true", help="show what would change")
        s.add_argument("--home", help=argparse.SUPPRESS)
    s = sub.add_parser("import", help="import Claude/ChatGPT data-export zips into a searchable archive")
    s.add_argument("paths", nargs="*", help="zip/json/folder (default: everything in data/inbox)")
    s = sub.add_parser("context", help="turn a chat/session into a paste-ready context pack")
    s.add_argument("query", nargs="?", default="")
    s.add_argument("--pick", type=int)
    s.add_argument("--max-chars", type=int, default=60000)
    sub.add_parser("status", help="show what's in the vault")
    args = p.parse_args()
    if not args.cmd:
        menu(p)
        if sys.stdin.isatty():
            input("\nDone. Press Enter to close.")
        return
    dispatch(args)


HTML = r"""<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1"><title>AI Chat Archive</title>
<style>
:root{--bg:#f7f7f5;--fg:#1c1917;--mut:#78716c;--line:#e7e5e4;--card:#fff;--u:#eef0ff;--acc:#4f46e5}
@media (prefers-color-scheme:dark){:root{--bg:#0f0e0d;--fg:#e7e5e4;--mut:#a8a29e;--line:#292524;--card:#1a1817;--u:#1f1d3a;--acc:#a5b4fc}}
*{box-sizing:border-box}body{margin:0;font:14px/1.55 system-ui,-apple-system,Segoe UI,sans-serif;background:var(--bg);color:var(--fg);display:grid;grid-template-columns:340px 1fr;height:100vh}
aside{border-right:1px solid var(--line);display:flex;flex-direction:column;min-height:0}
.top{padding:12px;border-bottom:1px solid var(--line);display:grid;gap:8px}
input,select,button{font:inherit;color:var(--fg)}input,select{width:100%;padding:8px 10px;border:1px solid var(--line);border-radius:8px;background:var(--card)}
#list{overflow:auto;flex:1}.item{padding:10px 12px;border-bottom:1px solid var(--line);cursor:pointer}
.item:hover,.item.on{background:var(--card)}.t{font-weight:600;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.m{color:var(--mut);font-size:12px}main{overflow:auto;padding:24px 32px;min-height:0}
.head{max-width:860px;margin:0 auto 16px;display:flex;gap:12px;align-items:flex-start;justify-content:space-between}
h1{margin:0;font-size:20px}button{padding:8px 12px;border-radius:8px;border:1px solid var(--line);background:var(--card);cursor:pointer;white-space:nowrap}
.msg{max-width:860px;margin:0 auto 12px;padding:12px 16px;border-radius:12px;background:var(--card);border:1px solid var(--line);white-space:pre-wrap;overflow-wrap:anywhere}
.msg.user{background:var(--u)}.role{font-size:12px;font-weight:700;color:var(--acc);margin-bottom:4px}
pre{background:rgba(127,127,127,.12);padding:10px;border-radius:8px;overflow:auto;white-space:pre;margin:6px 0}
@media (max-width:760px){body{grid-template-columns:1fr;grid-template-rows:42vh 1fr}aside{border-right:0;border-bottom:1px solid var(--line)}main{padding:16px}}
</style></head><body>
<aside><div class="top"><input id="q" placeholder="Search titles and messages" autofocus>
<select id="src"><option value="">All sources</option><option value="claude">Claude</option><option value="chatgpt">ChatGPT</option></select>
<div class="m" id="count"></div></div><div id="list"></div></aside>
<main id="view"><p class="m">Select a conversation.</p></main>
<script id="data" type="application/json">__DATA__</script>
<script>
const D=JSON.parse(document.getElementById('data').textContent);
D.forEach(c=>c._s=(c.title+'\n'+c.messages.map(m=>m.text).join('\n')).toLowerCase());
const esc=s=>String(s).replace(/[&<>"]/g,ch=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;'}[ch]));
const fmt=t=>t.split('```').map((p,i)=>i%2?'<pre>'+esc(p.replace(/^[^\n]*\n/,''))+'</pre>':esc(p)).join('');
const lab=s=>s==='claude'?'Claude':'ChatGPT';let cur=null;
function pack(c,max=60000){const h='# Context pack: '+c.title+'\n\n_Source: '+lab(c.source)+' chat · '+c.messages.length+' messages_\n\n> **Note to the assistant:** Below is the transcript of an earlier conversation I had with an AI assistant on another device. Treat it as shared context and continue from where it left off. Don\'t summarize it back to me unless I ask.\n\n---\n\n';
const b=c.messages.map(m=>'**'+(m.role==='user'?'User':'Assistant')+'**:\n\n'+m.text+'\n\n');let body=b.join('');
if(h.length+body.length>max){const f=b[0].slice(0,max/4);let r=max-h.length-f.length-120,t=[];for(let i=b.length-1;i>0&&b[i].length<=r;i--){t.unshift(b[i]);r-=b[i].length}
body=f+'\n_[... '+(b.length-1-t.length)+' earlier message(s) omitted for length ...]_\n\n'+t.join('')}
return h+body+'---\n\n_End of prior transcript. My next message follows._\n'}
function show(c){const v=document.getElementById('view');v.innerHTML='<div class="head"><div><h1>'+esc(c.title)+'</h1><div class="m">'+lab(c.source)+' · '+esc(c.created)+' → '+esc(c.updated)+' · '+c.messages.length+' messages</div></div><button id="cp">Copy as context pack</button></div>'+
c.messages.map(m=>'<div class="msg '+m.role+'"><div class="role">'+(m.role==='user'?'You':'Assistant')+(m.time?' · '+esc(m.time):'')+'</div>'+fmt(m.text)+'</div>').join('');
document.getElementById('cp').onclick=e=>{navigator.clipboard.writeText(pack(c)).then(()=>e.target.textContent='Copied ✓',()=>e.target.textContent='Copy failed')};v.scrollTop=0}
function render(){const q=document.getElementById('q').value.toLowerCase().trim(),s=document.getElementById('src').value;
const r=D.filter(c=>(!s||c.source===s)&&(!q||q.split(/\s+/).every(w=>c._s.includes(w))));
document.getElementById('count').textContent=r.length+' of '+D.length+' conversations';const L=document.getElementById('list');L.innerHTML='';
r.slice(0,3000).forEach(c=>{const d=document.createElement('div');d.className='item'+(c===cur?' on':'');
d.innerHTML='<div class="t">'+esc(c.title)+'</div><div class="m">'+lab(c.source)+' · '+esc(c.updated||c.created)+' · '+c.messages.length+' msgs</div>';
d.onclick=()=>{cur=c;show(c);render()};L.appendChild(d)})}
document.getElementById('q').oninput=render;document.getElementById('src').onchange=render;render();
</script></body></html>"""

def hold_window():
    """A double-clicked EXE closes its window on exit: keep it open so the message can be read."""
    if FROZEN and len(sys.argv) == 1 and sys.stdin.isatty():
        try:
            input("\nPress Enter to close.")
        except (EOFError, KeyboardInterrupt):
            pass


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        print("\nCancelled.")
        sys.exit(130)
    except SystemExit as ex:
        if ex.code not in (0, None):
            if isinstance(ex.code, str):
                print(ex.code)
            hold_window()
            sys.exit(1)
        raise
    except Exception:
        import traceback
        traceback.print_exc()
        hold_window()
        sys.exit(1)
