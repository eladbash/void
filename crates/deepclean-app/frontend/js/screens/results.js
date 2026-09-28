import { icon, ecoIcon } from '../icons.js';
import {
  sizeCell, staleShort, pathHtml, count, percent, formatBytes,
  escapeHtml, relativeTime, duration, splitBytes,
} from '../format.js';
import {
  store, visibleItems, selectionSummary, totalBytes, filtersActive,
  ecosystemsPresent, issueCount,
} from '../store.js';
import {
  selectedAction, effectiveRisk, kindLabel, projectLabel, ECO_NAMES, riskRank, isRecoverable,
  AI_ECOSYSTEMS, isActionable, worktreeInfo, agentName,
} from '../actions.js';

/* ── Shared bits ─────────────────────────────────────────────────────── */

function checkbox(state, disabled = false) {
  return `<span class="cb${disabled ? ' is-disabled' : ''}" aria-checked="${state}" role="checkbox" tabindex="-1"${disabled ? ' aria-disabled="true"' : ''}>
    ${icon('check', 'icon icon-check')}${icon('minus', 'icon icon-mixed')}</span>`;
}

/**
 * Risk badge. Safe is deliberately silent: roughly 80% of real items are Safe,
 * and a badge on 80% of rows is texture rather than signal. Absence is the
 * encoding, and the risk edge still carries it.
 */
function riskBadge(risk) {
  if (risk === 'caution') return `<span class="badge badge-caution">${icon('warning')} caution</span>`;
  if (risk === 'danger') return `<span class="badge badge-danger">${icon('alert-circle')} danger</span>`;
  return '';
}

function groupsFor(items) {
  const mode = store.config?.ui?.grouping ?? 'ecosystem';
  const buckets = new Map();

  for (const item of items) {
    let key, label;
    if (mode === 'risk') {
      key = effectiveRisk(item, store.overrides, store.config?.ui?.prefer_trash);
      label = key.charAt(0).toUpperCase() + key.slice(1);
    } else if (mode === 'project') {
      key = item.project_root || '__global__';
      label = item.project_root ? String(item.project_root).split('/').pop() : 'Global caches';
    } else {
      key = item.ecosystem;
      label = ECO_NAMES[key] || key;
    }
    if (!buckets.has(key)) buckets.set(key, { key, label, items: [], bytes: 0, eco: item.ecosystem });
    const b = buckets.get(key);
    b.items.push(item);
    b.bytes += item.size_bytes || 0;
  }

  const list = [...buckets.values()];
  if (mode === 'risk') {
    // Fixed severity order; Safe first because it is the bulk-select target.
    list.sort((a, b) => riskRank(a.key) - riskRank(b.key));
  } else if (mode === 'ecosystem') {
    // AI ecosystems lead: they are what this release is about, and they are
    // the ones most likely to hold the surprising gigabytes. Size orders the
    // rest, and each half.
    const ai = (g) => (AI_ECOSYSTEMS.includes(g.key) ? 0 : 1);
    list.sort((a, b) => ai(a) - ai(b) || b.bytes - a.bytes);
  } else {
    list.sort((a, b) => b.bytes - a.bytes);
  }

  const maxBytes = list.reduce((m, g) => Math.max(m, g.bytes), 1);
  for (const g of list) {
    g.share = g.bytes / maxBytes;
    sortItems(g.items);
  }
  return list;
}

function sortItems(items) {
  const s = store.sort;
  items.sort((a, b) => {
    if (s === 'stale') return (b.days_stale ?? -1) - (a.days_stale ?? -1);
    if (s === 'name') return kindLabel(a).localeCompare(kindLabel(b));
    if (s === 'risk') {
      return riskRank(effectiveRisk(b, store.overrides)) - riskRank(effectiveRisk(a, store.overrides));
    }
    return (b.size_bytes || 0) - (a.size_bytes || 0);
  });
}

/* ── Rows ────────────────────────────────────────────────────────────── */

