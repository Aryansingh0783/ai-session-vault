/* AI Session Vault - front end. No dependencies; talks to the local Rust core over /api. */
(function () {
  "use strict";
  const $ = (s, r = document) => r.querySelector(s);
  const $$ = (s, r = document) => Array.from(r.querySelectorAll(s));
  const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
  const ico = (n, cls = "") => `<svg class="ids-icon ${cls}" aria-hidden="true"><use href="#i-${n}"/></svg>`;
  const loader = (kind = "circle", cls = "") => `<span class="ld ld-${kind} ${cls}" role="img" aria-label="Loading">${kind === "dots" ? "<i></i>" : ""}</span>`;

  /* ------------------------------------------------------------ token + api */
  const params = new URLSearchParams(location.search);
  let TOKEN = params.get("t") || "";
  try {
    if (TOKEN) sessionStorage.setItem("avault_t", TOKEN);
    else TOKEN = sessionStorage.getItem("avault_t") || "";
  } catch (_) {}
  if (params.get("t")) history.replaceState(null, "", location.pathname + location.hash);

  async function api(path, opts = {}) {
    const r = await fetch(path, {
      method: opts.method || "GET",
      headers: { "X-Token": TOKEN, "Content-Type": "application/json" },
      body: opts.body ? JSON.stringify(opts.body) : undefined,
    });
    if (!r.ok) {
      let m = r.statusText;
      try { m = (await r.json()).error || m; } catch (_) {}
      throw new Error(m);
    }
    return (r.headers.get("content-type") || "").includes("json") ? r.json() : r.text();
  }

  /* ------------------------------------------------------------ helpers */
  const fmtBytes = (n) => {
    n = Number(n) || 0;
    if (n < 1024) return n + " B";
    const u = ["KB", "MB", "GB", "TB"];
    let i = -1;
    do { n /= 1024; i++; } while (n >= 1024 && i < u.length - 1);
    return (n >= 100 ? n.toFixed(0) : n.toFixed(1)) + " " + u[i];
  };
  const num = (n) => Number(n || 0).toLocaleString();
  const ago = (t) => {
    const ms = typeof t === "number" ? t : Date.parse(t);
    if (!ms) return "never";
    const s = Math.max(0, (Date.now() - ms) / 1000);
    if (s < 45) return "just now";
    if (s < 3600) return Math.round(s / 60) + " min ago";
    if (s < 86400) return Math.round(s / 3600) + " h ago";
    return Math.round(s / 86400) + " d ago";
  };
  const baseName = (p) => {
    if (!p) return "Unknown";
    if (p.startsWith("raw:")) return p.slice(4).split("-").filter(Boolean).slice(-2).join("-") || p.slice(4);
    const parts = p.replace(/\\/g, "/").split("/").filter(Boolean);
    const last = parts[parts.length - 1] || p;
    return last === "~" ? "Home folder" : last.length === 2 && last.endsWith(":") ? last : last;
  };
  const store = {
    get(k, d) { try { return JSON.parse(localStorage.getItem(k)) ?? d; } catch (_) { return d; } },
    set(k, v) { try { localStorage.setItem(k, JSON.stringify(v)); } catch (_) {} },
  };
  let toastTimer;
  function toast(msg) {
    const t = $("#toast");
    t.textContent = msg;
    t.classList.add("is-visible");
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => t.classList.remove("is-visible"), 2600);
  }
  async function copy(text, ok = "Copied") {
    try { await navigator.clipboard.writeText(text); toast(ok); } catch (_) { toast("Couldn't copy - select the text and copy it"); }
  }
  async function busy(btn, fn) {
    if (btn) btn.classList.add("is-busy");
    try { return await fn(); } catch (e) { toast(e.message || String(e)); } finally { if (btn) btn.classList.remove("is-busy"); }
  }

  /* ------------------------------------------------------------ state */
  const S = { overview: null, status: null, history: [], settings: null, doctor: null, devices: null, loadedOnce: false };

  const PHASE = {
    starting: "Starting", idle: "Up to date", waiting: "Waiting for your sessions to go quiet", syncing: "Syncing",
    paused: "Paused", backoff: "Retrying after an error", stopped: "Engine stopped",
  };
  const phase = () => (S.status && S.status.status ? S.status.status.phase : S.status && !S.status.engine ? "readonly" : "starting");
  const phaseText = () => (phase() === "readonly" ? "Read-only view" : PHASE[phase()] || "Working");

  async function loadCore() {
    const [overview, status, history, settings] = await Promise.all([api("/api/overview"), api("/api/status"), api("/api/history"), api("/api/settings")]);
    Object.assign(S, { overview, status, history, settings, loadedOnce: true });
  }

  /* ------------------------------------------------------------ live updates (sidebar, header, engine card) */
  function updateLive() {
    const st = S.status || {};
    const ph = phase();
    const syncing = ph === "syncing";
    const dot = $("#side-dot");
    if (dot) { dot.className = "team-dot" + (syncing ? " is-busy" : ph === "idle" ? " is-live" : ""); }
    const dn = $("#side-device-name");
    const me = S.overview && S.overview.devices.find((d) => d.this_device);
    if (dn) dn.textContent = (me && me.name) || "This device";
    const ds = $("#side-device-state");
    if (ds) ds.textContent = phaseText();
    const btn = $("#btn-sync");
    if (btn) {
      btn.disabled = !st.engine || syncing;
      btn.title = st.engine ? "" : "The sync engine isn't running in this session";
      btn.innerHTML = syncing ? `${loader("circle")}<span>Syncing</span>` : `${ico("refresh", "ids-icon--xs")}<span>Sync now</span>`;
    }
    const user = (S.settings && S.settings.user) || "";
    const ov0 = S.overview;
    const heads = {
      projects: ["Projects", ov0 ? `${num(ov0.totals.projects)} tracked across ${ov0.totals.devices} device${ov0.totals.devices === 1 ? "" : "s"}` : ""],
      devices: ["Devices", "Computers that share this vault"],
      archive: ["Archive", "Search every session and imported chat on this device"],
      health: ["Health", "Checks, integrity and cleanup"],
      settings: ["Settings", "What gets synced, and what never leaves a device"],
    };
    const head = heads[route()];
    $("#greeting").textContent = head ? head[0] : user ? `Hi, ${user}!` : "Hi!";
    $("#avatar").textContent = (user || (me && me.name) || "V").charAt(0).toUpperCase();
    const ov = S.overview;
    $("#subline").textContent = head ? head[1] : ov ? `${ov.totals.devices} device${ov.totals.devices === 1 ? "" : "s"} · ${num(ov.totals.sessions)} sessions · last sync ${st.status && st.status.last_sync ? ago(st.status.last_sync) : "none yet"}` : "";
    const tools = S.settings && S.settings.tools;
    if (tools) {
      $$(".side-list li").forEach((li) => {
        const on = tools[li.dataset.tool];
        li.classList.toggle("is-off", !on);
        li.title = on ? "Found on this device" : "Not found on this device";
        const sm = $("small", li);
        if (sm) sm.textContent = on ? "" : "n/a";
      });
    }
    const lastErr = st.last_error && (!st.status || st.status.phase === "backoff");
    const bell = $("#bell-dot");
    if (bell) bell.hidden = !lastErr && !(S.history[S.history.length - 1] && S.history[S.history.length - 1].conflicts);

    // engine card on the dashboard
    const title = $("#eng-title");
    if (title) {
      title.textContent = syncing ? "Syncing your sessions" : phaseText();
      $("#eng-loader").innerHTML = syncing ? loader("circle") : ph === "waiting" || ph === "backoff" ? loader("dots") : "";
      const last = st.status && st.status.last_sync;
      $("#eng-meta").textContent = last ? `Last sync ${ago(last)}` : "No sync yet";
      const pb = $("#eng-pause");
      if (pb) pb.innerHTML = ico(ph === "paused" ? "play" : "pause");
      if (pb) pb.setAttribute("aria-label", ph === "paused" ? "Resume engine" : "Pause engine");
    }
    const pr = st.progress;
    const bar = $("#prog-bar");
    if (bar) {
      const pct = pr && pr.total ? Math.min(100, Math.round((pr.done / pr.total) * 100)) : syncing ? 4 : 0;
      bar.style.width = pct + "%";
      $("#prog-text").textContent = syncing && pr ? `${pr.phase === "restore" ? "Checking vault" : "Uploading"} · ${num(pr.done)} / ${num(pr.total)}` : st.status ? `${num(st.status.pending_changes)} change${st.status.pending_changes === 1 ? "" : "s"} waiting` : "Nothing waiting";
    }
  }

  /* ------------------------------------------------------------ SSE */
  let sse;
  function connectEvents() {
    try { sse = new EventSource("/api/events?t=" + encodeURIComponent(TOKEN)); } catch (_) { return; }
    sse.onmessage = (m) => {
      let ev; try { ev = JSON.parse(m.data); } catch (_) { return; }
      S.status = S.status || { engine: true };
      if (ev.event === "status") { S.status.status = ev; }
      else if (ev.event === "progress") { S.status.progress = { phase: ev.phase, done: ev.done, total: ev.total }; }
      else if (ev.event === "error") { S.status.last_error = ev.message; toast("Sync problem: " + ev.message); }
      else if (ev.event === "sync_finished") {
        S.status.progress = null;
        const r = ev.report;
        if (r.uploaded || r.downloaded || r.merged) toast(`Synced · ${r.uploaded} up, ${r.downloaded} down${r.merged ? ", " + r.merged + " merged" : ""}`);
        refreshSoft();
      }
      updateLive();
    };
  }
  let softTimer;
  function refreshSoft() {
    clearTimeout(softTimer);
    softTimer = setTimeout(async () => {
      try { await loadCore(); updateLive(); if (["dashboard", "projects", "devices"].includes(route())) renderPage(); } catch (_) {}
    }, 400);
  }

  /* ------------------------------------------------------------ pages */
  const route = () => (location.hash.replace(/^#\/?/, "").split(/[\/?]/)[0] || "dashboard");

  let lastRoute = null;
  function renderPage() {
    const r = route();
    $$(".ids-nav__item").forEach((a) => a.classList.toggle("is-active", a.dataset.route === r));
    const page = $("#page");
    const fn = PAGES[r] || PAGES.dashboard;
    if (!S.loadedOnce) { page.innerHTML = `<div class="loading">${loader("circle", "ld--lg")}</div>`; return; }
    const scroll = page.scrollTop;
    page.classList.toggle("is-entering", r !== lastRoute);
    lastRoute = r;
    page.innerHTML = fn();
    if (AFTER[r]) AFTER[r]();
    page.scrollTop = scroll;
    updateLive();
  }

  /* ---- dashboard ---- */
  function activityBuckets() {
    const days = [];
    const today = new Date(); today.setHours(0, 0, 0, 0);
    for (let i = 6; i >= 0; i--) { const d = new Date(today); d.setDate(d.getDate() - i); days.push({ d, up: 0, down: 0 }); }
    for (const h of S.history) {
      const t = new Date(Date.parse(h.at)); if (isNaN(t)) continue; t.setHours(0, 0, 0, 0);
      const b = days.find((x) => x.d.getTime() === t.getTime());
      if (b) { b.up += h.uploaded; b.down += h.downloaded + h.merged; }
    }
    return days;
  }
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
    const up = days.map((d, i) => xy(d.up, i)), down = days.map((d, i) => xy(d.down, i));
    const any = days.some((d) => d.up || d.down);
    const peak = days.reduce((m, d, i) => (d.up > days[m].up ? i : m), 0);
    const tip = any && days[peak].up ? `<div class="act__tip" style="left:${(peak / 6) * 100}%;top:${(up[peak][1] / H) * 100}%">${num(days[peak].up)} up</div>` : "";
    const names = ["S", "M", "T", "W", "T", "F", "S"];
    return `<section class="card ids-card ids-card--glass act">
      <header class="card__head"><h2 class="card__title">Sync activity</h2><span class="tool">${ico("wave")}</span></header>
      <div class="legend"><span class="ids-legend"><span class="ids-dot"></span>Uploaded</span><span class="ids-legend"><span class="ids-dot ids-dot--muted"></span>Received</span></div>
      <div class="act__chart"><svg viewBox="0 0 ${W} ${H}" preserveAspectRatio="none" aria-label="Sync activity over the last 7 days">
        <path class="ids-chart__axis" d="M.5 0V${H - .5}H${W - .5}V0" vector-effect="non-scaling-stroke"/>
        <path class="ids-chart__study" d="${smooth(down)}" vector-effect="non-scaling-stroke"/>
        <path class="ids-chart__sport" d="${smooth(up)}" vector-effect="non-scaling-stroke"/></svg>${tip}
        ${any ? "" : `<div class="act__empty">No sync activity yet.<br>It appears here after your first sync.</div>`}</div>
      <div class="act__days ids-chart__days">${days.map((d, i) => `<span class="ids-chart__day ${i === 6 ? "is-active" : ""}" title="${d.d.toDateString()}">${names[d.d.getDay()]}</span>`).join("")}</div>
    </section>`;
  }
  function storageCard() {
    const o = S.overview, raw = o.totals.unique_bytes || o.totals.bytes, disk = o.disk_bytes || 0;
    const saved = raw > 0 ? Math.max(0, Math.min(100, Math.round((1 - disk / raw) * 100))) : 0;
    const C = 2 * Math.PI * 33;
    return `<section class="card ids-card sto">
      <header class="card__head"><h2 class="card__title">Storage</h2><span class="tool">${ico("bars")}</span></header>
      <div class="sto__body">
        <div class="ids-gauge"><svg viewBox="0 0 93 93" aria-hidden="true">
          <circle class="ids-gauge__ring" cx="46.5" cy="46.5" r="44.5" stroke="#1b1b1b" stroke-width="3.6"/>
          <circle class="ids-gauge__ring" cx="46.5" cy="46.5" r="33" stroke="#ececec" stroke-width="3"/>
          <circle class="ids-gauge__ring" cx="46.5" cy="46.5" r="33" stroke="#2b2b2b" stroke-width="3" stroke-dasharray="${(saved / 100) * C} ${C}" transform="rotate(-90 46.5 46.5)"/>
          <circle class="ids-gauge__ring" cx="46.5" cy="46.5" r="22" stroke="#e3e3e3" stroke-width="2.4"/>
          <circle class="ids-gauge__ring" cx="46.5" cy="46.5" r="22" stroke="#a7a7a7" stroke-width="2.4" stroke-dasharray="${Math.min(1, o.totals.devices / 4) * 138} 200" transform="rotate(-90 46.5 46.5)"/></svg>
          <div class="ids-gauge__center"><div class="ids-gauge__value">${saved}%</div><div class="ids-gauge__label">saved</div></div></div>
        <div class="sto__legend">
          <div><span class="ids-dot"></span>On disk <b>${fmtBytes(disk)}</b></div>
          <div><span class="ids-dot ids-dot--muted"></span>Original <b>${fmtBytes(raw)}</b></div>
          <div><span class="ids-dot ids-dot--faint"></span>${num(o.totals.files)} files · encrypted</div></div>
      </div>
      <div class="sto__foot"><button class="ids-iconbtn ids-iconbtn--dark" data-action="copy-vault" aria-label="Copy vault folder path">${ico("copy")}</button>
        <button class="ids-btn ids-btn--ghost" data-action="verify">Verify vault ${ico("download", "ids-icon--sm")}</button></div>
    </section>`;
  }
  function healthCard() {
    const items = S.doctor || [];
    const ok = items.filter((x) => x.level === "Ok").length;
    const C = 2 * Math.PI * 17;
    const rows = items.length ? items.map((x) => `<div class="chk__row is-${x.level.toLowerCase()}"><span class="ids-check ${x.level === "Ok" ? "is-checked" : ""}">${ico("check")}</span><div>${esc(x.title)}${x.detail && x.level !== "Ok" ? `<small>${esc(x.detail.length > 110 ? x.detail.slice(0, 107) + "…" : x.detail)}</small>` : ""}</div></div>`).join("") : `<div class="loading">${loader("dots")}</div>`;
    return `<section class="card ids-card ids-card--panel chk">
      <header class="card__head"><h2 class="card__title">Health checks</h2>
        <span class="ring" aria-label="${ok} of ${items.length} checks passed"><svg viewBox="0 0 40 40"><circle class="t" cx="20" cy="20" r="17"/><circle class="v" cx="20" cy="20" r="17" stroke-dasharray="${items.length ? (ok / items.length) * C : 0} ${C}"/></svg>${ok}/${items.length}</span></header>
      <div class="chk__rows">${rows}</div></section>`;
  }
  function engineCards() {
    const st = S.status || {}, last = [...S.history].reverse().find((h) => true);
    return `<div class="tcard">
        <div class="tcard__top">${ico("wave")}<span id="eng-loader"></span></div>
        <div class="tcard__title" id="eng-title">${esc(phaseText())}</div>
        <div class="tcard__meta" id="eng-meta">&nbsp;</div>
        ${st.engine ? `<button class="tcard__corner" id="eng-pause" data-action="pause-toggle">${ico("pause")}</button>` : ""}</div>
      <div class="tcard"><div class="tcard__top">${ico("target")}</div>
        <div class="tcard__title">Queue</div>
        <div class="progress"><span id="prog-bar"></span></div>
        <div class="tcard__meta" id="prog-text">&nbsp;</div></div>
      <div class="tcard tcard--dark"><div class="tcard__top">${ico("stats")}</div>
        <div class="tcard__title">Last sync</div>
        ${last ? `<div class="tcard__nums"><div><b>${num(last.uploaded)}</b><span>uploaded</span></div><div><b>${num(last.downloaded + last.merged)}</b><span>received</span></div></div>
        <div class="tcard__meta">${ago(last.at)}${last.conflicts ? ` · ${last.conflicts} conflict${last.conflicts > 1 ? "s" : ""}` : ""}</div>` : `<div class="tcard__meta" style="margin-top:14px">Nothing yet</div>`}</div>
      <button class="addcard" data-action="add-device">${ico("plus")}<span>Add a device</span></button>`;
  }
  function projectCards(list, limit) {
    const total = S.overview.totals.devices || 1;
    const C = 2 * Math.PI * 14;
    return list.slice(0, limit).map((p) => `<article class="pcard ${p.exists_locally || p.portable_path.startsWith("raw:") ? "" : "pcard--dim"}">
      <header class="pcard__head"><h3 class="pcard__title" title="${esc(p.portable_path)}">${esc(baseName(p.portable_path))}</h3>
        <span class="ring" title="${p.devices.length} of ${total} devices have this project"><svg viewBox="0 0 34 34"><circle class="t" cx="17" cy="17" r="14"/><circle class="v" cx="17" cy="17" r="14" stroke-dasharray="${(p.devices.length / total) * C} ${C}"/></svg>${p.devices.length}/${total}</span></header>
      <div class="pcard__meta"><span class="ids-dot"></span>${p.exists_locally ? "On this device" : p.portable_path.startsWith("raw:") ? "Notes only" : "Folder not on this device"}</div>
      <p class="pcard__path"><b>${num(p.sessions)} session${p.sessions === 1 ? "" : "s"}</b> · ${fmtBytes(p.bytes)} · active ${ago(p.last_activity_ms)}<br>${esc(p.portable_path)}</p></article>`).join("");
  }
  function dashboardPage() {
    const o = S.overview, t = o.totals;
    const cover = o.projects.length ? Math.round((o.projects.filter((p) => p.exists_locally).length / o.projects.length) * 100) : 0;
    return `<div class="grid">
      <section class="card ids-card ids-card--dark c4 ov">
        <header class="card__head"><h2 class="card__title">Vault overview</h2><span class="tool">${ico("lock")}</span></header>
        <div class="ov__stats"><div class="ov__stat"><span class="ov__num">${num(t.sessions)}</span><span class="ov__lbl">sessions<br>in the vault</span></div><span class="ov__div"></span>
          <div class="ov__stat"><span class="ov__num">${num(t.projects)}</span><span class="ov__lbl">projects<br>tracked</span></div></div>
        <div class="ov__bar"><span style="width:${cover}%"></span></div>
        <div class="ov__cap">${o.projects.filter((p) => p.exists_locally).length} of ${t.projects} project folders are on this device</div>
        <div class="mini-row"><div class="mini">${ico("target")}<b>${num(t.projects)}</b><span>Projects</span></div><div class="mini">${ico("laptop")}<b>${num(t.devices)}</b><span>Devices</span></div><div class="mini">${ico("spiral")}<b>${num(t.files)}</b><span>Files</span></div></div>
      </section>
      <div class="c4">${activityCard()}</div>
      <div class="c4">${storageCard()}</div>
      <div class="c4">${healthCard()}</div>
      <div class="c8"><div class="section-head" style="margin-top:0"><h2>Sync engine</h2><a class="small muted" href="#/health">Details ${ico("chev-r", "ids-icon--xs")}</a></div><div class="proc">${engineCards()}</div></div>
    </div>
    <div class="section-head"><h2>Recent projects</h2><a class="small" href="#/projects">View all ${ico("chev-r", "ids-icon--xs")}</a></div>
    ${o.projects.length ? `<div class="pgrid">${projectCards(o.projects, 6)}</div>` : `<div class="empty">${loader("ball")}<h3>Nothing in the vault yet</h3><p>Press <b>Sync now</b> to upload this device's sessions.</p></div>`}`;
  }

  /* ---- projects ---- */
  let pq = "", psort = store.get("psort", "recent"), pview = store.get("pview", "grid");
  function projectsPage() {
    return `<div class="toolbar"><input class="input input--grow" id="pq" placeholder="Filter projects" value="${esc(pq)}" aria-label="Filter projects">
      <select class="input select" id="psort" aria-label="Sort projects"><option value="recent">Most recent</option><option value="sessions">Most sessions</option><option value="name">Name</option><option value="size">Largest</option></select>
      <div class="seg"><button data-view="grid" class="${pview === "grid" ? "is-active" : ""}" aria-label="Grid view">${ico("grid")}</button><button data-view="list" class="${pview === "list" ? "is-active" : ""}" aria-label="List view">${ico("list")}</button></div></div>
      <div id="plist"></div>`;
  }
  function renderProjectList() {
    let l = [...S.overview.projects].filter((p) => (p.portable_path + " " + baseName(p.portable_path)).toLowerCase().includes(pq.toLowerCase()));
    const by = { recent: (a, b) => b.last_activity_ms - a.last_activity_ms, sessions: (a, b) => b.sessions - a.sessions, size: (a, b) => b.bytes - a.bytes, name: (a, b) => baseName(a.portable_path).localeCompare(baseName(b.portable_path)) };
    l.sort(by[psort] || by.recent);
    const el = $("#plist");
    if (!l.length) { el.innerHTML = `<div class="empty"><h3>No projects match</h3></div>`; return; }
    el.innerHTML = pview === "grid" ? `<div class="pgrid">${projectCards(l, 500)}</div>` :
      `<div class="list">${l.map((p) => `<div class="row"><div class="row__title"><b>${esc(baseName(p.portable_path))}</b><div class="small row__title">${esc(p.portable_path)}</div></div>
        <div><b>${num(p.sessions)}</b><div class="small">sessions</div></div><div><b>${fmtBytes(p.bytes)}</b><div class="small">size</div></div><div><b>${ago(p.last_activity_ms)}</b><div class="small">active</div></div>
        <button class="ids-btn ids-btn--ghost ids-btn--sm" data-action="exclude" data-path="${esc(p.portable_path)}">Exclude</button></div>`).join("")}</div>`;
  }

  /* ---- devices ---- */
  function devicesPage() {
    const d = S.devices;
    if (!d) return `<div class="loading">${loader("circle", "ld--lg")}</div>`;
    return `<div class="dgrid">${d.devices.map((x) => `<article class="dcard ${x.this_device ? "is-me" : ""}">
        <div class="dcard__top"><h3>${esc(x.name)}</h3>${x.this_device ? `<span class="chip">This device</span>` : ""}</div>
        <dl class="kv"><dt>System</dt><dd>${esc(x.os)}</dd><dt>Last sync</dt><dd>${ago(x.updated)}</dd><dt>Files</dt><dd>${num(x.files)}</dd><dt>Id</dt><dd class="mono">${esc(x.id.slice(0, 8))}</dd></dl>
        ${x.this_device ? "" : `<button class="ids-btn ids-btn--ghost ids-btn--sm" data-action="forget-device" data-id="${esc(x.id)}">Forget this device</button>`}</article>`).join("")}
      <button class="addcard" data-action="add-device" style="min-height:200px">${ico("plus")}<span>Add a device</span></button></div>
      <div class="section-head"><h2>Passphrase-free access</h2></div>
      ${d.joined.length ? `<div class="list">${d.joined.map((j) => `<div class="row" style="grid-template-columns:1fr auto auto"><div><b>${esc(j.name || "Device")}</b><div class="small">joined ${ago(j.created)}</div></div><span class="mono muted">${esc(j.id)}</span><button class="ids-btn ids-btn--ghost ids-btn--sm" data-action="revoke" data-id="${esc(j.id)}">Revoke</button></div>`).join("")}</div>` : `<p class="muted">Devices that joined with a one-time code appear here. Revoking removes their access slot.</p>`}
      ${d.warnings.length ? `<p class="small muted" style="margin-top:14px">${d.warnings.map(esc).join("<br>")}</p>` : ""}`;
  }

  /* ---- archive ---- */
  let aq = "", acur = null;
  function archivePage() {
    const hq = new URLSearchParams(location.hash.split("?")[1] || "");
    if (hq.has("q")) aq = hq.get("q");
    return `<div class="toolbar"><input class="input input--grow" id="aq" placeholder="Search every Claude Code session and imported chat" value="${esc(aq)}" autofocus aria-label="Search sessions"></div>
      <div class="arch"><div class="hits" id="hits"></div><div id="viewer"><div class="viewer"><div class="empty">${loader("ball")}<h3>Pick a conversation</h3><p>Search above, then open a result to read it or copy it as a context pack for a new chat.</p></div></div></div></div>`;
  }
  let searchSeq = 0;
  async function doSearch() {
    const my = ++searchSeq, hits = $("#hits");
    hits.innerHTML = `<div class="loading">${loader("fade")}</div>`;
    try {
      const r = await api("/api/search?q=" + encodeURIComponent(aq));
      if (my !== searchSeq) return;
      if (r.length && new URLSearchParams(location.hash.split("?")[1] || "").has("open") && !acur) { setTimeout(() => { const b = $(".hit"); if (b) openHit(b); }, 0); }
      hits.innerHTML = r.length ? r.map((h) => `<button class="hit ${acur && acur.id === h.id ? "is-active" : ""}" data-action="open-hit" data-source="${esc(h.source)}" data-id="${esc(h.id)}">
          <div class="hit__top"><span>${esc(h.source)}${h.project ? " · " + esc(baseName(h.project)) : ""}</span><span>${esc(h.updated)}</span></div>
          <div class="hit__title">${esc(h.title || "Untitled")}</div><div class="hit__snip">${esc(h.snippet || h.messages + " messages")}</div></button>`).join("") : `<div class="empty"><h3>No matches</h3><p>Try fewer or different words.</p></div>`;
    } catch (e) { hits.innerHTML = `<div class="empty"><h3>Search failed</h3><p>${esc(e.message)}</p></div>`; }
  }
  async function openHit(btn) {
    $$(".hit").forEach((h) => h.classList.toggle("is-active", h === btn));
    const v = $("#viewer");
    v.innerHTML = `<div class="viewer"><div class="loading">${loader("circle")}</div></div>`;
    try {
      const c = await api(`/api/conversation?source=${encodeURIComponent(btn.dataset.source)}&id=${encodeURIComponent(btn.dataset.id)}`);
      acur = c;
      v.innerHTML = `<div class="viewer"><div class="viewer__head"><div><h2>${esc(c.title || "Untitled")}</h2><p class="small muted">${esc(c.source)} · ${c.messages.length} messages · ${esc(c.updated)}${c.project ? " · " + esc(c.project) : ""}</p></div>
        <button class="ids-btn ids-btn--dark" data-action="copy-pack">${ico("copy", "ids-icon--sm")} Copy as context pack</button></div>
        <div class="msgs">${c.messages.slice(0, 400).map((m) => `<div class="msg ${m.role === "user" ? "user" : ""}"><small>${m.role === "user" ? "You" : "Assistant"}${m.time ? " · " + esc(m.time) : ""}</small>${esc(m.text.length > 6000 ? m.text.slice(0, 6000) + "\n… (shortened)" : m.text)}</div>`).join("")}${c.messages.length > 400 ? `<p class="muted small">Showing the first 400 messages. The context pack keeps the first and the most recent ones.</p>` : ""}</div></div>`;
    } catch (e) { v.innerHTML = `<div class="viewer"><div class="empty"><h3>Couldn't open it</h3><p>${esc(e.message)}</p></div></div>`; }
  }

  /* ---- health ---- */
  let verifyOut = "";
  function healthPage() {
    const items = S.doctor || [];
    return `<div class="grid"><section class="card ids-card c7"><header class="card__head"><h2 class="card__title">Checks</h2>
        <button class="ids-btn ids-btn--dark ids-btn--sm" data-action="deep-scan">Scan for leaked secrets</button></header>
        <div class="chk__rows">${items.map((x) => `<div class="chk__row is-${x.level.toLowerCase()}"><span class="ids-check ${x.level === "Ok" ? "is-checked" : ""}">${ico("check")}</span><div>${esc(x.title)}${x.detail ? `<small>${esc(x.detail)}</small>` : ""}</div></div>`).join("") || loader("dots")}</div></section>
      <section class="card ids-card c5"><header class="card__head"><h2 class="card__title">Vault integrity</h2></header>
        <p class="small muted" style="margin-top:8px">Checks that every file the devices list is present. A deep check also decrypts and verifies each one.</p>
        <div class="inline"><button class="ids-btn ids-btn--ghost" data-action="verify">Quick check</button><button class="ids-btn ids-btn--dark" data-action="verify-deep">Deep check</button></div>
        <div id="verify-out" class="small" style="margin-top:14px">${verifyOut}</div>
        <hr class="ids-hr" style="margin:18px 0"><p class="small muted">Remove data no device references any more.</p>
        <div class="inline"><button class="ids-btn ids-btn--ghost" data-action="gc">Clean up unused data</button></div></section></div>`;
  }

  /* ---- settings ---- */
  function settingsPage() {
    const s = S.settings;
    return `<div class="stack"><section class="card ids-card"><h2 class="card__title">Vault</h2>
        <dl class="kv kv--left" style="margin-top:14px"><dt>Folder</dt><dd class="mono">${esc(s.vault_path)}</dd><dt>Vault id</dt><dd class="mono">${esc(s.vault_id)}</dd><dt>Sync engine</dt><dd>${s.engine ? "Running" : "Not running (read-only view)"}</dd></dl></section>
      <section class="card ids-card"><h2 class="card__title">Never upload these projects</h2><p class="small muted" style="margin-top:6px">Plugin-generated or noisy projects. Anything already in the vault stays there.</p>
        <div class="tags">${s.exclude_projects.length ? s.exclude_projects.map((p) => `<span class="tag">${esc(p)}<button data-action="unexclude" data-path="${esc(p)}" aria-label="Stop excluding ${esc(p)}">${ico("x")}</button></span>`).join("") : `<span class="small muted">Nothing excluded.</span>`}</div>
        <form class="inline" data-form="exclude"><input class="input input--grow" name="path" placeholder="~/.claude-mem/observer-sessions" aria-label="Project path to exclude"><button class="ids-btn ids-btn--dark" type="submit">Exclude</button></form></section>
      <section class="card ids-card"><h2 class="card__title">Kept on each device</h2><p class="small muted" style="margin-top:6px">These <span class="mono">settings.json</span> keys hold machine-specific commands or paths, so they never leave the device they belong to.</p>
        <div class="tags">${s.local_keys.map((k) => `<span class="tag is-fixed mono">${esc(k)}</span>`).join("")}</div></section>
      <section class="card ids-card"><h2 class="card__title">Folders on this device</h2>
        <dl class="kv kv--left" style="margin-top:14px"><dt>Claude Code</dt><dd class="mono">${esc(s.claude_dir)}</dd><dt>Codex</dt><dd class="mono">${esc(s.codex_dir)}</dd><dt>Vault state</dt><dd class="mono">${esc(s.state_dir)}</dd></dl></section></div>`;
  }

  const PAGES = { dashboard: dashboardPage, projects: projectsPage, devices: devicesPage, archive: archivePage, health: healthPage, settings: settingsPage };
  const AFTER = {
    projects() {
      $("#psort").value = psort; renderProjectList();
      $("#pq").addEventListener("input", (e) => { pq = e.target.value; renderProjectList(); });
      $("#psort").addEventListener("change", (e) => { psort = e.target.value; store.set("psort", psort); renderProjectList(); });
    },
    archive() { $("#aq").addEventListener("input", debounce((e) => { aq = e.target.value; doSearch(); }, 300)); doSearch(); },
    async devices() { if (!S.devices) { try { S.devices = await api("/api/devices"); } catch (_) { S.devices = { devices: [], joined: [], warnings: ["Couldn't load devices"] }; } if (route() === "devices") renderPage(); } },
    async health() { if (!S.doctor) { try { S.doctor = await api("/api/doctor"); if (route() === "health") renderPage(); } catch (_) {} } },
    dashboard() { if (!S.doctor) api("/api/doctor").then((d) => { S.doctor = d; if (route() === "dashboard") renderPage(); }).catch(() => {}); },
  };
  const debounce = (fn, ms) => { let t; return (...a) => { clearTimeout(t); t = setTimeout(() => fn(...a), ms); }; };

  /* ------------------------------------------------------------ modals + popover */
  function modal(html) {
    const root = $("#modal-root");
    root.innerHTML = `<div class="modal-bg" data-action="close-modal"><div class="modal" role="dialog" aria-modal="true" onclick="event.stopPropagation()">${html}</div></div>`;
    const f = $("button, input", root); if (f) f.focus();
  }
  const closeModal = () => { $("#modal-root").innerHTML = ""; };
  async function enrollModal() {
    modal(`<h2>Add a device</h2><div class="loading">${loader("fade")}</div>`);
    try {
      const r = await api("/api/enroll", { method: "POST", body: { minutes: 15 } });
      modal(`<h2>Add a device</h2><p>On the other computer, install AI Session Vault, point it at this vault's folder and run:</p>
        <div class="code" id="enroll-code">${esc(r.code)}</div><p class="small muted mono">avault join --vault "&lt;vault folder&gt;" --code ${esc(r.code)}</p>
        <p class="small muted" style="margin-top:10px">Works once and expires in ${r.expires_in_minutes} minutes. The passphrase is never shared.</p>
        <div class="modal__actions"><button class="ids-btn ids-btn--ghost" data-action="close-modal">Done</button><button class="ids-btn ids-btn--dark" data-action="copy-code">${ico("copy", "ids-icon--sm")} Copy code</button></div>`);
    } catch (e) { closeModal(); toast(e.message); }
  }
  function activityPopover() {
    const ex = $(".popover"); if (ex) { ex.remove(); return; }
    const h = [...S.history].reverse().slice(0, 8);
    const err = S.status && S.status.last_error;
    const el = document.createElement("div");
    el.className = "popover"; el.setAttribute("role", "dialog");
    el.innerHTML = `<h3>Recent activity</h3>${err ? `<div class="pop-row"><b>Last problem</b><br><span class="muted">${esc(err)}</span></div>` : ""}` +
      (h.length ? h.map((x) => `<div class="pop-row"><b>${x.full ? "Full sync" : "Quick sync"}</b> · ${ago(x.at)}<br><span class="muted">${num(x.uploaded)} up · ${num(x.downloaded)} down${x.merged ? " · " + x.merged + " merged" : ""}${x.conflicts ? " · " + x.conflicts + " conflict(s)" : ""}${x.failed ? " · " + x.failed + " failed" : ""}</span>${x.notes.map((n) => `<br><span class="small">${esc(n)}</span>`).join("")}</div>`).join("") : `<p class="muted small">Nothing yet.</p>`);
    document.body.appendChild(el);
    setTimeout(() => document.addEventListener("click", function once(e) { if (!el.contains(e.target) && e.target.closest("#btn-bell") == null) { el.remove(); document.removeEventListener("click", once); } }), 0);
  }

  /* ------------------------------------------------------------ actions */
  const ACTIONS = {
    sync: (b) => busy(b, () => api("/api/sync", { method: "POST" }).then(() => toast("Sync started"))),
    "pause-toggle": (b) => busy(b, () => api(phase() === "paused" ? "/api/resume" : "/api/pause", { method: "POST" })),
    activity: () => activityPopover(),
    "add-device": enrollModal,
    "close-modal": closeModal,
    "copy-code": () => copy($("#enroll-code").textContent, "Code copied"),
    "copy-vault": () => copy(S.settings.vault_path, "Vault folder copied"),
    "copy-pack": async (b) => busy(b, async () => { copy(await api(`/api/pack?source=${encodeURIComponent(acur.source)}&id=${encodeURIComponent(acur.id)}`), "Context pack copied - paste it into a new chat"); }),
    "open-hit": openHit,
    verify: (b) => runVerify(b, false),
    "verify-deep": (b) => runVerify(b, true),
    gc: (b) => busy(b, async () => { const r = await api("/api/gc", { method: "POST" }); toast(`Removed ${r.removed} unused object(s), freed ${fmtBytes(r.freed_bytes)}`); await loadCore(); updateLive(); }),
    "deep-scan": (b) => busy(b, async () => { b.innerHTML = `${loader("circle")} Scanning…`; S.doctor = await api("/api/doctor?deep=1"); renderPage(); toast("Scan finished"); }),
    exclude: (b) => busy(b, async () => { await api("/api/exclude", { method: "POST", body: { path: b.dataset.path } }); toast("Excluded - it won't be uploaded again"); S.settings = await api("/api/settings"); }),
    unexclude: (b) => busy(b, async () => { await api("/api/exclude", { method: "POST", body: { path: b.dataset.path, remove: true } }); S.settings = await api("/api/settings"); renderPage(); }),
    revoke: (b) => busy(b, async () => { if (!confirm("Revoke this device's passphrase-free access?")) return; await api("/api/revoke", { method: "POST", body: { id: b.dataset.id } }); S.devices = null; renderPage(); toast("Access revoked"); }),
    "forget-device": (b) => busy(b, async () => { if (!confirm("Forget this device? Its files stay in the vault only if another device has them too.")) return; await api("/api/forget-device", { method: "POST", body: { id: b.dataset.id } }); S.devices = null; await loadCore(); renderPage(); }),
  };
  async function runVerify(btn, deep) {
    const out = $("#verify-out");
    if (out) out.innerHTML = `${loader("fade")} Checking…`;
    await busy(btn, async () => {
      const r = await api("/api/verify", { method: "POST", body: { deep } });
      const bad = r.report.missing.length + r.report.corrupt.length;
      verifyOut = bad ? `<b>${bad} problem(s)</b> in ${num(r.report.objects)} objects.<br>${[...r.report.missing.map((m) => "Missing: " + m), ...r.report.corrupt.map((m) => "Corrupt: " + m)].slice(0, 6).map(esc).join("<br>")}<br><span class="muted">Missing files usually mean the cloud folder is still syncing.</span>` : `Healthy · ${num(r.report.objects)} objects checked${deep ? " and decrypted" : ""}.`;
      if (out) out.innerHTML = verifyOut; else toast(bad ? bad + " problem(s) found - see Health" : "Vault is healthy");
    });
  }

  document.addEventListener("click", (e) => {
    const el = e.target.closest("[data-action]");
    if (el && ACTIONS[el.dataset.action]) { if (el.tagName === "A") e.preventDefault(); ACTIONS[el.dataset.action](el); return; }
    const v = e.target.closest("[data-view]");
    if (v) { pview = v.dataset.view; store.set("pview", pview); $$("[data-view]").forEach((x) => x.classList.toggle("is-active", x === v)); renderProjectList(); }
  });
  document.addEventListener("submit", (e) => {
    const f = e.target.closest("[data-form=exclude]");
    if (!f) return;
    e.preventDefault();
    const path = new FormData(f).get("path").trim();
    if (!path) return;
    busy(null, async () => { await api("/api/exclude", { method: "POST", body: { path } }); S.settings = await api("/api/settings"); renderPage(); toast("Excluded"); });
  });
  document.addEventListener("keydown", (e) => { if (e.key === "Escape") { closeModal(); const p = $(".popover"); if (p) p.remove(); } });
  window.addEventListener("hashchange", renderPage);

  /* ------------------------------------------------------------ boot */
  (async function boot() {
    if (!TOKEN) {
      $("#page").innerHTML = `<div class="empty"><h3>Open the app from the link <span class="mono">avault ui</span> prints</h3><p>That link carries the one-time access token for this session.</p></div>`;
      return;
    }
    renderPage();
    try {
      await loadCore();
      renderPage();
      if (!params.has("static")) connectEvents(); // ?static: no live stream (used for screenshots)
      if (!params.has("static")) {
        setInterval(() => { if (S.status && S.status.engine) updateLive(); }, 30000);
        setInterval(async () => { try { await loadCore(); updateLive(); } catch (_) {} }, 60000);
      }
    } catch (e) {
      $("#page").innerHTML = `<div class="empty"><h3>Can't reach the vault service</h3><p>${esc(e.message)}</p></div>`;
    }
  })();
})();
