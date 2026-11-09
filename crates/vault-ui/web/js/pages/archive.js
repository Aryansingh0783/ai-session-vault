/* Archive: search every session and read one. */

import { api, friendlyError } from "../api.js";
import { emptyState, errorState, loader, skeletonList } from "../ui.js";
import { $, $$, baseName, debounce, esc, ico } from "../util.js";

let query = "";
let current = null; // the conversation being read
let lastHit = null; // { source, id } of the last open attempt, for retry
let searchSeq = 0;

const hashParams = () => new URLSearchParams(location.hash.split("?")[1] || "");

export function archivePage() {
  const hp = hashParams();
  if (hp.has("q")) query = hp.get("q");
  return `<div class="toolbar"><input class="input input--grow" id="aq" placeholder="Search every Claude Code session and imported chat" value="${esc(query)}" aria-label="Search sessions"></div>
    <div class="arch"><div class="hits" id="hits"></div><div id="viewer">${viewerPlaceholder()}</div></div>`;
}

const viewerPlaceholder = () =>
  `<div class="viewer"><div class="empty">${loader("ball")}<h3>Pick a conversation</h3><p>Search above, then open a result to read it or copy it as a context pack for a new chat.</p></div></div>`;

function hitRow(h) {
  const active = current && current.id === h.id ? "is-active" : "";
  return `<button class="hit ${active}" data-action="open-hit" data-source="${esc(h.source)}" data-id="${esc(h.id)}">
    <div class="hit__top"><span>${esc(h.source)}${h.project ? " · " + esc(baseName(h.project)) : ""}</span><span>${esc(h.updated)}</span></div>
    <div class="hit__title">${esc(h.title || "Untitled")}</div><div class="hit__snip">${esc(h.snippet || h.messages + " messages")}</div></button>`;
}

export async function doSearch() {
  const mine = ++searchSeq;
  const hits = $("#hits");
  if (!hits) return;
  hits.innerHTML = skeletonList(3);
  try {
    const rows = await api("/api/search?q=" + encodeURIComponent(query));
    if (mine !== searchSeq || !$("#hits")) return;
    if (rows.length && hashParams().has("open") && !current) setTimeout(() => { const first = $(".hit"); if (first) openHit(first.dataset); }, 0);
    hits.innerHTML = rows.length
      ? rows.map(hitRow).join("")
      : emptyState("No matches", query ? "Try fewer or different words." : "Nothing has been archived on this device yet.", query ? { label: "Clear search", action: "clear-search" } : null);
  } catch (e) {
    if (mine === searchSeq && $("#hits")) hits.innerHTML = errorState(friendlyError(e), { action: "retry-search" }, "Search failed");
  }
}

function messageHtml(m) {
  const text = m.text.length > 6000 ? m.text.slice(0, 6000) + "\n… (shortened)" : m.text;
  return `<div class="msg ${m.role === "user" ? "user" : ""}"><small>${m.role === "user" ? "You" : "Assistant"}${m.time ? " · " + esc(m.time) : ""}</small>${esc(text)}</div>`;
}

function conversationHtml(c) {
  const more = c.messages.length > 400 ? `<p class="muted small">Showing the first 400 messages. The context pack keeps the first and the most recent ones.</p>` : "";
  return `<div class="viewer"><div class="viewer__head"><div><h2>${esc(c.title || "Untitled")}</h2><p class="small muted">${esc(c.source)} · ${c.messages.length} messages · ${esc(c.updated)}${c.project ? " · " + esc(c.project) : ""}</p></div>
    <button class="ids-btn ids-btn--dark" data-action="copy-pack">${ico("copy", "ids-icon--sm")} Copy as context pack</button></div>
    <div class="msgs">${c.messages.slice(0, 400).map(messageHtml).join("")}${more}</div></div>`;
}

/** Open a conversation. `ref` is { source, id } (a button's dataset works). */
export async function openHit(ref) {
  const { source, id } = ref;
  lastHit = { source, id };
  $$(".hit").forEach((h) => h.classList.toggle("is-active", h.dataset.id === id && h.dataset.source === source));
  const viewer = $("#viewer");
  viewer.innerHTML = `<div class="viewer">${skeletonList(3)}</div>`;
  try {
    current = await api(`/api/conversation?source=${encodeURIComponent(source)}&id=${encodeURIComponent(id)}`);
    viewer.innerHTML = conversationHtml(current);
  } catch (e) {
    viewer.innerHTML = `<div class="viewer">${errorState(friendlyError(e), { action: "retry-open" }, "Couldn't open it")}</div>`;
  }
}

export const retryOpen = () => lastHit && openHit(lastHit);
export const currentConversation = () => current;

export function clearSearch() {
  query = "";
  const box = $("#aq");
  if (box) box.value = "";
  doSearch();
}

export function archiveAfter() {
  $("#aq").addEventListener("input", debounce((e) => { query = e.target.value; doSearch(); }, 300));
  doSearch();
}
