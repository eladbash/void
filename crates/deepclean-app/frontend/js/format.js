/**
 * Number, size, path and time formatting.
 *
 * One formatter, used everywhere. The old UI had two that disagreed — Rust's
 * `ByteSize` filled `size_display` for rows while JS used base 1000 for totals,
 * so a row could read 1.5 GB while contributing 1.6 GB to the sum. Everything
 * here derives from `size_bytes`; `size_display` is ignored.
 */

const UNITS = ['B', 'kB', 'MB', 'GB', 'TB', 'PB'];
const numberFormat = new Intl.NumberFormat();

/**
 * Split a byte count into number and unit.
 *
 * Base 1000 with `kB`/`MB`/`GB` to match Finder's Get Info, which is the figure
 * users compare against. Lowercase `k` is the correct SI prefix.
 */
export function splitBytes(bytes) {
  if (bytes == null || !Number.isFinite(bytes) || bytes < 0) return { n: '—', u: '' };
  if (bytes === 0) return { n: '0', u: 'B' };

  let i = Math.min(Math.floor(Math.log10(bytes) / 3), UNITS.length - 1);
  let v = bytes / Math.pow(1000, i);
  // 999.97 kB should read as 1.0 MB, not 1000.0 kB.
  if (v >= 999.95 && i < UNITS.length - 1) { i += 1; v = bytes / Math.pow(1000, i); }

  // Keep the trailing .0 below 10: a consistent decimal position down a column
  // matters more than brevity.
  const decimals = i === 0 ? 0 : v < 100 ? 1 : 0;
  return { n: v.toFixed(decimals), u: UNITS[i] };
}

/** Single-string form, for tooltips and clipboard. */
export function formatBytes(bytes) {
  const { n, u } = splitBytes(bytes);
  return u ? `${n} ${u}` : n;
}

/** Magnitude tier driving the size cell's typographic weight. */
export function magnitude(bytes) {
  if (bytes == null || bytes === 0) return 'unknown';
  if (bytes >= 1e10) return 'xl';
  if (bytes >= 1e9) return 'lg';
  if (bytes >= 1e8) return 'md';
  return 'sm';
}

/** Size cell markup: two elements, never one string — a space is not tabular. */
export function sizeCell(bytes) {
  const mag = magnitude(bytes);
  if (mag === 'unknown') {
    return `<span class="size" data-mag="unknown"><span class="size-num">unknown</span></span>`;
  }
  const { n, u } = splitBytes(bytes);
  return `<span class="size" data-mag="${mag}"><span class="size-num tnum">${n}</span><span class="size-unit">${u}</span></span>`;
}

/**
 * Compact staleness for the 56px column: a sortable magnitude, not prose.
 * Null renders as an em dash — an empty cell reads as a rendering bug.
 */
export function staleShort(days) {
  if (days == null) return '—';
  if (days === 0) return 'today';
  if (days < 365) return `${days}d`;
  return `${(days / 365).toFixed(1)}y`;
}

/** Verbose form for tooltips, where the user is deciding whether to delete. */
export function staleLong(days, lastModified) {
  if (days == null) return 'Modification time unavailable';
  const rtf = new Intl.RelativeTimeFormat(undefined, { numeric: 'auto', style: 'long' });
  let value, unit;
  if (days < 30) { value = -days; unit = 'day'; }
  else if (days < 365) { value = -Math.round(days / 30); unit = 'month'; }
  else { value = -Math.round((days / 365) * 10) / 10; unit = 'year'; }

  const relative = rtf.format(value, unit);
  if (!lastModified) return relative;
  const absolute = new Intl.DateTimeFormat(undefined, { dateStyle: 'medium' }).format(new Date(lastModified));
  return `${relative} · ${absolute}`;
}

/** Exact counts always. Abbreviating in a tool that audits numbers is a lie. */
export function count(n) {
  return numberFormat.format(n);
}

export function percent(part, total) {
  if (!total) return '0%';
  const p = (part / total) * 100;
  // A 40 MB ecosystem in a 30 GB scan rounds to 0%, which reads as "empty".
  if (p > 0 && p < 1) return '<1%';
  if (p > 99 && p < 100) return '>99%';
  return `${Math.round(p)}%`;
}

export function duration(ms) {
  if (ms < 1000) return `${Math.round(ms)}ms`;
  if (ms < 60000) return `${(ms / 1000).toFixed(1)}s`;
  const m = Math.floor(ms / 60000);
  const s = Math.round((ms % 60000) / 1000);
  return `${m}m ${String(s).padStart(2, '0')}s`;
}

export function relativeTime(iso) {
  const then = new Date(iso).getTime();
  const secs = Math.round((Date.now() - then) / 1000);
  if (secs < 60) return 'just now';
  if (secs < 3600) return `${Math.floor(secs / 60)}m ago`;
  if (secs < 86400) return `${Math.floor(secs / 3600)}h ago`;
  return new Intl.DateTimeFormat(undefined, { dateStyle: 'medium', timeStyle: 'short' }).format(then);
}

let HOME = '';
export function setHome(path) { HOME = (path || '').replace(/\/$/, ''); }

export function tildify(p) {
  const n = String(p).replace(/\\/g, '/');
  if (HOME && n.startsWith(HOME)) return `~${n.slice(HOME.length)}`;
  return n.replace(/^\/(?:Users|home)\/[^/]+/, '~');
}

export function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, (c) => (
    { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]
  ));
}

/**
 * Render a path with four emphasis roles, so the meaningful segments read as
 * names and the separators recede.
 *
 * The path is the item's identity — three `node_modules` rows are otherwise
 * indistinguishable — so it lives permanently on the row, not behind a hover.
 */
export function pathHtml(fullPath, projectName) {
  const p = tildify(fullPath);
  const absolute = p.startsWith('/');
  const segments = p.split('/').filter(Boolean);
  if (!segments.length) return `<span class="path">${escapeHtml(p)}</span>`;

  const leaf = segments.length - 1;
  let project = projectName ? segments.lastIndexOf(projectName) : -1;
  if (project === leaf) project = -1;

  let out = absolute && segments[0] !== '~' ? '<span class="s-sep">/</span>' : '';
  segments.forEach((seg, i) => {
    const role = i === leaf ? 'leaf' : i === project ? 'project' : i === 0 ? 'root' : 'dim';
    out += `<span class="s-${role}">${escapeHtml(seg)}</span>`;
    if (i !== leaf) out += '<span class="s-sep">/</span>';
  });
  return `<span class="path" title="${escapeHtml(fullPath)}">${out}</span>`;
}