function itemRow(item, groupMax) {
  const risk = effectiveRisk(item, store.overrides, store.config?.ui?.prefer_trash);
  const action = selectedAction(item, store.overrides, store.config?.ui?.prefer_trash);
  const actionCount = (item.available_actions || []).length;
  const selected = store.selected.has(item.id);
  const threshold = store.config?.staleness_threshold_days ?? 30;
  const isStale = item.days_stale != null && item.days_stale > threshold;

  // Square-root scaling: with one 40 GB item beside thirty 200 MB ones, linear
  // scaling renders thirty invisible slivers.
  const ratio = item.size_bytes && groupMax ? Math.sqrt(item.size_bytes / groupMax) : 0.02;

  const cleanState = store.clean?.states?.get(item.id);
  const stateClass = cleanState ? ` is-${cleanState.state}` : '';

  let leadCell;
  if (cleanState?.state === 'active') leadCell = '<span class="spinring"></span>';
  else if (cleanState?.state === 'queued') leadCell = '<span class="dim">◦</span>';
  else if (cleanState?.state === 'failed') leadCell = `<span style="color:var(--danger-text);display:flex">${icon('alert-circle', 'icon icon-sm')}</span>`;
  else if (cleanState?.state === 'done') leadCell = `<span style="color:var(--safe-text);display:flex">${icon('check', 'icon icon-sm')}</span>`;
  else leadCell = checkbox(selected ? 'true' : 'false', !isActionable(item));

  const line2 = cleanState?.state === 'failed'
    ? `<span class="t-mono-xs" style="color:var(--danger-text)">${escapeHtml(cleanState.error || 'Failed')}</span>
       ${cleanState.canFallback ? `<button class="chip chip-quiet" data-act="fallback" data-id="${item.id}">Use “Remove directory” instead ${icon('arrow-right', 'icon icon-sm')}</button>` : ''}`
    : `${pathHtml(item.path, item.project_name)}
       ${actionCount > 1
        ? `<button class="chip chip-quiet" data-act="actions" data-id="${item.id}">${actionCount} actions ${icon('chevron-down', 'icon icon-sm')}</button>`
        : action ? `<span class="t-caption via" title="${escapeHtml(action.label)}">via ${escapeHtml(action.label)}</span>`
        : '<span class="t-caption via">informational</span>'}`;

  const wt = item.ecosystem === 'worktrees' ? worktreeInfo(item) : null;
  const wtChips = wt ? `${wt.branch ? `<span class="wt-branch" title="Branch">${icon('git-branch', 'icon icon-sm')}${escapeHtml(wt.branch)}</span>` : ''}${
    wt.chips.map((c) => `<span class="badge badge-${c.tone}" title="${escapeHtml(c.title)}">${escapeHtml(c.label)}</span>`).join('')}` : '';

  return `<div class="item${selected ? ' is-selected' : ''}${store.focusedId === item.id ? ' is-focused' : ''}${stateClass}"
      data-id="${item.id}" role="row" tabindex="-1"
      style="--eco-accent:var(--eco-${item.ecosystem});--size-ratio:${ratio.toFixed(3)}">
    <span class="cell-check" data-act="toggle" data-id="${item.id}">${leadCell}</span>
    <span class="riskedge ${risk}"></span>
    <span class="cell-main">
      <span class="line1">
        <span class="kindname">${escapeHtml(kindLabel(item))}</span>
        ${projectLabel(item) ? `<span class="projname">· ${escapeHtml(projectLabel(item))}</span>` : ''}
        ${wtChips}
        ${item.agent && item.ecosystem !== 'worktrees' ? `<span class="badge badge-agent">${escapeHtml(agentName(item.agent))}</span>` : ''}
        ${riskBadge(risk)}
      </span>
      <span class="line2">${line2}</span>
    </span>
    <span class="cell-stale col-stale t-caption${isStale ? ' is-stale' : ''}">${staleShort(item.days_stale)}</span>
    <span class="col-mag"><span class="magbar"><i style="width:${Math.max(3, ratio * 100).toFixed(1)}%"></i></span></span>
    <span class="col-size">${sizeCell(item.size_bytes)}</span>
    <span class="col-chev cell-chev" data-act="open" data-id="${item.id}">${icon('chevron-right', 'icon icon-sm')}</span>
  </div>`;
}

