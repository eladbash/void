import * as api from './api.js';
import { icon, MARK } from './icons.js';
import { setHome, escapeHtml, formatBytes, count } from './format.js';
import {
  store, visibleItems, selectionSummary, resetFilters, recordIssue, filtersActive, presetFor,
} from './store.js';
import {
  selectedAction, effectiveRisk, ALL_ECOSYSTEMS, ECO_NAMES, kindLabel, isActionable,
} from './actions.js';
import { renderResults, groupsFor } from './screens/results.js';
import { renderDrawer, renderConfirm, renderIssues, buildPlan } from './screens/panels.js';
import { renderSettings } from './screens/settings.js';
import { renderHistory } from './screens/history.js';
import { renderAgents } from './screens/agents.js';
import { installSprite } from './icons.js';

const root = document.getElementById('root');

/* ── Render ──────────────────────────────────────────────────────────── */

function rail() {
  const btn = (route, ico, label, key) => `
    <button class="rail-btn" data-act="route" data-route="${route}"
      ${store.route === route ? 'aria-current="page"' : ''} title="${label} (${key})" aria-label="${label}">
      ${icon(ico, 'icon icon-lg')}
      ${route === 'results' && store.items.length && store.route !== 'results' ? '<span class="rail-dot"></span>' : ''}
    </button>`;

  const disk = store.disk;
  const reclaim = store.items.reduce((s, i) => s + (i.size_bytes || 0), 0);
  const reclPct = disk?.total_bytes ? Math.min(100, (reclaim / disk.total_bytes) * 100) : 0;
  const usedPct = disk?.total_bytes ? Math.min(100, (disk.used_bytes / disk.total_bytes) * 100) : 0;

  return `<nav class="rail" aria-label="Sections">
    <div class="rail-mark${store.scanning ? ' is-scanning' : ''}" aria-hidden="true">${MARK}</div>
    ${btn('results', 'sidebar-toggle', 'Results', '⌘1')}
    ${btn('agents', 'sparkle', 'Agents', '⌘2')}
    ${btn('history', 'history', 'History', '⌘3')}
    ${btn('settings', 'settings', 'Settings', '⌘4')}
    <span class="spacer"></span>
    ${disk ? `<div class="rail-disk" title="${escapeHtml(formatBytes(disk.available_bytes))} free of ${escapeHtml(formatBytes(disk.total_bytes))}">
      <span class="rail-disk-bar">
        <i class="recl" style="height:${reclPct.toFixed(1)}%"></i>
        <i class="used" style="height:${Math.max(0, usedPct - reclPct).toFixed(1)}%"></i>
      </span>
      <span class="t-overline tnum">${Math.round(usedPct)}%</span>
    </div>` : ''}
  </nav>`;
}

/* Scroll containers that must survive a re-render. Rendering replaces the
   whole subtree, so without this a checkbox click would send the list back to
   the top — the row you just ticked jumps out from under the cursor. */
const SCROLLERS = ['#list', '.drawer-body', '.settings-scroll', '.agents-scroll'];

function captureScroll() {
  const saved = new Map();
  for (const sel of SCROLLERS) {
    const el = root.querySelector(sel);
    if (el && el.scrollTop) saved.set(sel, el.scrollTop);
  }
  return saved;
}

function restoreScroll(saved) {
  for (const [sel, top] of saved) {
    const el = root.querySelector(sel);
    // A shorter list clamps this automatically.
    if (el) el.scrollTop = top;
  }
}

/* A settings field being typed into must keep focus and caret across the
   re-render its own save triggers, or every keystroke would blur it. */
function captureFocus() {
  const el = document.activeElement;
  if (!el || !root.contains(el) || el.tagName !== 'INPUT') return null;
  const sel = el.dataset.cfgpath ? `[data-cfgpath="${el.dataset.cfgpath}"]`
    : el.dataset.cfg ? `[data-cfg="${el.dataset.cfg}"]` : null;
  if (!sel) return null;
  let range = null;
  try { range = [el.selectionStart, el.selectionEnd]; } catch { /* number inputs have none */ }
  return { sel, range };
}

