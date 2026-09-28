/**
 * Selection math that has to survive nesting.
 *
 * AI-era items nest: a stale project contains its `node_modules`, an agent
 * worktree contains its `target/`. Selecting both and adding their sizes
 * counts the inner bytes twice — the headline would promise space that does
 * not exist. Pure functions only, so they can be tested under `node --test`.
 */

/** Normalise a path for prefix comparison: forward slashes, no trailing slash. */
function norm(p) {
  const s = String(p ?? '').replace(/\\/g, '/');
  return s.length > 1 ? s.replace(/\/+$/, '') : s;
}

/**
 * Whether `outer` contains `inner` (or is the same path).
 * Component-wise: `/a/foo` does not contain `/a/foobar`.
 */
export function containsPath(outer, inner) {
  const o = norm(outer);
  const i = norm(inner);
  if (!o || !i) return false;
  if (o === i) return true;
  return i.startsWith(o.endsWith('/') ? o : `${o}/`);
}

/**
 * The items not contained in any other item of the list, in input order.
 *
 * An item only swallows another when its own size covers it. Some items are
 * addressed by a repository path without measuring the whole repository — a
 * repo's stale worktree records are a few kilobytes "at" the repo root — and
 * letting those hide a 4 GB `node_modules` inside the same repo would
 * undercount instead of overcount. Identical paths keep the first.
 */
export function outermost(items) {
  const list = [...items];
  // Sorting on a key where the separator sorts below every other character
  // puts each directory immediately before everything inside it (`/a/b`,
  // `/a/b/c`, then `/a/b-c`), so one pass with a stack of open ancestors
  // finds every containment in O(n log n) — this runs on every render while
  // a scan streams in thousands of items.
  const order = list
    .map((item, idx) => ({ item, idx, key: norm(item.path).replace(/\//g, '\u0000') }))
    .sort((a, b) => (a.key < b.key ? -1 : a.key > b.key ? 1 : a.idx - b.idx));

  const nested = new Set();
  const stack = [];
  for (const entry of order) {
    while (stack.length && !containsPath(stack[stack.length - 1].item.path, entry.item.path)) stack.pop();
    const size = entry.item.size_bytes || 0;
    if (stack.some((a) => (a.item.size_bytes || 0) >= size)) nested.add(entry.idx);
    stack.push(entry);
  }
  return list.filter((_, idx) => !nested.has(idx));
}

/** Bytes across `items`, counting nested ones once. */
export function totalBytesOf(items) {
  return outermost(items).reduce((sum, i) => sum + (i.size_bytes || 0), 0);
}

/** Path depth, for running nested cleans innermost-first. */
export function depth(p) {
  return norm(p).split('/').filter(Boolean).length;
}

/**
 * Order a batch so nested items run before the item that contains them.
 *
 * Removing the inner `node_modules` and then trashing the project works;
 * trashing the project first makes the inner action fail on a missing path.
 * Deeper paths run first; equal depths keep their order.
 */
export function innermostFirst(entries, pathOf = (e) => e.path) {
  return entries
    .map((e, i) => ({ e, i, d: depth(pathOf(e)) }))
    .sort((a, b) => b.d - a.d || a.i - b.i)
    .map((x) => x.e);
}