function groupHeader(g) {
  const collapsed = store.collapsed.has(g.key);
  const selectable = g.items.filter(isActionable);
  const all = selectable.length > 0 && selectable.every((i) => store.selected.has(i.id));
  const some = selectable.some((i) => store.selected.has(i.id));
  const state = all ? 'true' : some ? 'mixed' : 'false';

  return `<div class="group" data-group="${escapeHtml(String(g.key))}">
    <span data-act="toggle-group" data-group="${escapeHtml(String(g.key))}">${checkbox(state, !selectable.length)}</span>
    <button class="group-toggle" data-act="collapse" data-group="${escapeHtml(String(g.key))}">
      <span class="dim" style="display:flex">${icon(collapsed ? 'chevron-right' : 'chevron-down', 'icon icon-sm')}</span>
      <span style="color:var(--eco-${g.eco});display:flex">${ecoIcon(g.eco)}</span>
      <span class="t-body-strong gname">${escapeHtml(g.label)}</span>
      <span class="countpill tnum">${count(g.items.length)} item${g.items.length === 1 ? '' : 's'}</span>
      <span class="spacer"></span>
    </button>
    <span class="sharebar" style="--eco-accent:var(--eco-${g.eco})"><i style="width:${(g.share * 100).toFixed(1)}%"></i></span>
    <span class="gsize">${sizeCell(g.bytes)}</span>
  </div>`;
}

/* ── Chrome ──────────────────────────────────────────────────────────── */

function summaryStrip() {
  if (store.clean) return cleanProgressStrip();
  if (store.clean === null && store.lastResult) return resultCard();

  const total = totalBytes();
  const { n, u } = splitBytes(total);
  const sel = selectionSummary();
  const disk = store.disk;
  const safeCount = store.items.filter(
    (i) => isActionable(i) && effectiveRisk(i, store.overrides, store.config?.ui?.prefer_trash) === 'safe'
  ).length;

  const reclaimPct = disk?.total_bytes ? (total / disk.total_bytes) * 100 : 0;
  const usedPct = disk?.total_bytes ? (disk.used_bytes / disk.total_bytes) * 100 : 0;

  return `<div class="summary">
    <div class="summary-headline">
      <div class="metric">
        <span class="t-metric-sm tnum">${n}</span>
        <span class="t-body-sm dim">${u} reclaimable</span>
      </div>
      <div class="t-caption sub">${count(store.items.length)} items · ${ecosystemsPresent().length} ecosystems</div>
    </div>
    <div class="summary-disk">
      ${disk ? `<div class="diskmeter">
          <i class="seg-used" style="width:${Math.max(0, usedPct - reclaimPct).toFixed(1)}%"></i>
          <i class="seg-recl" style="width:${reclaimPct.toFixed(1)}%"></i>
        </div>
        <div class="t-caption legend">
          <span><b>${percent(total, disk.total_bytes)}</b> of your ${formatBytes(disk.total_bytes)} disk</span>
          <span class="dim">${formatBytes(disk.available_bytes)} free</span>
        </div>`
      : '<div class="t-caption dim">Disk capacity unavailable</div>'}
    </div>
    <div class="summary-action">
      ${sel.count
        ? `<div class="t-caption selinfo"><b>${count(sel.count)} selected</b> · ${formatBytes(sel.bytes)}${
            sel.nested ? ` <span class="dim" title="Items inside another selected item are counted once">(${count(sel.nested)} nested)</span>` : ''}${sel.hidden ? ` <button class="chip chip-quiet" data-act="clear-filters">${sel.hidden} hidden</button>` : ''}</div>
           <button class="btn btn-primary" data-act="review">Review &amp; Clean<span class="kbd">⌘↵</span></button>`
        : `<button class="btn btn-ghost btn-sm" data-act="select-safe">Select all safe (${count(safeCount)}) ${icon('arrow-right', 'icon icon-sm')}</button>`}
    </div>
  </div>`;
}

