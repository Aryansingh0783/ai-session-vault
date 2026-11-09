/* UI building blocks: loaders, skeletons, empty and error states, toasts. */

import { friendlyError } from "./api.js";
import { $, esc } from "./util.js";

export const loader = (kind = "circle", cls = "") =>
  `<span class="ld ld-${kind} ${cls}" role="img" aria-label="Loading">${kind === "dots" ? "<i></i>" : ""}</span>`;

/* ---- skeletons: grey placeholders in the shape of the content that is coming ---- */
const sk = (cls) => `<span class="sk ${cls}"></span>`;
export const skeletonCard = () => `<div class="sk-card" aria-hidden="true">${sk("sk--title")}${sk("sk--line")}${sk("sk--line")}${sk("sk--block")}</div>`;
export const skeletonGrid = (n = 6) => `<div class="sk-grid" role="status" aria-label="Loading">${Array.from({ length: n }, skeletonCard).join("")}</div>`;
export const skeletonList = (n = 4) =>
  `<div class="sk-list" role="status" aria-label="Loading">${Array.from({ length: n }, () => `<div class="sk-card" aria-hidden="true">${sk("sk--line")}${sk("sk--line")}</div>`).join("")}</div>`;
export const skeletonRows = (n = 4) => `<div class="chk__rows" role="status" aria-label="Loading">${Array.from({ length: n }, () => sk("sk--line")).join("")}</div>`;

/* ---- empty and error states ---- */
/** An empty state with a clear next step. `cta` = { label, action, attrs }. */
export function emptyState(title, body = "", cta = null) {
  return `<div class="empty"><h3>${esc(title)}</h3>${body ? `<p>${esc(body)}</p>` : ""}${ctaButton(cta)}</div>`;
}

/** An error state with a retry button. `retry` = { action, attrs }. */
export function errorState(message, retry = null, title = "That didn't load") {
  return `<div class="empty" role="alert"><h3>${esc(title)}</h3><p class="empty__detail">${esc(message)}</p>${retry ? ctaButton({ label: "Try again", ...retry }) : ""}</div>`;
}

function ctaButton(cta) {
  if (!cta) return "";
  const attrs = Object.entries(cta.attrs || {}).map(([k, v]) => ` data-${k}="${esc(v)}"`).join("");
  return `<button class="ids-btn ids-btn--dark" data-action="${esc(cta.action)}"${attrs}>${esc(cta.label)}</button>`;
}

/* ---- toasts and small helpers ---- */
let toastTimer;
export function toast(msg) {
  const t = $("#toast");
  t.textContent = msg;
  t.classList.add("is-visible");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => t.classList.remove("is-visible"), 3200);
}

export async function copy(text, ok = "Copied") {
  try { await navigator.clipboard.writeText(text); toast(ok); } catch (_) { toast("Couldn't copy. Select the text and copy it."); }
}

/** Run an action with a busy button; any failure becomes a toast. */
export async function busy(btn, fn) {
  if (btn) btn.classList.add("is-busy");
  try { return await fn(); } catch (e) { toast(friendlyError(e)); } finally { if (btn) btn.classList.remove("is-busy"); }
}