function restoreFocus(saved) {
  if (!saved) return;
  const el = root.querySelector(saved.sel);
  if (!el) return;
  el.focus();
  try { if (saved.range && saved.range[0] != null) el.setSelectionRange(...saved.range); } catch { /* not a text field */ }
}

function render() {
  const scroll = captureScroll();
  const focus = captureFocus();

  const screen =
    store.route === 'settings' ? renderSettings()
    : store.route === 'history' ? renderHistory()
    : store.route === 'agents' ? renderAgents()
    : renderResults();

  const overlays =
    (store.showConfirm ? renderConfirm(buildPlan()) : '') +
    (store.showIssues ? renderIssues() : '') +
    (store.toast ? toastHtml() : '');

  root.className = `app${store.drawerId ? ' has-drawer' : ''}`;
  root.innerHTML = `${rail()}<main class="screen">${screen}</main>${
    store.route === 'results' && store.drawerId ? renderDrawer() : ''
  }${overlays}`;

  restoreScroll(scroll);
  restoreFocus(focus);

  // Preserve the caret in the filter field across re-renders.
  if (store.focusFilter) {
    const input = document.getElementById('filterInput');
    if (input) { input.focus(); input.setSelectionRange(input.value.length, input.value.length); }
    store.focusFilter = false;
  }
}

function toastHtml() {
  return `<div class="toast">
    <span class="t-body-sm" style="flex:1">${escapeHtml(store.toast.text)}</span>
    ${store.toast.undo ? '<button class="btn btn-ghost btn-sm" data-act="undo-toast">Undo</button>' : ''}
    <button class="iconbtn" style="width:22px;height:22px" data-act="dismiss-toast" aria-label="Dismiss">${icon('close', 'icon icon-sm')}</button>
  </div>`;
}

/**
 * Coalesced re-render.
 *
 * During a scan, items arrive faster than they can be read. The old UI rebuilt
 * the entire list on every `item_found`, which is O(n^2) across a scan; this
 * batches into an animation frame and caps the rate while streaming.
 */
let frame = 0;
let lastPaint = 0;
function scheduleRender() {
  if (frame) return;
  frame = requestAnimationFrame(() => {
    frame = 0;
    const now = performance.now();
    if (store.scanning && now - lastPaint < 250) {
      setTimeout(scheduleRender, 250 - (now - lastPaint));
      return;
    }
    lastPaint = now;
    render();
  });
}

/** Send the list back to the top — for changes that replace what it contains. */
function resetScroll() {
  const el = root.querySelector('#list');
  if (el) el.scrollTop = 0;
}

function toast(text, undo = null) {
  store.toast = { text, undo };
  scheduleRender();
  clearTimeout(toast._t);
  toast._t = setTimeout(() => { store.toast = null; scheduleRender(); }, 5000);
}

/* ── Config ──────────────────────────────────────────────────────────── */

let saveTimer = 0;
function saveConfig({ dirty = false, render: rerender = true } = {}) {
  if (dirty) store.settingsDirty = true;
  applyPreferences();
  clearTimeout(saveTimer);
  saveTimer = setTimeout(async () => {
    try {
      await api.updateConfig(store.config);
      store.savedFlash = true;
      scheduleRender();
      setTimeout(() => { store.savedFlash = false; scheduleRender(); }, 1500);
    } catch (err) {
      toast(String(err));
    }
  }, 400);
  if (rerender) scheduleRender();
}

function applyPreferences() {
  const ui = store.config?.ui;
  if (!ui) return;
  const el = document.documentElement;
  if (ui.theme === 'system') el.removeAttribute('data-theme');
  else el.setAttribute('data-theme', ui.theme);
  el.setAttribute('data-density', ui.density);
}

/* ── Scanning ────────────────────────────────────────────────────────── */

async function startScan() {
  if (store.scanning) return;
  store.scanning = true;
  store.hasScanned = true;
  store.items = [];
  store.selected.clear();
  store.overrides.clear();
  store.issues.clear();
  store.deniedPaths.clear();
  store.pathsScanned = 0;
  store.lastResult = null;
  store.clean = null;
  store.scannerStatus = new Map(
    (store.config?.enabled_ecosystems || ALL_ECOSYSTEMS).map((e) => [e, 'pending'])
  );
  store.settingsDirty = false;
  render();

  try {
    await api.startScan();
  } catch (err) {
    store.scanning = false;
    recordIssue(String(err));
    scheduleRender();
  }
}