function cleanProgressStrip() {
  const c = store.clean;
  const done = c.completed + c.failed;
  const pct = c.total ? Math.round((done / c.total) * 100) : 0;
  return `<div class="summary is-progress">
    <div class="prog">
      <div class="prog-row">
        <span class="t-body-strong">Cleaning ${count(done + (c.current ? 1 : 0))} of ${count(c.total)}</span>
        <span class="spacer"></span>
        <span class="t-caption tnum dim">${pct}%</span>
      </div>
      <div class="bigbar"><i style="width:${pct}%"></i></div>
      <div class="prog-row">
        <span class="t-caption dim">${escapeHtml(c.current || 'Starting…')}</span>
        <span class="spacer"></span>
        <span class="t-caption tnum" style="color:var(--text-secondary)"><b>${formatBytes(c.bytes)}</b> freed so far</span>
      </div>
    </div>
    <button class="btn btn-secondary" data-act="cancel-clean">${icon('stop')}Stop</button>
  </div>`;
}

function resultCard() {
  const r = store.lastResult;
  const { n, u } = splitBytes(r.bytes_freed);
  return `<div class="resultcard${r.failed ? ' has-failures' : ''}">
    <span class="glyph">${icon(r.failed ? 'warning' : 'shield-check', 'icon icon-xl')}</span>
    <div class="spacer" style="flex:1">
      <div style="display:flex;align-items:baseline;gap:8px">
        <span class="t-metric-sm">Reclaimed ${n} ${u}</span>
        ${r.estimated ? `<span class="badge badge-neutral" title="Void cannot measure what cargo clean and similar commands remove. This figure uses each action's estimate.">estimated</span>` : ''}
      </div>
      <div class="t-body-sm tnum" style="color:var(--text-secondary);margin-top:3px">
        ${count(r.succeeded)} of ${count(r.total)} cleaned${r.failed ? ` · <span class="link">${count(r.failed)} failed</span>` : ''} · ${duration(r.duration_ms)}${r.cancelled ? ' · stopped early' : ''}
      </div>
    </div>
    <button class="btn btn-secondary" data-act="goto-history">View report</button>
    <button class="btn btn-ghost" data-act="scan">Scan again</button>
    <button class="iconbtn" data-act="dismiss-result" aria-label="Dismiss">${icon('close')}</button>
  </div>`;
}

function ticker() {
  if (!store.scanning) return '';
  const entries = [...store.scannerStatus.entries()];
  if (!entries.length) return '';
  return `<div class="ticker">${entries.map(([eco, status]) => {
    if (status === 'running') {
      return `<span class="ticker-item is-running"><span class="spinring"></span>${ECO_NAMES[eco] || eco}…</span>`;
    }
    if (typeof status === 'object') {
      return `<span class="ticker-item is-done">${icon('check', 'icon icon-sm')}${ECO_NAMES[eco] || eco} <span class="tnum">${status.done}</span></span>`;
    }
    return `<span class="ticker-item">○ ${ECO_NAMES[eco] || eco}</span>`;
  }).join('')}</div>`;
}

