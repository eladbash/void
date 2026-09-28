import { icon } from '../icons.js';
import { formatBytes, splitBytes, count, duration, escapeHtml } from '../format.js';
import { store } from '../store.js';
import { ECO_NAMES } from '../actions.js';

/** Bytes a run freed, measured where possible and estimated where not. */
function runBytes(run) {
  return run.items.reduce((sum, i) => {
    if (i.outcome !== 'succeeded') return sum;
    return sum + (i.bytes_freed > 0 ? i.bytes_freed : i.estimated_bytes);
  }, 0);
}

function runIsEstimated(run) {
  return run.items.some((i) => i.outcome === 'succeeded' && i.bytes_freed === 0 && i.estimated_bytes > 0);
}

function tally(run) {
  const ok = run.items.filter((i) => i.outcome === 'succeeded').length;
  const failed = run.items.length - ok;
  return { ok, failed };
}

function detail(run) {
  return `<div class="scrim" data-act="close-run">
    <div class="modal" style="width:640px" role="dialog" aria-label="Run detail">
      <div class="modal-head">
        <div style="flex:1">
          <div class="t-page-title">${escapeHtml(new Date(run.started_at).toLocaleString())}</div>
          <div class="t-body-sm dim tnum">${formatBytes(runBytes(run))} freed · ${count(run.items.length)} items · ${duration(run.duration_ms)}${
            run.trigger === 'guard' ? ' · run automatically by Guard' : ''}</div>
        </div>
        <button class="iconbtn" data-act="close-run" aria-label="Close">${icon('close')}</button>
      </div>
      <div class="modal-body">
        ${run.items.map((i) => `<div class="attn-item" style="border-bottom:1px solid var(--border-subtle)">
          <div class="row1">
            ${i.outcome === 'succeeded'
              ? `<span style="color:var(--safe-text);display:flex">${icon('check', 'icon icon-sm')}</span>`
              : `<span style="color:var(--danger-text);display:flex">${icon('alert-circle', 'icon icon-sm')}</span>`}
            <span class="lbl">${escapeHtml(i.action_label)}</span>
            <span class="amt">${i.outcome === 'succeeded'
              ? formatBytes(i.bytes_freed > 0 ? i.bytes_freed : i.estimated_bytes) : '—'}</span>
          </div>
          <div class="t-mono-xs dim">${escapeHtml(String(i.path))}</div>
          <div class="cmdline" style="margin-top:4px">${escapeHtml(i.method_summary)}</div>
          ${i.outcome === 'failed'
            ? `<div class="error-plate" style="margin-top:6px;max-width:none">${escapeHtml(i.error)}</div>` : ''}
        </div>`).join('')}
      </div>
      <div class="modal-foot">
        <span class="spacer"></span>
        <button class="btn btn-secondary" data-act="close-run">Close</button>
      </div>
    </div>
  </div>`;
}

export function renderHistory() {
  const h = store.history;
  if (!h) return '<div class="state"><div class="title">Loading history…</div></div>';

  const runs = h.runs || [];
  const total = splitBytes(h.total_bytes);
  const last30 = splitBytes(h.bytes_last_30_days);

  // The only chart in the product, because reclaimed-over-time is the one
  // thing here that is genuinely a time series.
  const recent = runs.slice(0, 24).reverse();
  const max = recent.reduce((m, r) => Math.max(m, runBytes(r)), 1);

  const body = runs.length ? `
    <div class="stattiles">
      <div class="stattile"><span class="k t-overline">All time</span><span class="t-metric-sm tnum">${total.n} ${total.u}</span></div>
      <div class="stattile"><span class="k t-overline">Last 30 days</span><span class="t-metric-sm tnum">${last30.n} ${last30.u}</span></div>
      <div class="stattile"><span class="k t-overline">Runs</span><span class="t-metric-sm tnum">${count(h.run_count)}</span></div>
    </div>
    <div style="padding:20px var(--pad-window-x) 14px">
      <div class="t-overline dim" style="margin-bottom:10px">Reclaimed per run</div>
      <div class="spark">${recent.map((r) => {
        const pct = Math.max(2, (runBytes(r) / max) * 100);
        return `<i class="${pct > 70 ? 'hi' : ''}" style="height:${pct.toFixed(1)}%" title="${escapeHtml(formatBytes(runBytes(r)))}"></i>`;
      }).join('')}</div>
    </div>
    <div class="tablehead">
      <span class="t-caption" style="width:150px">Date</span>
      <span class="t-caption" style="width:60px;text-align:right">Items</span>
      <span class="t-caption" style="width:90px;text-align:right">Freed</span>
      <span class="t-caption" style="flex:1;padding-left:16px">Result</span>
      <span class="col-chev"></span>
    </div>
    <div class="listwrap"><div class="list">
      ${runs.map((r) => {
        const t = tally(r);
        return `<div class="hrow" data-act="open-run" data-run="${r.id}">
          <span class="d t-body">${escapeHtml(new Date(r.started_at).toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' }))}</span>
          <span class="n t-body-sm">${count(r.items.length)}</span>
          <span class="f t-body">${formatBytes(runBytes(r))}${runIsEstimated(r) ? '*' : ''}</span>
          <span class="r t-body-sm">
            ${r.trigger === 'guard' ? '<span class="badge badge-neutral" title="Run by Guard mode’s automatic cleanup" style="margin-right:6px">auto</span>' : ''}<span class="ok-text">${count(t.ok)} ok</span>${t.failed ? ` · <span class="fail-text">${count(t.failed)} failed</span>` : ''}
          </span>
          <span class="col-chev dim">${icon('chevron-right', 'icon icon-sm')}</span>
        </div>`;
      }).join('')}
    </div></div>`
  : `<div class="listwrap"><div class="list"><div class="state">
      ${icon('history', 'glyph icon icon-xl')}
      <div class="title">No cleans yet</div>
      <div class="body">Space you reclaim will show up here, with the exact command that freed it.</div>
      <div class="actions"><button class="btn btn-secondary" data-act="goto-results">Go to results</button></div>
    </div></div></div>`;

  return `
    <div class="cmdbar">
      <div class="cmdbar-titles">
        <div class="t-section-title" style="font-size:15px">History</div>
        <div class="t-caption sub tnum">${count(h.run_count)} run${h.run_count === 1 ? '' : 's'} · ${formatBytes(h.total_bytes)} reclaimed</div>
      </div>
      <div class="cmdbar-actions">
        ${runs.length ? `<button class="btn btn-ghost btn-sm" data-act="clear-history">Clear history</button>` : ''}
      </div>
    </div>
    ${body}
    <div class="statusbar t-caption">
      <span>${runs.some(runIsEstimated) ? '* includes estimates — commands cannot report what they removed' : 'Kept locally, alongside your settings'}</span>
      <span class="spacer"></span>
    </div>
    ${store.openRunId ? detail(runs.find((r) => r.id === store.openRunId) || { items: [], started_at: new Date().toISOString(), duration_ms: 0 }) : ''}`;
}