function onScanEvent(ev) {
  switch (ev.type) {
    case 'scanner_started':
      store.scannerStatus.set(ev.ecosystem, 'running');
      break;
    case 'scanner_completed':
      store.scannerStatus.set(ev.ecosystem, { done: ev.items_found });
      break;
    case 'progress':
      store.pathsScanned = ev.paths_scanned;
      break;
    case 'item_found':
      store.items.push(ev.item);
      break;
    case 'permission_denied':
      store.deniedPaths.add(String(ev.path));
      break;
    case 'error':
      recordIssue(ev.message);
      break;
    case 'scan_complete':
      store.scanning = false;
      store.scanDurationMs = ev.summary?.scan_duration_ms || 0;
      store.lastScanAt = new Date().toISOString();
      for (const [eco, status] of store.scannerStatus) {
        if (status !== 'pending' && typeof status !== 'object') {
          store.scannerStatus.set(eco, { done: store.items.filter((i) => i.ecosystem === eco).length });
        }
      }
      refreshDisk();
      refreshAgentUsage();
      break;
  }
  scheduleRender();
}

/* ── Agents screen data ─────────────────────────────────────────────── */

async function refreshAgentUsage() {
  try { store.agentUsage = await api.getAgentUsage(); } catch { store.agentUsage = null; }
  scheduleRender();
}

async function refreshGuard() {
  try { store.guard = await api.getGuardStatus(); } catch { /* shown as unavailable */ }
  scheduleRender();
}

async function refreshIntegrations() {
  try { store.hooks = await api.getHookStatus(); store.hooksError = null; } catch (err) { store.hooksError = String(err); }
  try { store.mcp = await api.getMcpSnippet(); store.mcpError = null; } catch (err) { store.mcp = null; store.mcpError = String(err); }
  scheduleRender();
}

function refreshAgents() {
  refreshAgentUsage();
  refreshGuard();
  refreshIntegrations();
}

/** Jump to Results showing one quick filter, e.g. idle worktrees. */
function applyPreset(id) {
  const preset = presetFor(id);
  if (!preset) return;
  resetFilters();
  store.preset = preset;
  store.route = 'results';
  store.drawerId = null;
  resetScroll();
  scheduleRender();
}

/** Set a value in the config by dotted path (`guard.auto_clean`). */
function configRef(path) {
  const keys = path.split('.');
  const last = keys.pop();
  let obj = store.config;
  for (const k of keys) {
    if (obj[k] == null || typeof obj[k] !== 'object') obj[k] = {};
    obj = obj[k];
  }
  return { obj, key: last };
}

/** Settings under `ai.` change what a scan finds; the rest do not. */
const affectsScan = (path) => path.startsWith('ai.') || path === 'scan_roots' || path === 'enabled_ecosystems';

async function refreshDisk() {
  try { store.disk = await api.getDiskUsage(); } catch { store.disk = null; }
  scheduleRender();
}

/* ── Cleaning ────────────────────────────────────────────────────────── */

async function runClean(entries) {
  const selections = entries.map(({ item, action }) => ({ item_id: item.id, action_id: action.id }));
  if (!selections.length) return;

  store.showConfirm = false;
  store.clean = {
    total: selections.length,
    completed: 0,
    failed: 0,
    bytes: 0,
    current: null,
    states: new Map(entries.map(({ item }) => [item.id, { state: 'queued' }])),
    startedAt: Date.now(),
  };
  render();

  try {
    await api.executeClean(selections);
  } catch (err) {
    store.clean = null;
    toast(String(err));
    scheduleRender();
  }
}

