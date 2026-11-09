/* Reusable cards: health checks, storage, project cards. */

import { R, S } from "./state.js";
import { errorState, skeletonRows } from "./ui.js";
import { ago, baseName, esc, firstSentence, fmtBytes, ico, plural } from "./util.js";

/* ---- health checks (dashboard card and Health page share the row markup) ---- */
export function checkRow(x, full) {
  const detail = x.detail && (full || x.level !== "Ok") ? `<small>${esc(full ? x.detail : firstSentence(x.detail))}</small>` : "";
  return `<div class="chk__row is-${x.level.toLowerCase()}"><span class="ids-check ${x.level === "Ok" ? "is-checked" : ""}">${ico("check")}</span><div>${esc(x.title)}${detail}</div></div>`;
}

export function checkRows(full) {
  const r = R.doctor;
  if (r.state === "error") return errorState(r.error, { action: "retry", attrs: { res: "doctor" } }, "Couldn't run the checks");
  if (r.state !== "ready") return skeletonRows(5);
  return `<div class="chk__rows">${r.data.map((x) => checkRow(x, full)).join("")}</div>`;
}

export function healthCard() {
  const items = R.doctor.state === "ready" ? R.doctor.data : [];
  const ok = items.filter((x) => x.level === "Ok").length;
  const scored = items.filter((x) => x.level !== "Info").length;
  const C = 2 * Math.PI * 17;
  const ring = items.length
    ? `<span class="ring" aria-label="${ok} of ${scored} checks passed"><svg viewBox="0 0 40 40"><circle class="t" cx="20" cy="20" r="17"/><circle class="v" cx="20" cy="20" r="17" stroke-dasharray="${scored ? (ok / scored) * C : C} ${C}"/></svg>${ok}/${scored}</span>`
    : "";
  return `<section class="card ids-card ids-card--panel chk"><header class="card__head"><h2 class="card__title">Health checks</h2>${ring}</header>${checkRows(false)}</section>`;
}

/* ---- storage ---- */
export function storageCard() {
  const o = S.overview;
  const raw = o.totals.unique_bytes || o.totals.bytes;
  const disk = o.disk_bytes || 0;
  const saved = raw > 0 ? Math.max(0, Math.min(100, Math.round((1 - disk / raw) * 100))) : 0;
  const C = 2 * Math.PI * 33;
  const devices = Math.min(1, o.totals.devices / 4) * 138;
  return `<section class="card ids-card sto">
    <header class="card__head"><h2 class="card__title">Storage</h2><span class="tool">${ico("bars")}</span></header>
    <div class="sto__body">
      <div class="ids-gauge"><svg viewBox="0 0 93 93" aria-hidden="true">
        <circle class="ids-gauge__ring g-outer" cx="46.5" cy="46.5" r="44.5" stroke-width="3.6"/>
        <circle class="ids-gauge__ring g-track" cx="46.5" cy="46.5" r="33" stroke-width="3"/>
        <circle class="ids-gauge__ring g-val" cx="46.5" cy="46.5" r="33" stroke-width="3" stroke-dasharray="${(saved / 100) * C} ${C}" transform="rotate(-90 46.5 46.5)"/>
        <circle class="ids-gauge__ring g-track" cx="46.5" cy="46.5" r="22" stroke-width="2.4"/>
        <circle class="ids-gauge__ring g-val2" cx="46.5" cy="46.5" r="22" stroke-width="2.4" stroke-dasharray="${devices} 200" transform="rotate(-90 46.5 46.5)"/></svg>
        <div class="ids-gauge__center"><div class="ids-gauge__value">${saved}%</div><div class="ids-gauge__label">saved</div></div></div>
      <div class="sto__legend">
        <div><span class="ids-dot"></span>On disk <b>${fmtBytes(disk)}</b></div>
        <div><span class="ids-dot ids-dot--muted"></span>Original <b>${fmtBytes(raw)}</b></div>
        <div><span class="ids-dot ids-dot--faint"></span>${plural(o.totals.files, "file")} · encrypted</div></div>
    </div>
    <div class="sto__foot"><button class="ids-iconbtn ids-iconbtn--dark" data-action="copy-vault" aria-label="Copy vault folder path">${ico("copy")}</button>
      <button class="ids-btn ids-btn--ghost" data-action="verify">Verify vault ${ico("download", "ids-icon--sm")}</button></div>
  </section>`;
}

/* ---- project cards ---- */
function projectStatus(p) {
  if (p.exists_locally) return "On this device";
  return p.portable_path.startsWith("raw:") ? "Notes only" : "Folder not on this device";
}

export function projectCards(list, limit) {
  const total = S.overview.totals.devices || 1;
  const C = 2 * Math.PI * 14;
  return list.slice(0, limit).map((p) => {
    const name = baseName(p.portable_path);
    const dim = p.exists_locally || p.portable_path.startsWith("raw:") ? "" : "pcard--dim";
    return `<article class="pcard ${dim}" data-action="open-project" data-q="${esc(name)}" tabindex="0" role="link" aria-label="Search sessions in ${esc(name)}">
      <header class="pcard__head"><h3 class="pcard__title" title="${esc(p.portable_path)}">${esc(name)}</h3>
        <span class="ring" title="${p.devices.length} of ${total} devices have this project"><svg viewBox="0 0 34 34"><circle class="t" cx="17" cy="17" r="14"/><circle class="v" cx="17" cy="17" r="14" stroke-dasharray="${(p.devices.length / total) * C} ${C}"/></svg>${p.devices.length}/${total}</span></header>
      <div class="pcard__meta"><span class="ids-dot"></span>${projectStatus(p)}</div>
      <p class="pcard__path"><b>${plural(p.sessions, "session")}</b> · ${fmtBytes(p.bytes)} · active ${ago(p.last_activity_ms)}<br>${esc(p.portable_path)}</p></article>`;
  }).join("");
}

