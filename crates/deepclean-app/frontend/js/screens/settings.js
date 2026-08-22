import { icon, ecoIcon } from '../icons.js';
import { escapeHtml, formatBytes, count } from '../format.js';
import { store } from '../store.js';
import { ALL_ECOSYSTEMS, ECO_NAMES } from '../actions.js';

/**
 * Paths hard-coded in `safety.rs`. Shown read-only because the user cannot
 * change them and should know they exist — the alternative is a blocklist
 * screen that quietly understates what Void refuses to touch.
 */
const PROTECTED = [
  '~/Documents · ~/Desktop · ~/Downloads · ~/Pictures',
  '~/Library/Keychains · ~/Library/Preferences',
  '.ssh · .gnupg · .aws · .config · .gitconfig · .zshrc · .bashrc · .env · .Trash',
];

const SECTIONS = ['Scanning', 'Ecosystems', 'Safety', 'Performance', 'General', 'About'];

function pathList(paths, removable, emptyText) {
  if (!paths.length) return `<div class="pathlist"><div class="pathrow"><span class="p dim">${escapeHtml(emptyText)}</span></div></div>`;
  return `<div class="pathlist">${paths.map((p, i) => `
    <div class="pathrow">
      ${icon('folder', 'icon icon-sm')}
      <span class="p" title="${escapeHtml(String(p))}">${escapeHtml(String(p))}</span>
      ${removable ? `<button class="iconbtn" style="width:22px;height:22px" data-act="remove-path" data-list="${removable}" data-index="${i}" aria-label="Remove">${icon('close', 'icon icon-sm')}</button>` : ''}
    </div>`).join('')}</div>`;
}

function scanningSection(cfg) {
  return `
    <div class="field">
      <div class="t-overline dim" style="margin-bottom:8px">Scan roots</div>
      <div class="t-body-sm help">Directories Void walks. Defaults to your home folder.</div>
      ${pathList(cfg.scan_roots, 'scan_roots', 'No scan roots — Void has nothing to walk.')}
      <button class="btn btn-secondary btn-sm" style="margin-top:8px" data-act="add-scan-root">${icon('plus', 'icon icon-sm')}Add folder…</button>
    </div>

    <div class="field">
      <div class="t-overline dim" style="margin-bottom:8px">Staleness</div>
      <div class="t-body-sm help">Flag artifacts untouched for longer than</div>
      <div style="display:flex;align-items:center;gap:8px">
        <input class="input tnum" type="number" min="1" max="365" style="width:80px"
          value="${cfg.staleness_threshold_days}" data-cfg="staleness_threshold_days">
        <span class="t-body" style="color:var(--text-secondary)">days</span>
      </div>
      <div class="t-caption foot">
        Affects the stale marker and the “Stale only” filter. It does not change what Void scans.
      </div>
    </div>`;
}

function ecosystemsSection(cfg) {
  const enabled = new Set(cfg.enabled_ecosystems);
  const stats = new Map();
  for (const item of store.items) {
    const s = stats.get(item.ecosystem) || { bytes: 0, n: 0 };
    s.bytes += item.size_bytes || 0;
    s.n += 1;
    stats.set(item.ecosystem, s);
  }
  // Sorted by what they actually cost, so the expensive ones carry the numbers
  // that justify the toggle.
  const ordered = [...ALL_ECOSYSTEMS].sort(
    (a, b) => (stats.get(b)?.bytes || 0) - (stats.get(a)?.bytes || 0)
  );

  return `
    <div class="field">
      <div style="display:flex;align-items:baseline;margin-bottom:8px">
        <span class="t-overline dim">Ecosystems</span>
        <span class="spacer"></span>
        <button class="btn btn-ghost btn-sm" data-act="eco-all">Enable all</button>
        <button class="btn btn-ghost btn-sm" data-act="eco-none">Disable all</button>
      </div>
      <div class="pathlist">
        ${ordered.map((eco) => {
          const s = stats.get(eco);
          return `<div class="ecorow">
            <span style="color:var(--eco-${eco});display:flex">${ecoIcon(eco, 'icon')}</span>
            <span class="nm t-body">${escapeHtml(ECO_NAMES[eco])}</span>
            <span class="stat t-caption">${s ? `${formatBytes(s.bytes)} · ${count(s.n)} item${s.n === 1 ? '' : 's'}` : 'no results yet'}</span>
            <span class="toggle${enabled.has(eco) ? ' is-on' : ''}" role="switch"
              aria-checked="${enabled.has(eco)}" data-act="toggle-eco" data-eco="${eco}"><i></i></span>
          </div>`;
        }).join('')}
      </div>
    </div>`;
}

function safetySection(cfg) {
  return `
    <div class="field">
      <div class="t-overline dim" style="margin-bottom:8px">Always protected</div>
      <div class="t-body-sm help">Void refuses to delete these, whatever else you configure.</div>
      <div class="pathlist">
        ${PROTECTED.map((p) => `<div class="pathrow is-locked">
          ${icon('lock', 'icon icon-sm')}<span class="p">${escapeHtml(p)}</span></div>`).join('')}
      </div>
      <div class="t-caption foot">
        Files inside Downloads, Documents, Desktop and Pictures can still be cleaned individually —
        it is the folders themselves that are off limits.
      </div>
    </div>

    <div class="field">
      <div class="t-overline dim" style="margin-bottom:8px">Never touch these</div>
      <div class="t-body-sm help">Your own additions. Anything inside them is left alone.</div>
      ${pathList(cfg.blocked_paths, 'blocked_paths', 'Add a folder Void must leave alone.')}
      <button class="btn btn-secondary btn-sm" style="margin-top:8px" data-act="add-blocked">${icon('plus', 'icon icon-sm')}Add folder…</button>
    </div>`;
}