function onActionEvent(ev) {
  const c = store.clean;
  if (!c) return;

  switch (ev.type) {
    case 'started': {
      c.current = ev.label;
      for (const [id, s] of c.states) {
        if (s.state === 'queued' && !s.claimed) { s.state = 'active'; s.claimed = true; break; }
        if (id) continue;
      }
      break;
    }
    case 'completed': {
      const id = ev.item_id;
      c.completed += 1;
      c.bytes += ev.bytes_freed > 0 ? ev.bytes_freed : ev.estimated_bytes;
      c.states.set(id, { state: 'done', claimed: true });
      // Drop cleaned items so the list reflects reality without a rescan.
      store.items = store.items.filter((i) => i.id !== id);
      store.selected.delete(id);
      break;
    }
    case 'failed': {
      const id = ev.item_id;
      const item = store.items.find((i) => i.id === id);
      c.failed += 1;
      c.states.set(id, {
        state: 'failed',
        claimed: true,
        error: ev.error,
        // The single most likely real failure: a GUI-launched app has a bare
        // PATH, so `cargo`/`npm`/`go` may not resolve. Offer the direct
        // filesystem action instead of making the user hunt for it.
        canFallback: /was not found in PATH/.test(ev.error)
          && (item?.available_actions || []).some((a) => a.method.type === 'remove_dir'),
      });
      break;
    }
    case 'batch_complete': {
      store.lastResult = {
        total: ev.total,
        succeeded: ev.succeeded,
        failed: ev.failed,
        bytes_freed: ev.bytes_freed,
        estimated: ev.estimated,
        cancelled: ev.cancelled,
        duration_ms: Date.now() - c.startedAt,
      };
      store.clean = null;
      refreshHistory();
      refreshDisk();
      break;
    }
  }
  scheduleRender();
}

async function refreshHistory() {
  try { store.history = await api.getHistory(); } catch { /* history is best-effort */ }
  scheduleRender();
}

/* ── Interaction ─────────────────────────────────────────────────────── */

function itemsInOrder() {
  return groupsFor(visibleItems()).flatMap((g) => (store.collapsed.has(g.key) ? [] : g.items));
}

function toggleSelect(id) {
  if (store.selected.has(id)) store.selected.delete(id);
  else store.selected.add(id);
  scheduleRender();
}

async function pickFolder() {
  const dialog = window.__TAURI__?.dialog;
  if (!dialog?.open) {
    toast('Folder picker unavailable in this build.');
    return null;
  }
  const picked = await dialog.open({ directory: true, multiple: false });
  return typeof picked === 'string' ? picked : null;
}

