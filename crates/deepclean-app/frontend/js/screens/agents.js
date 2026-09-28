import { icon, ecoIcon } from '../icons.js';
import { escapeHtml, formatBytes, splitBytes, count, relativeTime, percent } from '../format.js';
import { store, presetFor, matchesPreset } from '../store.js';
import { totalBytesOf } from '../selection.js';

/**
 * The Agents screen: what AI tools cost you, whether the disk is safe, and
 * how to let the agents ask Void themselves.
 */

/**
 * Series slot for a usage category.
 *
 * Colour follows the category, never its rank: "Worktrees" is slot 1 in
 * every agent's bar, whether it is that agent's biggest slice or its
 * smallest. Categories nothing here anticipates share the neutral "other"
 * swatch rather than borrowing a hue that means something else.
 */
const CATEGORY_SLOTS = [
  [/worktree/i, 1],
  [/transcript|session|conversation|history|snapshot/i, 2],
  [/model|weights|blob/i, 3],
  [/browser/i, 4],
  [/cache/i, 5],
  [/log/i, 6],
  [/branch/i, 7],
  [/state|storage|database|workspace/i, 8],
];

export function categorySlot(label) {
  const hit = CATEGORY_SLOTS.find(([re]) => re.test(String(label || '')));
  return hit ? String(hit[1]) : 'other';
}

function stateBadge(state) {
  if (state === 'critical') return `<span class="badge badge-danger">${icon('alert-circle')} critical</span>`;
  if (state === 'low') return `<span class="badge badge-caution">${icon('warning')} low</span>`;
  if (state === 'ok') return `<span class="badge badge-safe">${icon('check')} ok</span>`;
  return '<span class="badge badge-neutral">unknown</span>';
}

function guardCard() {
  const g = store.guard;
  const status = g?.status;
  const free = status ? splitBytes(status.free_bytes) : null;
  const usedPct = status?.total_bytes
    ? Math.min(100, ((status.total_bytes - status.free_bytes) / status.total_bytes) * 100) : 0;
  const warn = store.config?.guard?.warn_free_percent;
  const interval = store.config?.guard?.check_interval_minutes;

  const last = g?.last_auto_clean;
  return `<section class="card acard" aria-labelledby="guard-h">
    <div class="acard-head">
      <span class="t-overline dim" id="guard-h">Guard</span>
      <span class="spacer"></span>
      ${g && !g.enabled ? '<span class="badge badge-neutral">off</span>' : status ? stateBadge(status.state) : ''}
    </div>
    ${status ? `
      <div class="metric"><span class="t-metric-sm tnum">${free.n}</span><span class="t-body-sm dim">${free.u} free</span>
        <span class="t-caption dim" style="margin-left:auto">${percent(status.free_bytes, status.total_bytes)} of ${formatBytes(status.total_bytes)}</span></div>
      <div class="diskmeter guardmeter" role="img" aria-label="${escapeHtml(`${Math.round(usedPct)}% of the disk used`)}">
        <i class="seg-used" style="width:${usedPct.toFixed(1)}%"></i>
        ${warn ? `<b class="threshold" style="left:${(100 - warn).toFixed(1)}%" title="Warns below ${warn}% free"></b>` : ''}
      </div>
      ${status.message ? `<div class="t-body-sm" style="color:var(--text-secondary);margin-top:8px">${escapeHtml(status.message)}</div>` : ''}`
    : `<div class="t-body-sm dim" style="margin:6px 0 4px">${g ? 'Not checked yet.' : 'Guard status unavailable.'}</div>`}
    <div class="t-caption dim" style="margin-top:8px">
      ${g?.checked_at ? `Checked ${escapeHtml(relativeTime(g.checked_at))}` : 'Never checked'}${
        g?.enabled && interval ? ` · every ${count(interval)} min` : ''}${
        g?.enabled ? (g.auto_clean ? ' · automatic cleanup on' : ' · notifications only') : ''}
    </div>
    ${last ? `<div class="t-caption" style="margin-top:4px;color:var(--safe-text)">Last automatic cleanup freed ${escapeHtml(formatBytes(last.bytes_freed))} · ${escapeHtml(relativeTime(last.at))}${last.failed ? ` · ${count(last.failed)} failed` : ''}</div>` : ''}
    ${g?.note ? `<div class="t-caption" style="margin-top:4px;color:var(--caution-text)">${escapeHtml(g.note)}</div>` : ''}
    <div class="acard-actions">
      <button class="btn btn-secondary btn-sm" data-act="guard-check" ${store.guardChecking ? 'disabled' : ''}>
        ${store.guardChecking ? '<span class="spinring"></span>Checking…' : `${icon('refresh', 'icon icon-sm')}Check now`}</button>
      <button class="btn btn-ghost btn-sm" data-act="goto-settings-section" data-section="Guard">Guard settings ${icon('chevron-right', 'icon icon-sm')}</button>
    </div>
  </section>`;
}