function toolbar() {
  const n = [store.ecoFilter.size, store.riskFilter.size, store.staleOnly ? 1 : 0, store.largeOnly ? 1 : 0, store.preset ? 1 : 0]
    .reduce((a, b) => a + b, 0);
  const groupLabels = { ecosystem: 'Ecosystem', project: 'Project', risk: 'Risk' };
  const sortLabels = { size: 'Size', stale: 'Last modified', name: 'Name', risk: 'Risk' };

  return `<div class="toolbar">
    <div class="search">${icon('search', 'icon icon-sm')}
      <input id="filterInput" placeholder="Filter by path, project, or type" value="${escapeHtml(store.filter)}" spellcheck="false">
      ${store.filter ? `<button class="iconbtn" style="width:18px;height:18px" data-act="clear-search" aria-label="Clear">${icon('close', 'icon icon-sm')}</button>` : ''}
    </div>
    <button class="chip${n ? ' is-active' : ''}" data-act="filters">${icon('filter')}Filters${n ? `<span class="countpill" style="margin-left:2px">${n}</span>` : ''}</button>
    ${store.preset ? `<button class="chip is-active" data-act="clear-preset" title="Clear this filter">${escapeHtml(store.preset.label)}${
      store.preset.minDays != null ? ` · ${count(store.preset.minDays)}+ days` : ''} ${icon('close', 'icon icon-sm')}</button>` : ''}
    <div class="toolbar-sep"></div>
    <button class="chip" data-act="group">Group: ${groupLabels[store.config?.ui?.grouping ?? 'ecosystem']} ${icon('chevron-down', 'icon icon-sm')}</button>
    <button class="chip" data-act="sort">${sortLabels[store.sort]} ${icon('chevron-down', 'icon icon-sm')}</button>
    <span class="spacer"></span>
    <button class="iconbtn" data-act="density" title="Density (⌘D)">${icon('sidebar-toggle')}</button>
    <button class="iconbtn" data-act="expand-all" title="Expand or collapse all (⌘⇧E)">${icon('chevron-down')}</button>
  </div>`;
}

const TABLE_HEAD = `<div class="tablehead">
  <span style="width:22px"></span><span style="width:12px"></span>
  <span class="t-caption" style="flex:1">Name</span>
  <span class="t-caption col-stale">Stale</span>
  <span class="col-mag"></span>
  <span class="t-caption col-size" style="justify-content:flex-end">Size</span>
  <span class="col-chev"></span>
</div>`;

function emptyState() {
  if (!store.hasScanned) {
    const roots = (store.config?.scan_roots || []).map((r) => String(r)).join(', ') || '~/';
    return `<div style="flex:1;display:flex;align-items:center;justify-content:center;padding:24px">
      <div style="width:440px;text-align:center;display:flex;flex-direction:column;align-items:center">
        <div style="color:var(--accent-bg);margin-bottom:24px">
          <svg viewBox="0 0 20 20" width="64" height="64" aria-hidden="true">
            <circle cx="10" cy="10" r="6.75" fill="none" stroke="currentColor" stroke-width="2.5"
              stroke-linecap="round" pathLength="100" stroke-dasharray="88 12" stroke-dashoffset="-81"/></svg>
        </div>
        <div class="t-page-title">Find what your builds left behind</div>
        <p class="t-body" style="margin-top:8px;color:var(--text-secondary);max-width:380px">
          Void walks your scan roots and reports every build artifact it finds, grouped by
          ecosystem. Nothing is deleted until you pick it and confirm.</p>
        <div class="card" style="width:100%;margin-top:24px;text-align:left;display:flex;align-items:center;gap:12px;padding:14px 16px">
          ${icon('folder', 'icon icon-lg')}
          <div style="flex:1;min-width:0">
            <div class="t-label dim">Scan roots</div>
            <div class="t-mono-sm" style="color:var(--text-primary);margin-top:2px;overflow:hidden;text-overflow:ellipsis;white-space:nowrap">${escapeHtml(roots)}</div>
            <div class="t-caption dim" style="margin-top:2px">${(store.config?.enabled_ecosystems || []).length} ecosystems enabled</div>
          </div>
          <button class="btn btn-ghost btn-sm" data-act="goto-settings">Change ${icon('chevron-right', 'icon icon-sm')}</button>
        </div>
        <button class="btn btn-primary btn-lg" style="width:240px;margin-top:16px" data-act="scan">Start first scan</button>
        <div class="t-caption dim" style="margin-top:12px">Typically takes 30–90 seconds</div>
      </div>
    </div>`;
  }

  if (store.items.length === 0) {
    return `<div class="state">
      ${icon('shield-check', 'glyph icon icon-xl')}
      <div class="title">Nothing to reclaim</div>
      <div class="body">Void scanned ${count(store.pathsScanned)} paths and found no build artifacts.
        On macOS this is more often a permissions problem than a clean machine.</div>
      <div class="actions">
        <button class="btn btn-secondary" data-act="goto-settings">Check scan roots</button>
        <button class="btn btn-ghost" data-act="scan">Scan again</button>
      </div>
      <div class="hint">⌘R to rescan</div>
    </div>`;
  }

  return `<div class="state" data-size="sm">
    ${icon('filter', 'glyph icon icon-lg')}
    <div class="title">No items match</div>
    <div class="body">The scan found ${count(store.items.length)} items, none matching the current filters.</div>
    <div class="actions"><button class="btn btn-ghost btn-sm" data-act="clear-filters">Clear filters</button></div>
  </div>`;
}