const ACTIONS = {
  route: (el) => {
    store.route = el.dataset.route;
    if (store.route === 'history') refreshHistory();
    if (store.route === 'agents') refreshAgents();
    scheduleRender();
  },
  'goto-settings-section': (el) => { store.route = 'settings'; store.settingsSection = el.dataset.section; scheduleRender(); },

  'guard-check': async () => {
    store.guardChecking = true;
    scheduleRender();
    try { store.guard = await api.runGuardNow(); } catch (err) { toast(String(err)); }
    store.guardChecking = false;
    scheduleRender();
  },
  preset: (el) => applyPreset(el.dataset.preset),
  'clear-preset': () => { store.preset = null; resetScroll(); scheduleRender(); },
  'hooks-install': async () => {
    try { store.hooks = await api.installHooks(); store.hooksError = null; toast('Claude Code hooks installed'); }
    catch (err) { store.hooksError = String(err); }
    scheduleRender();
  },
  'hooks-uninstall': async () => {
    try { store.hooks = await api.uninstallHooks(); store.hooksError = null; toast('Claude Code hooks removed'); }
    catch (err) { store.hooksError = String(err); }
    scheduleRender();
  },
  'copy-text': (el) => { navigator.clipboard?.writeText(el.dataset.text || ''); toast('Copied'); },
  'goto-settings': () => { store.route = 'settings'; scheduleRender(); },
  'goto-results': () => { store.route = 'results'; scheduleRender(); },
  'goto-history': () => { store.route = 'history'; refreshHistory(); scheduleRender(); },

  scan: () => startScan(),
  'rescan-now': () => { store.route = 'results'; startScan(); },

  toggle: (el) => {
    const item = store.items.find((i) => i.id === el.dataset.id);
    if (item && !isActionable(item)) return;
    toggleSelect(el.dataset.id);
  },
  open: (el) => { store.drawerId = el.dataset.id; scheduleRender(); },
  'close-drawer': () => { store.drawerId = null; scheduleRender(); },

  'prev-item': () => moveDrawer(-1),
  'next-item': () => moveDrawer(1),

  'toggle-group': (el) => {
    const key = el.dataset.group;
    const group = groupsFor(visibleItems()).find((g) => String(g.key) === key);
    if (!group) return;
    const selectable = group.items.filter(isActionable);
    const all = selectable.every((i) => store.selected.has(i.id));
    for (const i of selectable) {
      if (all) store.selected.delete(i.id);
      else store.selected.add(i.id);
    }
    scheduleRender();
  },
  collapse: (el) => {
    const key = el.dataset.group;
    if (store.collapsed.has(key)) store.collapsed.delete(key);
    else store.collapsed.add(key);
    scheduleRender();
  },
  'expand-all': () => {
    const keys = groupsFor(visibleItems()).map((g) => String(g.key));
    if (store.collapsed.size) store.collapsed.clear();
    else keys.forEach((k) => store.collapsed.add(k));
    scheduleRender();
  },

  'select-safe': () => {
    for (const item of visibleItems()) {
      if (isActionable(item) && effectiveRisk(item, store.overrides, store.config?.ui?.prefer_trash) === 'safe') {
        store.selected.add(item.id);
      }
    }
    scheduleRender();
  },
  'clear-filters': () => { resetFilters(); resetScroll(); scheduleRender(); },
  'clear-search': () => { store.filter = ''; resetScroll(); scheduleRender(); },

  review: () => { store.showConfirm = true; scheduleRender(); },
  'close-confirm': () => { store.showConfirm = false; scheduleRender(); },
  'do-clean': () => runClean(buildPlan().entries),
  'clean-one': (el) => {
    const item = store.items.find((i) => i.id === el.dataset.id);
    const action = item && selectedAction(item, store.overrides, store.config?.ui?.prefer_trash);
    if (item && action) { store.drawerId = null; runClean([{ item, action }]); }
  },
  'cancel-clean': async () => { await api.cancelClean(); toast('Stopping after the current item…'); },
  'dismiss-result': () => { store.lastResult = null; scheduleRender(); },

  'toggle-trash': () => {
    store.config.ui.prefer_trash = !store.config.ui.prefer_trash;
    saveConfig();
  },

  'choose-action': (el) => {
    store.overrides.set(el.dataset.id, el.dataset.action);
    scheduleRender();
  },
  actions: (el) => { store.drawerId = el.dataset.id; scheduleRender(); },
  fallback: (el) => {
    const item = store.items.find((i) => i.id === el.dataset.id);
    const alt = (item?.available_actions || []).find((a) => a.method.type === 'remove_dir');
    if (item && alt) {
      store.overrides.set(item.id, alt.id);
      runClean([{ item, action: alt }]);
    }
  },

  'copy-path': (el) => {
    const item = store.items.find((i) => i.id === el.dataset.id);
    if (item) { navigator.clipboard?.writeText(String(item.path)); toast('Path copied'); }
  },
  reveal: async (el) => {
    try {
      await api.revealItem(el.dataset.id);
    } catch (err) {
      toast(String(err));
    }
  },
  'exclude-path': (el) => {
    const item = store.items.find((i) => i.id === el.dataset.id);
    if (!item) return;
    const before = [...store.config.blocked_paths];
    store.config.blocked_paths.push(item.path);
    store.items = store.items.filter((i) => i.id !== item.id);
    store.selected.delete(item.id);
    store.drawerId = null;
    saveConfig();
    toast(`${kindLabel(item)} excluded`, () => {
      store.config.blocked_paths = before;
      store.items.push(item);
      saveConfig();
    });
  },
  'undo-toast': () => {
    store.toast?.undo?.();
    store.toast = null;
    scheduleRender();
  },
  'dismiss-toast': () => { store.toast = null; scheduleRender(); },

  'show-issues': () => { store.showIssues = true; scheduleRender(); },
  'close-issues': () => { store.showIssues = false; scheduleRender(); },
  'dismiss-banner': () => { store.deniedPaths.clear(); scheduleRender(); },
  'copy-issues': () => {
    const text = [...store.deniedPaths, ...store.issues.keys()].join('\n');
    navigator.clipboard?.writeText(text);
    toast('Copied');
  },

  density: () => {
    const ui = store.config.ui;
    ui.density = ui.density === 'compact' ? 'comfortable' : 'compact';
    saveConfig();
  },
  group: () => {
    const order = ['ecosystem', 'project', 'risk'];
    const ui = store.config.ui;
    ui.grouping = order[(order.indexOf(ui.grouping) + 1) % order.length];
    store.collapsed.clear();
    resetScroll();
    saveConfig();
  },
  sort: () => {
    const order = ['size', 'stale', 'name', 'risk'];
    store.sort = order[(order.indexOf(store.sort) + 1) % order.length];
    resetScroll();
    scheduleRender();
  },
  filters: () => {
    // Cycles the one filter that matters most without a popover: large items.
    store.largeOnly = !store.largeOnly;
    scheduleRender();
  },

  'settings-section': (el) => { store.settingsSection = el.dataset.section; scheduleRender(); },
  'toggle-eco': (el) => {
    const eco = el.dataset.eco;
    const list = store.config.enabled_ecosystems;
    const i = list.indexOf(eco);
    if (i >= 0) list.splice(i, 1); else list.push(eco);
    saveConfig({ dirty: true });
  },
  'eco-all': () => { store.config.enabled_ecosystems = [...ALL_ECOSYSTEMS]; saveConfig({ dirty: true }); },
  'eco-none': () => { store.config.enabled_ecosystems = []; saveConfig({ dirty: true }); },
  'toggle-ui': (el) => {
    const key = el.dataset.key;
    store.config.ui[key] = !store.config.ui[key];
    saveConfig();
  },
  'set-ui': (el) => {
    store.config.ui[el.dataset.key] = el.dataset.value;
    saveConfig();
  },
  'add-scan-root': async () => {
    const dir = await pickFolder();
    if (dir) { store.config.scan_roots.push(dir); saveConfig({ dirty: true }); }
  },
  'add-worktree-root': async () => {
    const dir = await pickFolder();
    if (dir) {
      const { obj, key } = configRef('ai.extra_worktree_roots');
      obj[key] = [...(obj[key] || []), dir];
      saveConfig({ dirty: true });
    }
  },
  'toggle-cfg': (el) => {
    const path = el.dataset.path;
    const { obj, key } = configRef(path);
    obj[key] = !obj[key];
    saveConfig({ dirty: affectsScan(path) });
  },
  'toggle-policy': (el) => {
    const policy = store.config.guard?.policies?.[Number(el.dataset.index)];
    if (!policy) return;
    policy.enabled = !policy.enabled;
    saveConfig();
  },
  'toggle-login': async () => {
    const next = !store.config.ui.launch_at_login;
    try {
      await api.setLaunchAtLogin(next);
      store.config.ui.launch_at_login = next;
    } catch (err) {
      toast(String(err));
    }
    scheduleRender();
  },
  'add-blocked': async () => {
    const dir = await pickFolder();
    if (dir) { store.config.blocked_paths.push(dir); saveConfig(); }
  },
  'remove-path': (el) => {
    const list = el.dataset.list;
    const { obj, key } = configRef(list);
    (obj[key] || []).splice(Number(el.dataset.index), 1);
    saveConfig({ dirty: affectsScan(list) });
  },
  'restore-performance': () => {
    const cores = navigator.hardwareConcurrency || 8;
    Object.assign(store.config, {
      walker_threads: Math.max(1, Math.floor(cores / 2)),
      max_concurrent_analyses: 8,
      cpu_threshold_percent: 70,
    });
    saveConfig();
  },

  'open-run': (el) => { store.openRunId = el.dataset.run; scheduleRender(); },
  'close-run': () => { store.openRunId = null; scheduleRender(); },
  'clear-history': async () => {
    await api.clearHistory();
    await refreshHistory();
    toast('History cleared');
  },

  overflow: () => toast('Rescan with ⌘R · settings with ⌘,'),
};

