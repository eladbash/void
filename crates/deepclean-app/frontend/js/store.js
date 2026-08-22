/** Central mutable state. Screens read it; events and user input write it. */

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
  sort: 'size',
  collapsed: new Set(),

  focusedId: null,
  drawerId: null,

  config: null,
  disk: null,
  history: null,

  /** Live clean progress. */
  clean: null,

  hasScanned: false,
};

/** Selection totals, computed from the authoritative item list. */
export function selectionSummary() {
  let bytes = 0;
  let hidden = 0;
  const visible = new Set(visibleItems().map((i) => i.id));
  for (const id of store.selected) {
    const item = store.items.find((i) => i.id === id);
    if (!item) continue;
    bytes += item.size_bytes || 0;
    if (!visible.has(id)) hidden += 1;
  }
  return { count: store.selected.size, bytes, hidden };
}

export function totalBytes() {
  return store.items.reduce((sum, i) => sum + (i.size_bytes || 0), 0);
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
    store.largeOnly
  );
}

export function resetFilters() {
  store.ecoFilter.clear();
  store.riskFilter.clear();
  store.staleOnly = false;
  store.largeOnly = false;
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
