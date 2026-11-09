/* Dialogs and the recent-activity popover. */

import { api, friendlyError } from "./api.js";
import { S } from "./state.js";
import { errorState, loader } from "./ui.js";
import { $, ago, esc, ico, num } from "./util.js";

export function modal(html) {
  const root = $("#modal-root");
  root.innerHTML = `<div class="modal-bg" data-action="close-modal"><div class="modal" role="dialog" aria-modal="true">${html}</div></div>`;
  const first = $("button, input", root);
  if (first) first.focus();
}

export const closeModal = () => { $("#modal-root").innerHTML = ""; };

/* ---- add a device ---- */
export async function enrollModal() {
  modal(`<h2>Add a device</h2><div class="loading" role="status">${loader("fade")}</div>`);
  try {
    const r = await api("/api/enroll", { method: "POST", body: { minutes: 15 } });
    modal(`<h2>Add a device</h2><p>On the other computer, install AI Session Vault, point it at this vault's folder and run:</p>
      <div class="code" id="enroll-code">${esc(r.code)}</div><p class="small muted mono">avault join --vault "&lt;vault folder&gt;" --code ${esc(r.code)}</p>
      <p class="small muted mt-10">Works once and expires in ${r.expires_in_minutes} minutes. The passphrase is never shared.</p>
      <div class="modal__actions"><button class="ids-btn ids-btn--ghost" data-action="close-modal">Done</button><button class="ids-btn ids-btn--dark" data-action="copy-code">${ico("copy", "ids-icon--sm")} Copy code</button></div>`);
  } catch (e) {
    modal(`<h2>Add a device</h2>${errorState(friendlyError(e), { action: "add-device" }, "Couldn't create a code")}<div class="modal__actions"><button class="ids-btn ids-btn--ghost" data-action="close-modal">Close</button></div>`);
  }
}

/* ---- confirm before anything destructive ---- */
let pendingConfirm = null;

export function confirmModal({ title, body, label, run }) {
  pendingConfirm = run;
  modal(`<h2>${esc(title)}</h2><p>${esc(body)}</p>
    <div class="modal__actions"><button class="ids-btn ids-btn--ghost" data-action="close-modal">Cancel</button><button class="ids-btn ids-btn--dark" data-action="confirm-yes">${esc(label)}</button></div>`);
}

export async function confirmYes() {
  const run = pendingConfirm;
  pendingConfirm = null;
  closeModal();
  if (run) await run();
}

/* ---- recent activity ---- */
function activityRow(x) {
  const parts = [`${num(x.uploaded)} up`, `${num(x.downloaded)} down`];
  if (x.merged) parts.push(`${x.merged} merged`);
  if (x.conflicts) parts.push(`${x.conflicts} conflict(s)`);
  if (x.failed) parts.push(`${x.failed} failed`);
  return `<div class="pop-row"><b>${x.full ? "Full sync" : "Quick sync"}</b> · ${ago(x.at)}<br><span class="muted">${parts.join(" · ")}</span>${x.notes.map((n) => `<br><span class="small">${esc(n)}</span>`).join("")}</div>`;
}

export function activityPopover() {
  const existing = $(".popover");
  if (existing) { existing.remove(); return; }
  const recent = [...S.history].reverse().slice(0, 8);
  const problem = S.status && S.status.last_error;
  const el = document.createElement("div");
  el.className = "popover";
  el.setAttribute("role", "dialog");
  el.setAttribute("aria-label", "Recent activity");
  el.innerHTML = `<h3>Recent activity</h3>${problem ? `<div class="pop-row"><b>Last problem</b><br><span class="muted">${esc(problem)}</span></div>` : ""}${recent.length ? recent.map(activityRow).join("") : `<p class="muted small">No syncs have changed anything yet.</p>`}`;
  document.body.appendChild(el);
  setTimeout(() => document.addEventListener("click", function once(e) {
    if (el.contains(e.target) || e.target.closest("#btn-bell")) return;
    el.remove();
    document.removeEventListener("click", once);
  }), 0);
}

