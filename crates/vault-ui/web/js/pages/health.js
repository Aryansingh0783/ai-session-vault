/* Health: doctor checks, vault integrity and cleanup. */

import { api } from "../api.js";
import { checkRows } from "../cards.js";
import { R, ensure, hooks, loadCore } from "../state.js";
import { busy, loader, toast } from "../ui.js";
import { $, esc, fmtBytes, num } from "../util.js";
import { updateLive } from "../live.js";

let verifyOut = "";

export function healthPage() {
  return `<div class="grid"><section class="card ids-card c7"><header class="card__head"><h2 class="card__title">Checks</h2>
      <button class="ids-btn ids-btn--dark ids-btn--sm" data-action="deep-scan">Scan for leaked secrets</button></header>${checkRows(true)}</section>
    <section class="card ids-card c5"><header class="card__head"><h2 class="card__title">Vault integrity</h2></header>
      <p class="small muted mt-8">Checks that every file the devices list is present. A deep check also decrypts and verifies each one.</p>
      <div class="inline"><button class="ids-btn ids-btn--ghost" data-action="verify">Quick check</button><button class="ids-btn ids-btn--dark" data-action="verify-deep">Deep check</button></div>
      <div id="verify-out" class="small mt-14" role="status">${verifyOut}</div>
      <hr class="ids-hr divider"><p class="small muted">Remove data no device references any more.</p>
      <div class="inline"><button class="ids-btn ids-btn--ghost" data-action="gc">Clean up unused data</button></div></section></div>`;
}

export const healthAfter = () => ensure("doctor");

function verifySummary(report, deep) {
  const bad = report.missing.length + report.corrupt.length;
  if (!bad) return { bad, html: `Healthy · ${num(report.objects)} objects checked${deep ? " and decrypted" : ""}.` };
  const lines = [...report.missing.map((m) => "Missing: " + m), ...report.corrupt.map((m) => "Corrupt: " + m)].slice(0, 6).map(esc).join("<br>");
  return { bad, html: `<b>${bad} problem(s)</b> in ${num(report.objects)} objects.<br>${lines}<br><span class="muted">Missing files usually mean the cloud folder is still syncing.</span>` };
}

export async function runVerify(btn, deep) {
  const out = $("#verify-out");
  if (out) out.innerHTML = `${loader("fade")} Checking…`;
  const done = await busy(btn, async () => {
    const r = await api("/api/verify", { method: "POST", body: { deep } });
    const s = verifySummary(r.report, deep);
    verifyOut = s.html;
    if (out) out.innerHTML = verifyOut; else toast(s.bad ? `${s.bad} problem(s) found. See Health.` : "Vault is healthy");
    return true;
  });
  if (!done && out) out.innerHTML = verifyOut; // the failure toast already explained why
}

export async function runGc(btn) {
  await busy(btn, async () => {
    const r = await api("/api/gc", { method: "POST" });
    toast(r.removed ? `Removed ${r.removed} unused object(s), freed ${fmtBytes(r.freed_bytes)}` : "Nothing to clean up");
    await loadCore();
    updateLive();
  });
}

export async function runDeepScan(btn) {
  await busy(btn, async () => {
    btn.innerHTML = `${loader("circle")} Scanning…`;
    R.doctor.data = await api("/api/doctor?deep=1");
    R.doctor.state = "ready";
    toast("Scan finished");
    hooks.render({ keepScroll: true });
  });
  if (btn.isConnected) btn.textContent = "Scan for leaked secrets";
}

