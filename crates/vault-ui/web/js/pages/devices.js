/* Devices: computers sharing the vault, and passphrase-free access. */

import { R, ensure } from "../state.js";
import { emptyState, errorState, skeletonGrid } from "../ui.js";
import { ago, esc, ico, num } from "../util.js";

function deviceCard(x) {
  const forget = x.this_device ? "" : `<button class="ids-btn ids-btn--ghost ids-btn--sm" data-action="forget-device" data-id="${esc(x.id)}">Forget this device</button>`;
  return `<article class="dcard ${x.this_device ? "is-me" : ""}">
    <div class="dcard__top"><h3>${esc(x.name)}</h3>${x.this_device ? `<span class="chip">This device</span>` : ""}</div>
    <dl class="kv"><dt>System</dt><dd>${esc(x.os)}</dd><dt>Last sync</dt><dd>${ago(x.updated)}</dd><dt>Files</dt><dd>${num(x.files)}</dd><dt>Id</dt><dd class="mono">${esc(x.id.slice(0, 8))}</dd></dl>${forget}</article>`;
}

function joinedRow(j) {
  return `<div class="row row--joined"><div><b>${esc(j.name || "Device")}</b><div class="small">joined ${ago(j.created)}</div></div><span class="mono muted">${esc(j.id)}</span>
    <button class="ids-btn ids-btn--ghost ids-btn--sm" data-action="revoke" data-id="${esc(j.id)}">Revoke</button></div>`;
}

export function devicesPage() {
  const r = R.devices;
  if (r.state === "error") return errorState(r.error, { action: "retry", attrs: { res: "devices" } }, "Couldn't load the devices");
  if (r.state !== "ready") return skeletonGrid(3);
  const d = r.data;
  const joined = d.joined.length
    ? `<div class="list">${d.joined.map(joinedRow).join("")}</div>`
    : emptyState("No passphrase-free devices yet", "Add a device to get a one-time code. It joins without ever seeing the passphrase.", { label: "Add a device", action: "add-device" });
  return `<div class="dgrid">${d.devices.map(deviceCard).join("")}<button class="addcard minh-200" data-action="add-device">${ico("plus")}<span>Add a device</span></button></div>
    <div class="section-head"><h2>Passphrase-free access</h2></div>${joined}
    ${d.warnings.length ? `<p class="small muted mt-14">${d.warnings.map(esc).join("<br>")}</p>` : ""}`;
}

export const devicesAfter = () => ensure("devices");
