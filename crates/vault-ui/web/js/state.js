/* App state. `S` holds the core data; `R` tracks lazily loaded resources (loading / ready / error). */

import { api, friendlyError } from "./api.js";

export const S = { overview: null, status: null, history: [], settings: null, loadedOnce: false, coreError: null };

/** Set by main.js so pages can ask for a re-render without importing it. */
export const hooks = { render: () => {}, boot: () => {} };

export const route = () => location.hash.replace(/^#\/?/, "").split(/[/?]/)[0] || "dashboard";

const PHASE = {
  starting: "Starting",
  idle: "Up to date",
  waiting: "Waiting for your sessions to go quiet",
  syncing: "Syncing",
  paused: "Paused",
  backoff: "Retrying after an error",
  stopped: "Engine stopped",
};

export function phase() {
  if (S.status && S.status.status) return S.status.status.phase;
  return S.status && !S.status.engine ? "readonly" : "starting";
}
export const phaseText = () => (phase() === "readonly" ? "Read-only view" : PHASE[phase()] || "Working");

/** Load the data every page needs. Throws ApiError. */
export async function loadCore() {
  const [overview, status, history, settings] = await Promise.all([api("/api/overview"), api("/api/status"), api("/api/history"), api("/api/settings")]);
  Object.assign(S, { overview, status, history, settings, loadedOnce: true, coreError: null });
}

/* ---- lazily loaded resources ---- */
const idle = () => ({ state: "idle", data: null, error: "" });
export const R = { devices: idle(), doctor: idle() };
const PATHS = { devices: "/api/devices", doctor: "/api/doctor" };

/** Fetch a resource once. Errors stay in `R[name].error` until the user retries. */
export async function ensure(name) {
  const r = R[name];
  if (r.state !== "idle") return;
  r.state = "loading";
  try {
    r.data = await api(PATHS[name]);
    r.state = "ready";
  } catch (e) {
    r.state = "error";
    r.error = friendlyError(e);
  }
  hooks.render({ keepScroll: true });
}

export function invalidate(name) {
  R[name] = idle();
}

export function retry(name) {
  invalidate(name);
  hooks.render({ keepScroll: true });
}
