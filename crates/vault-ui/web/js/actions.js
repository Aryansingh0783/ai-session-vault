/* Click, submit and keyboard handling: one delegated listener, one table of actions. */

import { api } from "./api.js";
import { updateLive } from "./live.js";
import { activityPopover, closeModal, confirmModal, confirmYes, enrollModal } from "./modals.js";
import { clearSearch, currentConversation, doSearch, openHit, retryOpen } from "./pages/archive.js";
import { runDeepScan, runGc, runVerify } from "./pages/health.js";
import { clearProjectFilter, setProjectView } from "./pages/projects.js";
import { S, hooks, invalidate, loadCore, phase, retry } from "./state.js";
import { busy, copy, toast } from "./ui.js";
import { $, $$ } from "./util.js";

const post = (path, body) => api(path, { method: "POST", body });

async function refreshSettings() {
  S.settings = await api("/api/settings");
  hooks.render({ keepScroll: true });
}

const ACTIONS = {
  "set-theme": (el) => { window.VaultTheme.set(el.dataset.themeId); hooks.render({ keepScroll: true }); },
  sync: (b) => busy(b, () => post("/api/sync").then(() => toast("Sync started"))),
  "pause-toggle": (b) => busy(b, () => post(phase() === "paused" ? "/api/resume" : "/api/pause")),
  activity: activityPopover,
  "add-device": enrollModal,
  "close-modal": closeModal,
  "confirm-yes": confirmYes,
  "copy-code": () => copy($("#enroll-code").textContent, "Code copied"),
  "copy-vault": () => copy(S.settings.vault_path, "Vault folder copied"),
  "copy-pack": (b) => busy(b, async () => {
    const c = currentConversation();
    copy(await api(`/api/pack?source=${encodeURIComponent(c.source)}&id=${encodeURIComponent(c.id)}`), "Context pack copied. Paste it into a new chat.");
  }),
  "open-hit": (el) => openHit(el.dataset),
  "open-project": (el) => { location.hash = "#/archive?q=" + encodeURIComponent(el.dataset.q); },
  "retry-open": retryOpen,
  "retry-search": doSearch,
  "clear-search": clearSearch,
  "clear-project-filter": clearProjectFilter,
  retry: (el) => retry(el.dataset.res),
  "retry-core": () => hooks.boot(),
  verify: (b) => runVerify(b, false),
  "verify-deep": (b) => runVerify(b, true),
  gc: (b) => runGc(b),
  "deep-scan": (b) => runDeepScan(b),
  exclude: (b) => busy(b, async () => {
    await post("/api/exclude", { path: b.dataset.path });
    toast("Excluded. It won't be uploaded again.");
    await refreshSettings();
  }),
  unexclude: (b) => busy(b, async () => {
    await post("/api/exclude", { path: b.dataset.path, remove: true });
    await refreshSettings();
  }),
  revoke: (b) => confirmModal({
    title: "Revoke access?",
    body: "This device will need the passphrase again before it can open the vault.",
    label: "Revoke",
    run: () => busy(b, async () => { await post("/api/revoke", { id: b.dataset.id }); invalidate("devices"); hooks.render({ keepScroll: true }); toast("Access revoked"); }),
  }),
  "forget-device": (b) => confirmModal({
    title: "Forget this device?",
    body: "Its files stay in the vault only if another device has them too.",
    label: "Forget device",
    run: () => busy(b, async () => { await post("/api/forget-device", { id: b.dataset.id }); invalidate("devices"); await loadCore(); updateLive(); hooks.render({ keepScroll: true }); }),
  }),
};

export function installActions() {
  document.addEventListener("click", (e) => {
    const el = e.target.closest("[data-action]");
    if (el && el.classList.contains("modal-bg") && e.target.closest(".modal")) return; // clicks inside the dialog don't dismiss it
    if (el && ACTIONS[el.dataset.action]) {
      if (el.tagName === "A") e.preventDefault();
      ACTIONS[el.dataset.action](el);
      return;
    }
    const view = e.target.closest("[data-view]");
    if (view) {
      $$("[data-view]").forEach((x) => x.classList.toggle("is-active", x === view));
      setProjectView(view.dataset.view);
    }
  });

  document.addEventListener("submit", (e) => {
    const form = e.target.closest("[data-form=exclude]");
    if (!form) return;
    e.preventDefault();
    const path = String(new FormData(form).get("path") || "").trim();
    if (!path) { toast("Enter a project path first."); return; }
    busy(null, async () => { await post("/api/exclude", { path }); await refreshSettings(); toast("Excluded"); });
  });

  document.addEventListener("keydown", (e) => {
    const card = e.target.matches && e.target.matches("[data-action=open-project]");
    if (card && (e.key === "Enter" || e.key === " ")) { e.preventDefault(); ACTIONS["open-project"](e.target); }
    if (e.key === "Escape") { closeModal(); const p = $(".popover"); if (p) p.remove(); }
  });
}

