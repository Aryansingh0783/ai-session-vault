/* First-run setup: create a vault, connect to one, or unlock one. Talks to /api/setup/*. */

import { api, friendlyError } from "./api.js";
import { $, $$, esc } from "./util.js";

const card = $("#card");
const view = { status: null, folder: "", recovery: "" };

const logo = `<img class="setup__logo" src="assets/logo.png" alt="" width="52" height="52">`;
const errorBox = () => `<div class="setup__error" id="err" role="alert" hidden></div>`;

function showError(msg) {
  const el = $("#err");
  el.textContent = msg;
  el.hidden = !msg;
}

/** Run an action with the submit button disabled; show any failure inside the form. */
async function submit(btn, label, run) {
  btn.disabled = true;
  const text = btn.textContent;
  btn.textContent = label;
  showError("");
  try {
    await run();
  } catch (e) {
    showError(friendlyError(e));
    btn.disabled = false;
    btn.textContent = text;
  }
}

function chips(list, targetId) {
  if (!list.length) return "";
  return `<div class="chips" role="group" aria-label="Suggested folders">${list.map((s) =>
    `<button type="button" class="chip" data-path="${esc(s.path)}" data-target="${targetId}">${esc(s.label)}${s.has_vault ? " · has a vault" : ""}</button>`).join("")}</div>`;
}

function wireChips() {
  $$(".chip").forEach((c) => c.addEventListener("click", () => {
    const input = $("#" + c.dataset.target);
    input.value = c.dataset.path;
    input.focus();
  }));
}

/* ---- screens ---- */
function welcome() {
  card.innerHTML = `${logo}<h1>Welcome to AI Session Vault</h1>
    <p>It keeps your Claude Code, Codex and Claude Desktop sessions in one encrypted folder, so every computer you use has the same history. Nothing leaves your own folders.</p>
    ${view.status.reason ? `<div class="setup__error mt-10" role="alert">${esc(view.status.reason)}</div>` : ""}
    <div class="setup__choices">
      <button class="choice" data-go="create"><b>This is my first computer</b><span>Create a new vault and protect it with a passphrase.</span></button>
      <button class="choice" data-go="connect"><b>I already have a vault</b><span>Connect this computer to the vault you made on another one.</span></button>
    </div>`;
  $$("[data-go]").forEach((b) => b.addEventListener("click", () => ({ create, connect })[b.dataset.go]()));
}

function create() {
  const first = view.status.suggestions.find((s) => !s.has_vault);
  card.innerHTML = `${logo}<h1>Create your vault</h1>
    <p>Pick a folder that your cloud drive already syncs (OneDrive, Dropbox, Google Drive) so your other computers can find it. Everything in it is encrypted.</p>
    <form class="setup__form" id="f" novalidate>
      <label>Vault folder<input class="input" id="folder" autocomplete="off" spellcheck="false" value="${esc(first ? first.path : "")}" placeholder="C:\\Users\\you\\OneDrive\\AI Session Vault">${chips(view.status.suggestions.filter((s) => !s.has_vault), "folder")}</label>
      <label>Passphrase <span class="hint">At least 12 characters. Use the same one on every computer.</span><input class="input" id="pass" type="password" autocomplete="new-password"></label>
      <label>Repeat passphrase<input class="input" id="pass2" type="password" autocomplete="new-password"></label>
      <label class="setup__check"><input type="checkbox" id="remember" checked><span>Remember it on this computer, in the Windows credential store, so the app opens without asking.</span></label>
      ${errorBox()}
      <div class="setup__actions"><button type="button" class="ids-btn ids-btn--ghost" id="back">Back</button><button class="ids-btn ids-btn--dark" id="go">Create vault</button></div>
    </form>`;
  wireChips();
  $("#back").addEventListener("click", welcome);
  $("#folder").focus();
  $("#f").addEventListener("submit", (e) => {
    e.preventDefault();
    if ($("#pass").value !== $("#pass2").value) return showError("The two passphrases don't match.");
    submit($("#go"), "Creating…", async () => {
      const r = await api("/api/setup/create", { method: "POST", body: { folder: $("#folder").value, passphrase: $("#pass").value, remember: $("#remember").checked } });
      view.recovery = r.recovery_key;
      recovery();
    });
  });
}

function recovery() {
  card.innerHTML = `${logo}<h1>Save your recovery key</h1>
    <p>This is the only way back in if you forget the passphrase. It is shown once. Copy it into a password manager or write it down.</p>
    <div class="recovery" id="key">${esc(view.recovery)}</div>
    <div class="setup__form">
      <button type="button" class="ids-btn ids-btn--ghost" id="copy">Copy recovery key</button>
      <label class="setup__check"><input type="checkbox" id="saved"><span>I have saved the recovery key somewhere safe.</span></label>
      ${errorBox()}
      <div class="setup__actions"><span></span><button class="ids-btn ids-btn--dark" id="go" disabled>Open my vault</button></div>
    </div>`;
  $("#copy").addEventListener("click", async () => {
    try { await navigator.clipboard.writeText(view.recovery); $("#copy").textContent = "Copied"; } catch (_) { $("#copy").textContent = "Select the key above and copy it"; }
  });
  $("#saved").addEventListener("change", (e) => { $("#go").disabled = !e.target.checked; });
  $("#go").addEventListener("click", () => submit($("#go"), "Opening…", finish));
}

