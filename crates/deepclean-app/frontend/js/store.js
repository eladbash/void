/** Central mutable state. Screens read it; events and user input write it. */

import { outermost, totalBytesOf } from './selection.js';
import { isActionable } from './actions.js';

export const store = {
  route: 'results',

  items: [],
  /** item.id -> action.id, when the user overrides the default. */
  overrides: new Map(),
  /** Selected item ids. Independent of render state and of filtering. */
  selected: new Set(),

  scanning: false,
  cleaning: false,
  pathsScanned: 0,
  scanDurationMs: 0,
  lastScanAt: null,

  /** ecosystem -> 'pending' | 'running' | { done: n } */
  scannerStatus: new Map(),

  /** Grouped, deduplicated scan problems: message -> count. */
  issues: new Map(),
  deniedPaths: new Set(),

  filter: '',
  ecoFilter: new Set(),
  riskFilter: new Set(),
  staleOnly: false,
  largeOnly: false,
  /** A quick filter from the Agents screen or the tray: { id, label, kinds, minDays }. */
  preset: null,
  sort: 'size',
  collapsed: new Set(),

  focusedId: null,
  drawerId: null,

  config: null,
  disk: null,
  history: null,

  /** Agents screen data. `null` until fetched; errors kept as strings. */
  agentUsage: null,
  guard: null,
  hooks: null,
  hooksError: null,
  mcp: null,
  mcpError: null,

  /** Live clean progress. */
  clean: null,

  hasScanned: false,
};

/** The selected items that still exist, in list order. */
export function selectedItems() {
  return store.items.filter((i) => store.selected.has(i.id));
}

/**
 * Selection totals, computed from the authoritative item list.
 *
 * Bytes count nested selections once: a stale project and the
 * `node_modules` inside it are one amount of space, not two.
 */
export function selectionSummary() {
  const chosen = selectedItems();
  const visible = new Set(visibleItems().map((i) => i.id));
  const hidden = chosen.filter((i) => !visible.has(i.id)).length;
  return {
    count: chosen.length,
    bytes: totalBytesOf(chosen),
    nested: chosen.length - outermost(chosen).length,
    hidden,
  };
}

/**
 * Reclaimable total across every result, nested items counted once.
 *
 * Informational items (no actions: a locked worktree, a container VM disk
 * image) are excluded — nothing Void can run frees their bytes, and a VM disk
 * *contains* the Docker data already listed, so counting it would double the
 * total past the size of the disk.
 */
export function totalBytes() {
  return totalBytesOf(store.items.filter(isActionable));
}

/**
 * Quick filters offered by the Agents screen and the tray. Thresholds come
 * from the AI settings, so "idle" means what the user said it means.
 */
export function presetFor(id, config = store.config) {
  const ai = config?.ai || {};
  switch (id) {
    case 'idle-worktrees':
      return { id, label: 'Idle worktrees', kinds: ['agent_worktree'], minDays: ai.worktree_idle_days ?? 3 };
    case 'old-transcripts':
      return { id, label: 'Old transcripts', kinds: ['agent_transcripts'], minDays: ai.agent_data_retention_days ?? 30 };
    case 'duplicate-models':
      return { id, label: 'Duplicate models', kinds: ['duplicate_model_files'], minDays: null };
    default:
      return null;
  }
}

export function matchesPreset(item, preset) {
  if (!preset) return true;
  if (preset.kinds?.length && !preset.kinds.includes(item.kind)) return false;
  if (preset.minDays != null && !(item.days_stale != null && item.days_stale >= preset.minDays)) return false;
  return true;
}

export function ecosystemsPresent() {
  return [...new Set(store.items.map((i) => i.ecosystem))];
}

/** Items surviving the current filters, in no particular order. */
export function visibleItems() {
  const q = store.filter.trim().toLowerCase();
  const threshold = store.config?.staleness_threshold_days ?? 30;

  return store.items.filter((item) => {
    if (store.ecoFilter.size && !store.ecoFilter.has(item.ecosystem)) return false;
    if (store.riskFilter.size && !store.riskFilter.has(item.risk)) return false;
    if (store.staleOnly && !(item.days_stale != null && item.days_stale > threshold)) return false;
    if (store.largeOnly && (item.size_bytes || 0) < 1e9) return false;
    if (!matchesPreset(item, store.preset)) return false;
    if (q) {
      const hay = `${item.path} ${item.project_name || ''} ${item.kind}`.toLowerCase();
      if (!hay.includes(q)) return false;
    }
    return true;
  });
}

export function filtersActive() {
  return (
    store.ecoFilter.size > 0 ||
    store.riskFilter.size > 0 ||
    store.staleOnly ||
    store.largeOnly ||
    store.preset != null
  );
}

export function resetFilters() {
  store.ecoFilter.clear();
  store.riskFilter.clear();
  store.staleOnly = false;
  store.largeOnly = false;
  store.preset = null;
  store.filter = '';
}

export function recordIssue(message) {
  store.issues.set(message, (store.issues.get(message) || 0) + 1);
}

export function issueCount() {
  let n = 0;
  for (const c of store.issues.values()) n += c;
  return n;
}