const QUICK = [
  { id: 'idle-worktrees', eco: 'worktrees', hint: (p) => `untouched ${p.minDays}+ days` },
  { id: 'old-transcripts', eco: 'agent_data', hint: (p) => `older than ${p.minDays} days` },
  { id: 'duplicate-models', eco: 'models', hint: () => 'byte-identical copies' },
];

function quickFilters() {
  return `<section class="card acard" aria-labelledby="quick-h">
    <div class="acard-head"><span class="t-overline dim" id="quick-h">Jump to</span></div>
    <div class="quicklist">
      ${QUICK.map((q) => {
        const preset = presetFor(q.id);
        const matches = store.items.filter((i) => matchesPreset(i, preset));
        return `<button class="quickrow" data-act="preset" data-preset="${q.id}">
          <span style="color:var(--eco-${q.eco});display:flex">${ecoIcon(q.eco)}</span>
          <span class="nm">
            <span class="t-body" style="color:var(--text-primary)">${escapeHtml(preset.label)}</span>
            <span class="t-caption dim">${escapeHtml(q.hint(preset))}</span>
          </span>
          <span class="t-caption tnum dim">${store.hasScanned
            ? `${count(matches.length)} · ${formatBytes(totalBytesOf(matches))}` : '—'}</span>
          ${icon('chevron-right', 'icon icon-sm')}
        </button>`;
      }).join('')}
    </div>
  </section>`;
}

function usageSection() {
  const usage = store.agentUsage || [];
  let body;
  if (!store.hasScanned) {
    body = `<div class="state" data-size="sm">
      ${icon('sparkle', 'glyph icon icon-lg')}
      <div class="title">Scan to see what each agent left behind</div>
      <div class="body">Worktrees, transcripts, browsers and models, attributed to the tool that created them.</div>
      <div class="actions"><button class="btn btn-primary btn-sm" data-act="scan">${icon('scan', 'icon icon-sm')}Scan now</button></div>
    </div>`;
  } else if (!usage.length) {
    body = `<div class="state" data-size="sm">
      ${icon('shield-check', 'glyph icon icon-lg')}
      <div class="title">No AI tool data found</div>
      <div class="body">The last scan found nothing Void could attribute to an AI agent or model runner.</div>
    </div>`;
  } else {
    const max = usage.reduce((m, a) => Math.max(m, a.total_bytes || 0), 1);
    body = `<div class="usage">${usage.map((a) => {
      const cats = (a.categories || []).filter((c) => c.bytes > 0);
      const summary = cats.map((c) => `${c.label} ${formatBytes(c.bytes)}`).join(', ');
      return `<div class="usage-row">
        <div class="usage-head">
          <span class="t-body-strong" style="color:var(--text-primary)">${escapeHtml(a.display_name || a.agent)}</span>
          <span class="t-caption dim tnum">${count(a.item_count)} item${a.item_count === 1 ? '' : 's'}</span>
          <span class="spacer"></span>
          <span class="t-body-strong tnum">${formatBytes(a.total_bytes)}</span>
        </div>
        <div class="usage-track">
          <div class="usage-bar" role="img" style="width:${Math.max(1, (a.total_bytes / max) * 100).toFixed(1)}%"
            aria-label="${escapeHtml(`${a.display_name || a.agent}: ${formatBytes(a.total_bytes)} — ${summary}`)}">
            ${cats.map((c) => `<i data-slot="${categorySlot(c.label)}" style="flex-grow:${c.bytes}"
              title="${escapeHtml(`${c.label} · ${formatBytes(c.bytes)} · ${count(c.count)} item${c.count === 1 ? '' : 's'}`)}"></i>`).join('')}
          </div>
        </div>
        <div class="usage-legend t-caption">
          ${cats.map((c) => `<span><i class="swatch" data-slot="${categorySlot(c.label)}"></i>${escapeHtml(c.label)}
            <b class="tnum">${formatBytes(c.bytes)}</b></span>`).join('')}
        </div>
      </div>`;
    }).join('')}</div>`;
  }

  return `<section class="asect" aria-labelledby="usage-h">
    <div class="asect-head"><span class="t-section-title" id="usage-h">Usage by agent</span>
      <span class="t-caption dim">Nested items are counted once</span></div>
    ${body}
  </section>`;
}

