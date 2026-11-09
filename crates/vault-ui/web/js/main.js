/* AI Session Vault - front end entry point. No dependencies; talks to the local Rust core over /api. */

import { installActions } from "./actions.js";
import { friendlyError, hasToken } from "./api.js";
import { startLive, updateLive } from "./live.js";
import { dashboardAfter, dashboardPage, dashboardSkeleton } from "./pages/dashboard.js";
import { archiveAfter, archivePage } from "./pages/archive.js";
import { devicesAfter, devicesPage } from "./pages/devices.js";
import { healthAfter, healthPage } from "./pages/health.js";
import { projectsAfter, projectsPage } from "./pages/projects.js";
import { settingsPage } from "./pages/settings.js";
import { S, hooks, loadCore, route } from "./state.js";
import { errorState } from "./ui.js";
import { $, $$ } from "./util.js";

const PAGES = { dashboard: dashboardPage, projects: projectsPage, devices: devicesPage, archive: archivePage, health: healthPage, settings: settingsPage };
const AFTER = { dashboard: dashboardAfter, projects: projectsAfter, devices: devicesAfter, archive: archiveAfter, health: healthAfter };

let lastRoute = null;

function renderPage({ keepScroll = false } = {}) {
  const r = route();
  $$(".ids-nav__item").forEach((a) => a.classList.toggle("is-active", a.dataset.route === r));
  const page = $("#page");
  if (S.coreError) { page.innerHTML = errorState(S.coreError, { action: "retry-core" }, "Can't reach the vault service"); return; }
  if (!S.loadedOnce) { page.innerHTML = dashboardSkeleton(); return; }
  const scroll = page.scrollTop;
  page.classList.toggle("is-entering", r !== lastRoute);
  lastRoute = r;
  page.innerHTML = (PAGES[r] || PAGES.dashboard)();
  if (AFTER[r]) AFTER[r]();
  page.scrollTop = keepScroll ? scroll : 0;
  updateLive();
}

async function boot() {
  if (!hasToken()) {
    $("#page").innerHTML = errorState("That link carries the one-time access token for this session. Run avault ui or avault app and use the link it prints.", null, "Open the app from the link avault prints");
    return;
  }
  S.coreError = null;
  renderPage();
  try {
    await loadCore();
    renderPage();
    startLive();
  } catch (e) {
    S.coreError = friendlyError(e);
    renderPage();
  }
}

hooks.render = renderPage;
hooks.boot = boot;
installActions();
window.addEventListener("hashchange", () => renderPage());
boot();