root.addEventListener('click', (e) => {
  const el = e.target.closest('[data-act]');
  if (!el) return;
  // Clicking the backdrop closes; a click that merely bubbled up to it from
  // inside the panel does not. Checking the scrim itself rather than "is this
  // a close action" matters: an X button contains an icon, so the click lands
  // on the <svg> and never equals the button.
  if (el.classList.contains('scrim') && e.target !== el) return;
  const fn = ACTIONS[el.dataset.act];
  if (fn) { e.preventDefault(); fn(el); }
});

root.addEventListener('change', (e) => {
  if (e.target.type === 'range' && e.target.dataset.cfgpath) scheduleRender();
});

root.addEventListener('input', (e) => {
  const t = e.target;
  if (t.id === 'filterInput') {
    store.filter = t.value;
    store.focusFilter = true;
    resetScroll();
    scheduleRender();
    return;
  }
  if (t.id === 'confirmInput') {
    const btn = document.getElementById('confirmBtn');
    if (btn) btn.disabled = t.value !== 'delete';
    return;
  }
  const path = t.dataset.cfgpath;
  if (path) {
    const n = Number(t.value);
    if (t.value === '' || !Number.isFinite(n)) return;
    const { obj, key } = configRef(path);
    obj[key] = n;
    // Re-rendering mid-drag would replace the slider under the pointer;
    // update its readout in place and render when the drag ends.
    const isRange = t.type === 'range';
    if (isRange) {
      const out = t.closest('.field')?.querySelector('.slider-row .t-caption');
      if (out) out.textContent = `below ${n}% free`;
    }
    saveConfig({ dirty: affectsScan(path), render: !isRange });
    return;
  }
  const key = t.dataset.cfg;
  if (key) {
    store.config[key] = Number(t.value);
    saveConfig({ dirty: key === 'staleness_threshold_days' ? false : true });
  }
});

