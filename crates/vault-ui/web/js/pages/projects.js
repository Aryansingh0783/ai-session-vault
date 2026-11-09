/* Projects: filterable, sortable grid or list. */

import { projectCards } from "../cards.js";
import { S } from "../state.js";
import { emptyState } from "../ui.js";
import { $, baseName, esc, fmtBytes, ago, ico, num, prefs } from "../util.js";

let filter = "";
let sort = prefs.get("psort", "recent");
let view = prefs.get("pview", "grid");

const SORTERS = {
  recent: (a, b) => b.last_activity_ms - a.last_activity_ms,
  sessions: (a, b) => b.sessions - a.sessions,
  size: (a, b) => b.bytes - a.bytes,
  name: (a, b) => baseName(a.portable_path).localeCompare(baseName(b.portable_path)),
};

export function projectsPage() {
  const seg = (id, label, icon) => `<button data-view="${id}" class="${view === id ? "is-active" : ""}" aria-label="${label}">${ico(icon)}</button>`;
  return `<div class="toolbar"><input class="input input--grow" id="pq" placeholder="Filter projects" value="${esc(filter)}" aria-label="Filter projects">
    <select class="input select" id="psort" aria-label="Sort projects"><option value="recent">Most recent</option><option value="sessions">Most sessions</option><option value="name">Name</option><option value="size">Largest</option></select>
    <div class="seg">${seg("grid", "Grid view", "grid")}${seg("list", "List view", "list")}</div></div><div id="plist"></div>`;
}

function listRow(p) {
  return `<div class="row"><div class="row__title"><b>${esc(baseName(p.portable_path))}</b><div class="small row__title">${esc(p.portable_path)}</div></div>
    <div><b>${num(p.sessions)}</b><div class="small">sessions</div></div><div><b>${fmtBytes(p.bytes)}</b><div class="small">size</div></div><div><b>${ago(p.last_activity_ms)}</b><div class="small">active</div></div>
    <button class="ids-btn ids-btn--ghost ids-btn--sm" data-action="exclude" data-path="${esc(p.portable_path)}">Exclude</button></div>`;
}

export function renderProjectList() {
  const all = S.overview.projects;
  const needle = filter.toLowerCase();
  const list = all.filter((p) => (p.portable_path + " " + baseName(p.portable_path)).toLowerCase().includes(needle)).sort(SORTERS[sort] || SORTERS.recent);
  const el = $("#plist");
  if (!all.length) { el.innerHTML = emptyState("No projects yet", "Sync this device to add its Claude Code and Codex projects.", { label: "Sync now", action: "sync" }); return; }
  if (!list.length) { el.innerHTML = emptyState("No projects match", `Nothing matches "${filter}".`, { label: "Clear filter", action: "clear-project-filter" }); return; }
  el.innerHTML = view === "grid" ? `<div class="pgrid">${projectCards(list, 500)}</div>` : `<div class="list">${list.map(listRow).join("")}</div>`;
}

export function projectsAfter() {
  $("#psort").value = sort;
  renderProjectList();
  $("#pq").addEventListener("input", (e) => { filter = e.target.value; renderProjectList(); });
  $("#psort").addEventListener("change", (e) => { sort = e.target.value; prefs.set("psort", sort); renderProjectList(); });
}

export function setProjectView(next) {
  view = next;
  prefs.set("pview", view);
  renderProjectList();
}

export function clearProjectFilter() {
  filter = "";
  const box = $("#pq");
  if (box) box.value = "";
  renderProjectList();
}
