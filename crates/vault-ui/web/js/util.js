/* Small pure helpers: DOM lookups, escaping, formatting, local preferences. */

export const $ = (s, r = document) => r.querySelector(s);
export const $$ = (s, r = document) => Array.from(r.querySelectorAll(s));

const ENTITIES = { "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" };
export const esc = (s) => String(s ?? "").replace(/[&<>"']/g, (c) => ENTITIES[c]);

export const ico = (name, cls = "") => `<svg class="ids-icon ${cls}" aria-hidden="true"><use href="#i-${name}"/></svg>`;

export function fmtBytes(n) {
  n = Number(n) || 0;
  if (n < 1024) return n + " B";
  const units = ["KB", "MB", "GB", "TB"];
  let i = -1;
  do { n /= 1024; i++; } while (n >= 1024 && i < units.length - 1);
  return (n >= 100 ? n.toFixed(0) : n.toFixed(1)) + " " + units[i];
}

export const num = (n) => Number(n || 0).toLocaleString();
export const plural = (n, one, many = one + "s") => `${num(n)} ${n === 1 ? one : many}`;

export function ago(t) {
  const ms = typeof t === "number" ? t : Date.parse(t);
  if (!ms) return "never";
  const s = Math.max(0, (Date.now() - ms) / 1000);
  if (s < 45) return "just now";
  if (s < 3600) return Math.round(s / 60) + " min ago";
  if (s < 86400) return Math.round(s / 3600) + " h ago";
  return Math.round(s / 86400) + " d ago";
}

/** A short display name for a portable project path. */
export function baseName(p) {
  if (!p) return "Unknown";
  if (p.startsWith("raw:")) return p.slice(4).split("-").filter(Boolean).slice(-2).join("-") || p.slice(4);
  const parts = p.replace(/\\/g, "/").split("/").filter(Boolean);
  const last = parts[parts.length - 1] || p;
  return last === "~" ? "Home folder" : last;
}

/** First sentence of a long explanation, capped so it fits a card. */
export function firstSentence(text) {
  const m = String(text).match(/^.*?[.!?](\s|$)/);
  const s = (m ? m[0] : String(text)).trim();
  return s.length > 150 ? s.slice(0, 147).replace(/\s+\S*$/, "") + "…" : s;
}

export function debounce(fn, ms) {
  let t;
  return (...args) => { clearTimeout(t); t = setTimeout(() => fn(...args), ms); };
}

/* Per-viewer preferences. Storage can be unavailable (private windows, blocked site data); the app
   works without it, so failures here are intentionally not reported to the user. */
export const prefs = {
  get(key, fallback) {
    try { return JSON.parse(localStorage.getItem(key)) ?? fallback; } catch (_) { return fallback; }
  },
  set(key, value) {
    try { localStorage.setItem(key, JSON.stringify(value)); } catch (_) { /* optional */ }
  },
};