function moveDrawer(delta) {
  const order = itemsInOrder();
  const i = order.findIndex((x) => x.id === store.drawerId);
  const next = order[i + delta];
  if (next) { store.drawerId = next.id; scheduleRender(); }
}

document.addEventListener('keydown', (e) => {
  const mod = e.metaKey || e.ctrlKey;
  const typing = /^(INPUT|TEXTAREA)$/.test(e.target.tagName);

  if (mod && e.key === '1') { store.route = 'results'; scheduleRender(); return e.preventDefault(); }
  if (mod && e.key === '2') { store.route = 'agents'; refreshAgents(); scheduleRender(); return e.preventDefault(); }
  if (mod && e.key === '3') { store.route = 'history'; refreshHistory(); scheduleRender(); return e.preventDefault(); }
  if ((mod && e.key === '4') || (mod && e.key === ',')) { store.route = 'settings'; scheduleRender(); return e.preventDefault(); }
  if (mod && e.key.toLowerCase() === 'r') { startScan(); return e.preventDefault(); }

  if (e.key === 'Escape') {
    if (store.showConfirm) store.showConfirm = false;
    else if (store.showIssues) store.showIssues = false;
    else if (store.openRunId) store.openRunId = null;
    else if (store.drawerId) store.drawerId = null;
    else if (typing) { store.filter = ''; e.target.blur(); }
    else if (store.selected.size) store.selected.clear();
    scheduleRender();
    return;
  }

  if (store.route !== 'results') return;

  if (mod && e.key === 'Enter' && store.selected.size) { store.showConfirm = true; scheduleRender(); return e.preventDefault(); }
  if ((mod && e.key.toLowerCase() === 'f') || (!typing && e.key === '/')) {
    e.preventDefault();
    store.focusFilter = true;
    scheduleRender();
    return;
  }
  if (typing) return;

  if (mod && e.key.toLowerCase() === 'a') {
    e.preventDefault();
    if (e.altKey) ACTIONS['select-safe']();
    else if (e.shiftKey) { store.selected.clear(); scheduleRender(); }
    else { visibleItems().filter(isActionable).forEach((i) => store.selected.add(i.id)); scheduleRender(); }
    return;
  }
  if (mod && e.key.toLowerCase() === 'd') { e.preventDefault(); return ACTIONS.density(); }
  if (mod && e.shiftKey && e.key.toLowerCase() === 'e') { e.preventDefault(); return ACTIONS['expand-all'](); }

  const order = itemsInOrder();
  if (!order.length) return;
  const i = order.findIndex((x) => x.id === store.focusedId);

  if (e.key === 'ArrowDown') { e.preventDefault(); store.focusedId = order[Math.min(order.length - 1, i + 1)]?.id ?? order[0].id; }
  else if (e.key === 'ArrowUp') { e.preventDefault(); store.focusedId = order[Math.max(0, i - 1)]?.id ?? order[0].id; }
  else if (e.key === ' ' && store.focusedId) { e.preventDefault(); toggleSelect(store.focusedId); return; }
  else if (e.key === 'Enter' && store.focusedId) { e.preventDefault(); store.drawerId = store.focusedId; }
  else if (e.altKey && store.drawerId && e.key === 'ArrowDown') { e.preventDefault(); return moveDrawer(1); }
  else if (e.altKey && store.drawerId && e.key === 'ArrowUp') { e.preventDefault(); return moveDrawer(-1); }
  else return;

  scheduleRender();
  document.querySelector('.item.is-focused')?.scrollIntoView({ block: 'nearest' });
});

