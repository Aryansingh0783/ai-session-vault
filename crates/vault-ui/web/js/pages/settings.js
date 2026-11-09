/* Settings: vault info, themes, exclusions, device-local keys, folders. */

import { S } from "../state.js";
import { esc, ico } from "../util.js";

function themePicker() {
  const themes = window.VaultTheme ? window.VaultTheme.themes : [];
  const active = window.VaultTheme ? window.VaultTheme.get() : "auto";
  const button = (t) => `<button class="theme ${active === t.id ? "is-active" : ""}" role="radio" aria-checked="${active === t.id}" data-action="set-theme" data-theme-id="${esc(t.id)}">
    <span class="theme__sw" data-sw="${esc(t.id)}"><i></i><i></i><i></i><i></i></span><span class="theme__name">${esc(t.name)}<small>${esc(t.note)}</small></span></button>`;
  return `<div class="themes" role="radiogroup" aria-label="Theme">${themes.map(button).join("")}</div>`;
}

function exclusionTags(list) {
  if (!list.length) return `<span class="small muted">Nothing excluded.</span>`;
  return list.map((p) => `<span class="tag">${esc(p)}<button data-action="unexclude" data-path="${esc(p)}" aria-label="Stop excluding ${esc(p)}">${ico("x")}</button></span>`).join("");
}

export function settingsPage() {
  const s = S.settings;
  return `<div class="stack"><section class="card ids-card"><h2 class="card__title">Vault</h2>
      <dl class="kv kv--left mt-14"><dt>Folder</dt><dd class="mono">${esc(s.vault_path)}</dd><dt>Vault id</dt><dd class="mono">${esc(s.vault_id)}</dd><dt>Sync engine</dt><dd>${s.engine ? "Running" : "Not running (read-only view)"}</dd></dl></section>
    <section class="card ids-card"><h2 class="card__title">Appearance</h2><p class="small muted mt-6">Saved on this device only.</p>${themePicker()}</section>
    <section class="card ids-card"><h2 class="card__title">Never upload these projects</h2><p class="small muted mt-6">Plugin-generated or noisy projects. Anything already in the vault stays there.</p>
      <div class="tags">${exclusionTags(s.exclude_projects)}</div>
      <form class="inline" data-form="exclude"><input class="input input--grow" name="path" placeholder="~/.claude-mem/observer-sessions" aria-label="Project path to exclude" maxlength="1024" required><button class="ids-btn ids-btn--dark" type="submit">Exclude</button></form></section>
    <section class="card ids-card"><h2 class="card__title">Kept on each device</h2><p class="small muted mt-6">These <span class="mono">settings.json</span> keys hold machine-specific commands or paths, so they never leave the device they belong to.</p>
      <div class="tags">${s.local_keys.map((k) => `<span class="tag is-fixed mono">${esc(k)}</span>`).join("")}</div></section>
    <section class="card ids-card"><h2 class="card__title">Folders on this device</h2>
      <dl class="kv kv--left mt-14"><dt>Claude Code</dt><dd class="mono">${esc(s.claude_dir)}</dd><dt>Codex</dt><dd class="mono">${esc(s.codex_dir)}</dd><dt>Vault state</dt><dd class="mono">${esc(s.state_dir)}</dd></dl></section></div>`;
}