/* ── Screen ──────────────────────────────────────────────────────────── */

export function renderResults() {
  const items = visibleItems();
  const groups = groupsFor(items);
  const showEmpty = !store.hasScanned || items.length === 0;

  const sub = store.scanning
    ? `Scanning · ${count(store.pathsScanned)} paths · ${count(store.items.length)} found`
    : store.hasScanned
      ? `${count(store.items.length)} items · scanned ${store.lastScanAt ? relativeTime(store.lastScanAt) : 'just now'}`
      : 'Ready to scan';

  const banner = store.hasScanned && store.deniedPaths.size && !store.scanning
    ? `<div class="banner">${icon('lock')}
        <span class="txt">Some folders couldn't be read, so what they hold is not listed here.</span>
        <button class="btn btn-secondary btn-sm" data-act="show-issues">See which</button>
        <button class="iconbtn" style="width:24px;height:24px" data-act="dismiss-banner" aria-label="Dismiss">${icon('close', 'icon icon-sm')}</button>
      </div>`
    : '';

  const listBody = showEmpty
    ? emptyState()
    : groups.map((g) => {
      const collapsed = store.collapsed.has(g.key);
      const groupMax = g.items.reduce((m, i) => Math.max(m, i.size_bytes || 0), 1);
      return groupHeader(g) + (collapsed ? '' : g.items.map((i) => itemRow(i, groupMax)).join(''));
    }).join('');

  return `
    ${cmdbar(sub)}
    ${store.scanning ? '<div class="progress is-indeterminate"></div>' : ''}
    ${store.hasScanned || store.scanning ? summaryStrip() : ''}
    ${banner}
    ${ticker()}
    ${store.hasScanned && !showEmpty ? toolbar() + TABLE_HEAD : ''}
    <div class="listwrap"><div class="list" id="list" role="grid">${listBody}</div></div>
    ${statusbar()}
  `;
}

function cmdbar(sub) {
  return `<div class="cmdbar">
    <div class="cmdbar-titles">
      <div class="t-section-title" style="font-size:15px">Results</div>
      <div class="t-caption sub">${escapeHtml(sub)}</div>
    </div>
    <div class="cmdbar-actions">
      ${store.scanning
        ? `<button class="btn btn-primary" disabled><span class="spinring"></span>Scanning…</button>`
        : `<button class="btn btn-primary" data-act="scan">${icon('scan')}Scan now</button>`}
      <button class="iconbtn" data-act="overflow" aria-label="More">${icon('more-horizontal')}</button>
    </div>
  </div>`;
}

function statusbar() {
  const issues = issueCount();
  const left = store.scanning
    ? `Walking · ${count(store.pathsScanned)} paths`
    : store.hasScanned
      ? `${count(store.pathsScanned)} paths · ${duration(store.scanDurationMs)}`
      : 'Idle';
  return `<div class="statusbar t-caption">
    <span>${escapeHtml(left)}</span>
    <span class="spacer"></span>
    ${issues ? `<button class="issues" data-act="show-issues">${icon('warning', 'icon icon-sm')} ${count(issues)} issue${issues === 1 ? '' : 's'}</button>` : ''}
    <span class="dim">${(store.config?.ui?.density ?? 'comfortable') === 'compact' ? 'Compact' : 'Comfortable'}</span>
  </div>`;
}

export { groupsFor, riskBadge, checkbox };