/* ── Boot ────────────────────────────────────────────────────────────── */

async function boot() {
  installSprite();

  const platform = navigator.userAgent.includes('Mac') ? 'macos'
    : navigator.userAgent.includes('Win') ? 'windows' : 'linux';
  document.documentElement.setAttribute('data-platform', platform);

  try {
    store.config = await api.getConfig();
  } catch {
    // Render something usable even if the bridge is unavailable.
    store.config = {
      scan_roots: [], blocked_paths: [], enabled_ecosystems: [...ALL_ECOSYSTEMS],
      staleness_threshold_days: 30, cpu_threshold_percent: 70,
      walker_threads: 4, max_concurrent_analyses: 8,
      ui: {
        theme: 'system', density: 'comfortable', grouping: 'ecosystem',
        confirm_before_cleaning: true, prefer_trash: true,
        launch_at_login: false, show_menu_bar_icon: true,
      },
      ai: {
        worktree_idle_days: 3, agent_data_retention_days: 30, large_session_file_mb: 100,
        stale_project_days: 90, extra_worktree_roots: [], detect_duplicate_models: true,
        min_duplicate_model_mb: 64,
      },
      guard: {
        enabled: true, check_interval_minutes: 15, warn_free_percent: 15,
        critical_free_percent: 5, auto_clean: false, policies: [],
      },
    };
  }
  applyPreferences();

  const firstRoot = store.config.scan_roots?.[0];
  if (firstRoot) setHome(String(firstRoot).replace(/\/$/, ''));

  try { store.configPath = await api.getConfigPath(); } catch { store.configPath = null; }

  render();

  api.onScanEvent(onScanEvent);
  api.onActionEvent(onActionEvent);
  api.onAppWarning((msg) => toast(String(msg)));
  api.onGuardStatus((view) => { store.guard = view; scheduleRender(); });
  api.onGuardCleaned((path) => {
    // Guard's automatic cleanup removed this; drop it so Results match disk.
    const gone = store.items.filter((i) => String(i.path) === String(path));
    if (!gone.length) return;
    store.items = store.items.filter((i) => String(i.path) !== String(path));
    for (const i of gone) store.selected.delete(i.id);
    scheduleRender();
  });
  api.onHistoryChanged(() => { refreshHistory(); refreshDisk(); refreshGuard(); });
  api.onTrayAction((what) => {
    if (what === 'idle-worktrees') {
      applyPreset('idle-worktrees');
      if (!store.hasScanned && !store.scanning) startScan();
    }
  });

  refreshDisk();
  refreshHistory();
  refreshGuard();

  try {
    const warnings = await api.takeStartupWarnings();
    if (warnings.length) toast(warnings[0]);
  } catch { /* nothing to report */ }

  // The tray's "Scan Now" reaches in through this hook.
  window.__void_scan = startScan;
}

boot();