function connect(mode = "passphrase") {
  const found = view.status.suggestions;
  card.innerHTML = `${logo}<h1>Connect to your vault</h1>
    <p>Choose the folder that holds the vault. If it lives in a cloud drive, wait until that folder has finished syncing to this computer.</p>
    <form class="setup__form" id="f" novalidate>
      <label>Vault folder<input class="input" id="folder" autocomplete="off" spellcheck="false" value="${esc(view.folder || (found.find((s) => s.has_vault) || {}).path || "")}" placeholder="C:\\Users\\you\\OneDrive\\AI Session Vault">${chips(found.filter((s) => s.has_vault), "folder")}</label>
      <div class="tabs" role="tablist"><button type="button" class="tab" role="tab" data-mode="passphrase" aria-selected="${mode === "passphrase"}">Passphrase</button><button type="button" class="tab" role="tab" data-mode="code" aria-selected="${mode === "code"}">Code from another computer</button></div>
      ${mode === "passphrase"
        ? `<label>Passphrase or recovery key<input class="input" id="secret" type="password" autocomplete="current-password"></label>
           <label class="setup__check"><input type="checkbox" id="remember" checked><span>Remember it on this computer, in the Windows credential store.</span></label>`
        : `<label>One-time code <span class="hint">On your other computer open Devices and choose Add a device. The code works once and expires in 15 minutes.</span><input class="input" id="secret" autocomplete="off" spellcheck="false" placeholder="XXXX-XXXX-XXXX"></label>`}
      ${errorBox()}
      <div class="setup__actions"><button type="button" class="ids-btn ids-btn--ghost" id="back">Back</button><button class="ids-btn ids-btn--dark" id="go">Connect</button></div>
    </form>`;
  wireChips();
  $$(".tab").forEach((t) => t.addEventListener("click", () => { view.folder = $("#folder").value; connect(t.dataset.mode); }));
  $("#back").addEventListener("click", welcome);
  $("#folder").addEventListener("change", checkFolder);
  $("#f").addEventListener("submit", (e) => {
    e.preventDefault();
    const folder = $("#folder").value;
    submit($("#go"), "Connecting…", async () => {
      if (mode === "code") await api("/api/setup/join", { method: "POST", body: { folder, code: $("#secret").value } });
      else await api("/api/setup/unlock", { method: "POST", body: { folder, passphrase: $("#secret").value, remember: $("#remember").checked } });
      await finish();
    });
  });
  $("#secret").focus();
}

/** Tell the user right away when the folder has no vault in it. */
async function checkFolder() {
  const v = $("#folder").value.trim();
  if (!v) return;
  try {
    const r = await api("/api/setup/inspect?folder=" + encodeURIComponent(v));
    showError(r.has_vault ? "" : "There is no vault in that folder yet. If it is still syncing, wait for it to finish.");
  } catch (e) { showError(friendlyError(e)); }
}

/** This computer already points at a vault but cannot open it (stored passphrase gone or wrong). */
function unlock() {
  const folder = view.status.folder;
  card.innerHTML = `${logo}<h1>Unlock your vault</h1>
    <p>${esc(view.status.reason || "Enter the passphrase to open the vault on this computer.")}</p>
    <p class="small mono">${esc(folder)}</p>
    <form class="setup__form" id="f" novalidate>
      <label>Passphrase or recovery key<input class="input" id="secret" type="password" autocomplete="current-password"></label>
      <label class="setup__check"><input type="checkbox" id="remember" checked><span>Remember it on this computer, in the Windows credential store.</span></label>
      ${errorBox()}
      <div class="setup__actions"><button type="button" class="ids-btn ids-btn--ghost" id="other">Use a different vault</button><button class="ids-btn ids-btn--dark" id="go">Unlock</button></div>
    </form>`;
  $("#other").addEventListener("click", welcome);
  $("#secret").focus();
  $("#f").addEventListener("submit", (e) => {
    e.preventDefault();
    submit($("#go"), "Unlocking…", async () => {
      await api("/api/setup/unlock", { method: "POST", body: { folder, passphrase: $("#secret").value, remember: $("#remember").checked } });
      await finish();
    });
  });
}

async function finish() {
  card.innerHTML = `${logo}<h1>All set</h1><p>Opening your vault…</p><div class="loading" role="status"><span class="ld ld-circle ld--lg" aria-label="Loading"></span></div>`;
  await api("/api/setup/finish", { method: "POST" });
}

async function boot() {
  try {
    view.status = await api("/api/setup");
    if (view.status.folder) unlock(); else welcome();
  } catch (e) {
    card.innerHTML = `${logo}<h1>Can't start setup</h1><p>${esc(friendlyError(e))}</p><div class="setup__actions"><span></span><button class="ids-btn ids-btn--dark" id="retry">Try again</button></div>`;
    $("#retry").addEventListener("click", boot);
  }
}

boot();