function performanceSection(cfg) {
  const cores = navigator.hardwareConcurrency || 8;
  const slider = (label, key, min, max, value, suffix) => `
    <div class="field" style="margin-bottom:18px">
      <div class="slider-row">
        <span class="t-label">${escapeHtml(label)}</span>
        <span class="t-caption tnum dim">${value}${escapeHtml(suffix)}</span>
      </div>
      <input class="slider" type="range" min="${min}" max="${max}" value="${value}" data-cfg="${key}">
    </div>`;

  return `
    <div class="t-overline dim" style="margin-bottom:12px">Performance</div>
    ${slider('Walker threads', 'walker_threads', 1, cores, cfg.walker_threads, ` of ${cores} cores`)}
    ${slider('Max concurrent analyses', 'max_concurrent_analyses', 1, 32, cfg.max_concurrent_analyses, '')}
    ${slider('CPU threshold', 'cpu_threshold_percent', 10, 100, cfg.cpu_threshold_percent, '%')}
    <button class="btn btn-secondary btn-sm" data-act="restore-performance">Restore defaults</button>`;
}

function generalSection(cfg) {
  const ui = cfg.ui;
  const toggleRow = (label, help, key, on) => `
    <div class="ecorow" style="height:auto;padding:12px 10px">
      <span style="flex:1">
        <span class="t-body" style="display:block;color:var(--text-primary)">${escapeHtml(label)}</span>
        ${help ? `<span class="t-caption dim" style="display:block;margin-top:2px">${escapeHtml(help)}</span>` : ''}
      </span>
      <span class="toggle${on ? ' is-on' : ''}" role="switch" aria-checked="${on}" data-act="toggle-ui" data-key="${key}"><i></i></span>
    </div>`;

  const segRow = (label, key, options, value) => `
    <div class="ecorow" style="height:auto;padding:12px 10px">
      <span class="nm t-body">${escapeHtml(label)}</span>
      <span class="seg">${options.map(([v, l]) =>
        `<button class="${v === value ? 'is-on' : ''}" data-act="set-ui" data-key="${key}" data-value="${v}">${escapeHtml(l)}</button>`
      ).join('')}</span>
    </div>`;

  return `
    <div class="field">
      <div class="t-overline dim" style="margin-bottom:8px">General</div>
      <div class="pathlist">
        ${segRow('Theme', 'theme', [['system', 'System'], ['light', 'Light'], ['dark', 'Dark']], ui.theme)}
        ${segRow('Density', 'density', [['comfortable', 'Comfortable'], ['compact', 'Compact']], ui.density)}
        ${segRow('Group by', 'grouping', [['ecosystem', 'Ecosystem'], ['project', 'Project'], ['risk', 'Risk']], ui.grouping)}
        ${toggleRow('Confirm before cleaning', 'Turning this off skips the risk review entirely.', 'confirm_before_cleaning', ui.confirm_before_cleaning)}
        ${toggleRow('Prefer Trash where available', 'Choose the recoverable action when an item offers one.', 'prefer_trash', ui.prefer_trash)}
        ${toggleRow('Show menu bar icon', '', 'show_menu_bar_icon', ui.show_menu_bar_icon)}
      </div>
    </div>`;
}

function aboutSection() {
  return `
    <div class="field">
      <div class="t-overline dim" style="margin-bottom:8px">About</div>
      <dl class="facts t-body-sm">
        <dt>Version</dt><dd>1.0.0</dd>
        <dt>Settings file</dt><dd class="t-mono-sm">${escapeHtml(store.configPath || '—')}</dd>
        <dt>License</dt><dd>MIT</dd>
      </dl>
    </div>`;
}

export function renderSettings() {
  const cfg = store.config;
  if (!cfg) return '<div class="state"><div class="title">Loading settings…</div></div>';

  const active = store.settingsSection || 'Scanning';
  const body = {
    Scanning: scanningSection,
    Ecosystems: ecosystemsSection,
    Safety: safetySection,
    Performance: performanceSection,
    General: generalSection,
    About: aboutSection,
  }[active](cfg);

  return `
    <div class="cmdbar">
      <div class="cmdbar-titles">
        <div class="t-section-title" style="font-size:15px">Settings</div>
        <div class="t-caption sub">${store.settingsDirty ? 'Changes apply to your next scan' : 'Saved to disk as you change them'}</div>
      </div>
      <div class="cmdbar-actions">
        ${store.savedFlash ? `<span class="t-caption" style="color:var(--safe-text);display:flex;align-items:center;gap:4px">${icon('check', 'icon icon-sm')}Saved</span>` : ''}
      </div>
    </div>
    <div class="settings-nav">
      <div class="seg">${SECTIONS.map((s) =>
        `<button class="${s === active ? 'is-on' : ''}" data-act="settings-section" data-section="${s}">${s}</button>`
      ).join('')}</div>
    </div>
    <div class="settings-scroll" style="flex:1;overflow-y:auto"><div class="form">${body}</div></div>
    ${store.settingsDirty ? `<div class="statusbar t-caption">
      <span>Changes apply to your next scan.</span>
      <button class="btn btn-ghost btn-sm" style="height:20px" data-act="rescan-now">Rescan now</button>
      <span class="spacer"></span>
    </div>` : ''}`;
}