function hooksCard() {
  const h = store.hooks;
  const cliMissing = h && !h.cli_path;
  return `<section class="card acard" aria-labelledby="hooks-h">
    <div class="acard-head">
      <span class="t-body-strong" id="hooks-h" style="color:var(--text-primary)">Claude Code hooks</span>
      <span class="spacer"></span>
      ${h ? (h.installed ? `<span class="badge badge-safe">${icon('check')} installed</span>` : '<span class="badge badge-neutral">not installed</span>') : ''}
    </div>
    <p class="t-body-sm" style="color:var(--text-secondary)">Warns your agent at session start when the disk is low, and trims a
      worktree’s build folders when the agent removes it.</p>
    ${h ? `<div class="t-caption dim" style="margin-top:8px">Settings file</div>
      <div class="cmdline" style="margin-top:2px">${escapeHtml(String(h.settings_path))}</div>
      ${h.command ? `<div class="t-caption dim" style="margin-top:6px">Runs</div><div class="cmdline" style="margin-top:2px">${escapeHtml(h.command)}</div>` : ''}` : ''}
    ${store.hooksError ? `<div class="error-plate" style="margin-top:8px;max-width:none">${escapeHtml(store.hooksError)}</div>` : ''}
    ${cliMissing && !store.hooksError ? cliHint() : ''}
    <div class="acard-actions">
      ${h?.installed
        ? '<button class="btn btn-secondary btn-sm" data-act="hooks-uninstall">Uninstall</button>'
        : `<button class="btn btn-primary btn-sm" data-act="hooks-install" ${cliMissing ? 'disabled' : ''}>Install hooks</button>`}
      <span class="t-caption dim">A backup of your settings is written first.</span>
    </div>
  </section>`;
}

function cliHint() {
  return `<div class="safety-line warn" style="margin-top:8px">${icon('warning')}
    <span>Needs the <code class="t-mono-xs">void</code> command-line tool on your PATH. Install it with
    <code class="t-mono-xs">cargo install --path crates/void-cli</code> or from the release download.</span></div>`;
}

function mcpCard() {
  const m = store.mcp;
  const command = m?.command || 'claude mcp add void -- void mcp';
  return `<section class="card acard" aria-labelledby="mcp-h">
    <div class="acard-head">
      <span class="t-body-strong" id="mcp-h" style="color:var(--text-primary)">MCP server</span>
      <span class="spacer"></span>
      ${icon('plug', 'icon icon-sm dim')}
    </div>
    <p class="t-body-sm" style="color:var(--text-secondary)">Let your agents check disk space and propose cleanups — you approve every plan.</p>
    <div class="t-caption dim" style="margin-top:10px">Claude Code</div>
    <div class="copyrow">
      <code class="cmdline">${escapeHtml(command)}</code>
      <button class="iconbtn" data-act="copy-text" data-text="${escapeHtml(command)}" aria-label="Copy command" title="Copy">${icon('copy', 'icon icon-sm')}</button>
    </div>
    ${m ? `<div class="t-caption dim" style="margin-top:10px">Other agents — add to the MCP config</div>
      <div class="copyrow">
        <pre class="cmdline snippet">${escapeHtml(prettyJson(m.json))}</pre>
        <button class="iconbtn" data-act="copy-text" data-text="${escapeHtml(prettyJson(m.json))}" aria-label="Copy JSON" title="Copy">${icon('copy', 'icon icon-sm')}</button>
      </div>` : ''}
    ${store.mcpError ? (m ? '' : cliHint()) : ''}
  </section>`;
}

function prettyJson(raw) {
  try { return JSON.stringify(JSON.parse(raw), null, 2); } catch { return String(raw); }
}

export function renderAgents() {
  const sub = 'Void cleans up after your AI agents — and your agents can call Void themselves.';
  return `
    <div class="cmdbar">
      <div class="cmdbar-titles">
        <div class="t-section-title" style="font-size:15px">Agents</div>
        <div class="t-caption sub">${escapeHtml(sub)}</div>
      </div>
      <div class="cmdbar-actions">
        ${store.scanning
          ? '<button class="btn btn-secondary" disabled><span class="spinring"></span>Scanning…</button>'
          : `<button class="btn btn-secondary" data-act="scan">${icon('scan')}Scan now</button>`}
      </div>
    </div>
    <div class="agents-scroll" style="flex:1;overflow-y:auto">
      <div class="agents">
        <div class="agents-grid">${guardCard()}${quickFilters()}</div>
        ${usageSection()}
        <section class="asect" aria-labelledby="int-h">
          <div class="asect-head"><span class="t-section-title" id="int-h">Integrations</span></div>
          <div class="agents-grid">${hooksCard()}${mcpCard()}</div>
        </section>
      </div>
    </div>`;
}
