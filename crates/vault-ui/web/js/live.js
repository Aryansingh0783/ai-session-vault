/* Live updates: the header, sidebar and engine card follow the sync engine's state. */

import { eventsUrl, friendlyError, isStatic } from "./api.js";
import { S, hooks, loadCore, phase, phaseText, route } from "./state.js";
import { loader, toast } from "./ui.js";
import { $, $$, ago, ico, num, plural } from "./util.js";

const HEADS = {
  devices: ["Devices", "Computers that share this vault"],
  archive: ["Archive", "Search every session and imported chat on this device"],
  health: ["Health", "Checks, integrity and cleanup"],
  settings: ["Settings", "What gets synced, and what never leaves a device"],
};

function pageHead(user) {
  const ov = S.overview;
  if (route() === "projects") return ["Projects", ov ? `${plural(ov.totals.projects, "project")} tracked across ${plural(ov.totals.devices, "device")}` : ""];
  if (HEADS[route()]) return HEADS[route()];
  const st = S.status || {};
  const last = st.status && st.status.last_sync ? ago(st.status.last_sync) : "none yet";
  return [user ? `Hi, ${user}!` : "Hi!", ov ? `${plural(ov.totals.devices, "device")} · ${plural(ov.totals.sessions, "session")} · last sync ${last}` : ""];
}

function updateHeader() {
  const user = (S.settings && S.settings.user) || "";
  const me = S.overview && S.overview.devices.find((d) => d.this_device);
  const [title, sub] = pageHead(user);
  $("#greeting").textContent = title;
  $("#subline").textContent = sub;
  $("#avatar").textContent = (user || (me && me.name) || "V").charAt(0).toUpperCase();
}

function updateSidebar(ph, syncing) {
  const me = S.overview && S.overview.devices.find((d) => d.this_device);
  $("#side-dot").className = "team-dot" + (syncing ? " is-busy" : ph === "idle" ? " is-live" : "");
  $("#side-device-name").textContent = (me && me.name) || "This device";
  $("#side-device-state").textContent = phaseText();
  const tools = S.settings && S.settings.tools;
  if (!tools) return;
  $$(".side-list li").forEach((li) => {
    const on = tools[li.dataset.tool];
    li.classList.toggle("is-off", !on);
    li.title = on ? "Found on this device" : "Not found on this device";
    const note = $("small", li);
    if (note) note.textContent = on ? "" : "n/a";
  });
}

function updateSyncButton(st, syncing) {
  const btn = $("#btn-sync");
  btn.disabled = !st.engine || syncing;
  btn.title = st.engine ? "" : "The sync engine isn't running in this session";
  btn.innerHTML = syncing ? `${loader("circle")}<span>Syncing</span>` : `${ico("refresh", "ids-icon--xs")}<span>Sync now</span>`;
}

function updateEngineCard(st, ph, syncing) {
  const title = $("#eng-title");
  if (!title) return;
  title.textContent = syncing ? "Syncing your sessions" : phaseText();
  $("#eng-loader").innerHTML = syncing ? loader("circle") : ph === "waiting" || ph === "backoff" ? loader("dots") : "";
  const last = st.status && st.status.last_sync;
  $("#eng-meta").textContent = last ? `Last sync ${ago(last)}` : "No sync yet";
  const pause = $("#eng-pause");
  if (pause) {
    pause.innerHTML = ico(ph === "paused" ? "play" : "pause");
    pause.setAttribute("aria-label", ph === "paused" ? "Resume engine" : "Pause engine");
  }
  const bar = $("#prog-bar");
  if (!bar) return;
  const pr = st.progress;
  bar.style.width = (pr && pr.total ? Math.min(100, Math.round((pr.done / pr.total) * 100)) : syncing ? 4 : 0) + "%";
  $("#prog-text").textContent = syncing && pr
    ? `${pr.phase === "restore" ? "Checking vault" : "Uploading"} · ${num(pr.done)} / ${num(pr.total)}`
    : st.status ? plural(st.status.pending_changes, "change") + " waiting" : "Nothing waiting";
}

export function updateLive() {
  const st = S.status || {};
  const ph = phase();
  const syncing = ph === "syncing";
  updateHeader();
  updateSidebar(ph, syncing);
  updateSyncButton(st, syncing);
  updateEngineCard(st, ph, syncing);
  const lastItem = S.history[S.history.length - 1];
  const problem = st.last_error && (!st.status || st.status.phase === "backoff");
  $("#bell-dot").hidden = !problem && !(lastItem && lastItem.conflicts);
}

/* ---- connection health: tell the user once if the service stops answering ---- */
let lastOfflineNotice = 0;
export function noteUnreachable(e) {
  console.warn("vault service unreachable:", e);
  if (Date.now() - lastOfflineNotice < 60000) return;
  lastOfflineNotice = Date.now();
  toast(friendlyError(e));
}

let softTimer;
function refreshSoon() {
  clearTimeout(softTimer);
  softTimer = setTimeout(async () => {
    try {
      await loadCore();
      updateLive();
      if (["dashboard", "projects", "devices"].includes(route())) hooks.render({ keepScroll: true });
    } catch (e) { noteUnreachable(e); }
  }, 400);
}

function onEvent(ev) {
  S.status = S.status || { engine: true };
  if (ev.event === "status") S.status.status = ev;
  else if (ev.event === "progress") S.status.progress = { phase: ev.phase, done: ev.done, total: ev.total };
  else if (ev.event === "error") { S.status.last_error = ev.message; toast("Sync problem: " + ev.message); }
  else if (ev.event === "sync_finished") {
    S.status.progress = null;
    const r = ev.report;
    if (r.uploaded || r.downloaded || r.merged) toast(`Synced · ${r.uploaded} up, ${r.downloaded} down${r.merged ? ", " + r.merged + " merged" : ""}`);
    refreshSoon();
  }
  updateLive();
}

/** Start the live stream and periodic refresh (not in ?static mode). */
export function startLive() {
  if (isStatic) return;
  let source;
  try { source = new EventSource(eventsUrl()); } catch (e) { noteUnreachable(e); return; }
  source.onmessage = (m) => {
    try { onEvent(JSON.parse(m.data)); } catch (e) { console.warn("ignored a malformed event", e); }
  };
  setInterval(() => { if (S.status && S.status.engine) updateLive(); }, 30000);
  setInterval(async () => {
    try { await loadCore(); updateLive(); } catch (e) { noteUnreachable(e); }
  }, 60000);
}
