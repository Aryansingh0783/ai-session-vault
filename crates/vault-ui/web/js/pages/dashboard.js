/* Dashboard: overview, activity chart, storage, health, engine and recent projects. */

import { healthCard, projectCards, storageCard } from "../cards.js";
import { S, ensure, phaseText } from "../state.js";
import { loader, skeletonCard, skeletonGrid } from "../ui.js";
import { ago, esc, ico, num, plural } from "../util.js";

function activityBuckets() {
  const today = new Date();
  today.setHours(0, 0, 0, 0);
  const days = [];
  for (let i = 6; i >= 0; i--) {
    const d = new Date(today);
    d.setDate(d.getDate() - i);
    days.push({ d, up: 0, down: 0 });
  }
  for (const h of S.history) {
    const t = new Date(Date.parse(h.at));
    if (isNaN(t)) continue;
    t.setHours(0, 0, 0, 0);
    const bucket = days.find((x) => x.d.getTime() === t.getTime());
    if (bucket) { bucket.up += h.uploaded; bucket.down += h.downloaded + h.merged; }
  }
  return days;
}

/** A smooth path through the points (Catmull-Rom converted to Bezier curves). */
function smooth(pts) {
  if (pts.length < 2) return "";
  let d = `M${pts[0][0]} ${pts[0][1]}`;
  for (let i = 0; i < pts.length - 1; i++) {
    const p0 = pts[i - 1] || pts[i], p1 = pts[i], p2 = pts[i + 1], p3 = pts[i + 2] || p2;
    const c1 = [p1[0] + (p2[0] - p0[0]) / 6, p1[1] + (p2[1] - p0[1]) / 6];
    const c2 = [p2[0] - (p3[0] - p1[0]) / 6, p2[1] - (p3[1] - p1[1]) / 6];
    d += ` C${c1[0].toFixed(1)} ${c1[1].toFixed(1)} ${c2[0].toFixed(1)} ${c2[1].toFixed(1)} ${p2[0]} ${p2[1]}`;
  }
  return d;
}

function activityCard() {
  const days = activityBuckets();
  const max = Math.max(1, ...days.map((d) => Math.max(d.up, d.down)));
  const W = 300, H = 110;
  const xy = (v, i) => [Math.round((i / 6) * W), Math.round(H - 8 - (v / max) * (H - 24))];
  const up = days.map((d, i) => xy(d.up, i));
  const down = days.map((d, i) => xy(d.down, i));
  const any = days.some((d) => d.up || d.down);
  const peak = days.reduce((m, d, i) => (d.up > days[m].up ? i : m), 0);
  const tip = any && days[peak].up ? `<div class="act__tip" style="left:${(peak / 6) * 100}%;top:${(up[peak][1] / H) * 100}%">${num(days[peak].up)} up</div>` : "";
  const names = ["S", "M", "T", "W", "T", "F", "S"];
  const lines = any
    ? `<path class="ids-chart__axis" d="M.5 0V${H - 0.5}H${W - 0.5}V0" vector-effect="non-scaling-stroke"/><path class="ids-chart__study" d="${smooth(down)}" vector-effect="non-scaling-stroke"/><path class="ids-chart__sport" d="${smooth(up)}" vector-effect="non-scaling-stroke"/>`
    : `<path class="act__idle" d="M0 ${H - 8}H${W}" vector-effect="non-scaling-stroke"/>`;
  const empty = any ? "" : `<div class="act__empty"><span>No sync activity yet.<br>It appears here after your first sync.<br><button class="ids-btn ids-btn--ghost ids-btn--sm mt-8" data-action="sync">Sync now</button></span></div>`;
  return `<section class="card ids-card ids-card--glass act">
    <header class="card__head"><h2 class="card__title">Sync activity</h2><span class="tool">${ico("wave")}</span></header>
    <div class="legend"><span class="ids-legend"><span class="ids-dot"></span>Uploaded</span><span class="ids-legend"><span class="ids-dot ids-dot--muted"></span>Received</span></div>
    <div class="act__chart"><svg viewBox="0 0 ${W} ${H}" preserveAspectRatio="none" aria-label="Sync activity over the last 7 days">${lines}</svg>${tip}${empty}</div>
    <div class="act__days ids-chart__days">${days.map((d, i) => `<span class="ids-chart__day ${i === 6 ? "is-active" : ""}" title="${esc(d.d.toDateString())}">${names[d.d.getDay()]}</span>`).join("")}</div>
  </section>`;
}

function engineCards() {
  const st = S.status || {};
  const last = S.history[S.history.length - 1];
  const lastBody = last
    ? `<div class="tcard__nums"><div><b>${num(last.uploaded)}</b><span>uploaded</span></div><div><b>${num(last.downloaded + last.merged)}</b><span>received</span></div></div>
       <div class="tcard__meta">${ago(last.at)}${last.conflicts ? ` · ${plural(last.conflicts, "conflict")}` : ""}</div>`
    : `<div class="tcard__meta tcard__meta--solo">Nothing transferred yet</div>`;
  return `<div class="tcard">
      <div class="tcard__top">${ico("wave")}<span id="eng-loader"></span></div>
      <div class="tcard__title" id="eng-title">${esc(phaseText())}</div>
      <div class="tcard__meta" id="eng-meta">&nbsp;</div>
      ${st.engine ? `<button class="tcard__corner" id="eng-pause" data-action="pause-toggle" aria-label="Pause engine">${ico("pause")}</button>` : ""}</div>
    <div class="tcard"><div class="tcard__top">${ico("target")}</div>
      <div class="tcard__title">Queue</div>
      <div class="progress"><span id="prog-bar"></span></div>
      <div class="tcard__meta" id="prog-text">&nbsp;</div></div>
    <div class="tcard tcard--dark"><div class="tcard__top">${ico("stats")}</div><div class="tcard__title">Last transfer</div>${lastBody}</div>
    <button class="addcard" data-action="add-device">${ico("plus")}<span>Add a device</span></button>`;
}

function overviewCard() {
  const o = S.overview, t = o.totals;
  const here = o.projects.filter((p) => p.exists_locally).length;
  const cover = o.projects.length ? Math.round((here / o.projects.length) * 100) : 0;
  return `<section class="card ids-card ids-card--dark c4 ov">
    <header class="card__head"><h2 class="card__title">Vault overview</h2><span class="tool">${ico("lock")}</span></header>
    <div class="ov__stats"><div class="ov__stat"><span class="ov__num">${num(t.sessions)}</span><span class="ov__lbl">sessions<br>in the vault</span></div><span class="ov__div"></span>
      <div class="ov__stat"><span class="ov__num">${num(t.projects)}</span><span class="ov__lbl">projects<br>tracked</span></div></div>
    <div class="ov__bar"><span style="width:${cover}%"></span></div>
    <div class="ov__cap">${here} of ${t.projects} project folders are on this device</div>
    <div class="mini-row"><div class="mini">${ico("target")}<b>${num(t.projects)}</b><span>Projects</span></div><div class="mini">${ico("laptop")}<b>${num(t.devices)}</b><span>Devices</span></div><div class="mini">${ico("spiral")}<b>${num(t.files)}</b><span>Files</span></div></div>
  </section>`;
}

export function dashboardSkeleton() {
  return `<div class="grid"><div class="c4">${skeletonCard()}</div><div class="c4">${skeletonCard()}</div><div class="c4">${skeletonCard()}</div></div>${skeletonGrid(3)}`;
}

export function dashboardPage() {
  const o = S.overview;
  const recent = o.projects.length
    ? `<div class="pgrid">${projectCards(o.projects, 6)}</div>`
    : `<div class="empty">${loader("ball")}<h3>Nothing in the vault yet</h3><p>Sync this device's sessions to get started.</p><button class="ids-btn ids-btn--dark" data-action="sync">Sync now</button></div>`;
  return `<div class="grid">${overviewCard()}<div class="c4">${activityCard()}</div><div class="c4">${storageCard()}</div><div class="c4">${healthCard()}</div>
    <div class="c8"><div class="section-head section-head--flush"><h2>Sync engine</h2><a class="small muted" href="#/health">Details ${ico("chev-r", "ids-icon--xs")}</a></div><div class="proc">${engineCards()}</div></div></div>
    <div class="section-head"><h2>Recent projects</h2><a class="small" href="#/projects">View all ${ico("chev-r", "ids-icon--xs")}</a></div>${recent}`;
}

export const dashboardAfter = () => ensure("doctor");
