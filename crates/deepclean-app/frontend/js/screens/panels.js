import { icon, ecoIcon } from '../icons.js';
import { sizeCell, staleLong, formatBytes, escapeHtml, count, pathHtml } from '../format.js';
import { store } from '../store.js';
import {
  selectedAction, describeMethod, outcomeText, safetyLines, kindLabel,
  ECO_NAMES, isRecoverable, methodClass, riskRank, effectiveRisk, projectLabel,
} from '../actions.js';

/* ── Detail drawer ───────────────────────────────────────────────────── */

function actionOption(item, action, chosen) {
  const method = describeMethod(action.method);
  const outcome = outcomeText(action);
  const isDefault = chosen && !store.overrides.has(item.id);

  return `<button class="action-opt" data-act="choose-action" data-id="${item.id}" data-action="${action.id}">
    <span class="radio${chosen ? ' is-on' : ''}"></span>
    <span class="body">
      <span class="row1">
        <span class="lbl">${escapeHtml(action.label)}</span>
        ${action.risk !== 'safe'
          ? `<span class="badge badge-${action.risk}">${icon(action.risk === 'danger' ? 'alert-circle' : 'warning')} ${action.risk}</span>`
          : `<span class="badge badge-safe">${icon('check')} safe</span>`}
        ${isDefault ? '<span class="badge badge-neutral">default</span>' : ''}
      </span>
      <span class="desc">${escapeHtml(action.description)}</span>
      <span class="cmdline">${escapeHtml(method.glyph)} ${escapeHtml(method.text)}${
        method.workingDir ? `<br>in ${escapeHtml(String(method.workingDir))}` : ''}</span>
      <span class="outcome">Frees ${outcome.frees ? formatBytes(outcome.frees) : 'unknown'} ·
        <span class="${outcome.recoverable ? 'recoverable' : ''}">${outcome.note}</span></span>
    </span>
  </button>`;
}

export function renderDrawer() {
  const item = store.items.find((i) => i.id === store.drawerId);
  if (!item) return '';

  const chosen = selectedAction(item, store.overrides, store.config?.ui?.prefer_trash);
  const actions = item.available_actions || [];
  const method = chosen ? chosen.method : null;

  return `<div class="drawer-scrim" data-act="close-drawer"></div>
  <aside class="drawer" role="dialog" aria-label="Item detail">
    <div class="drawer-head">
      <button class="iconbtn" data-act="prev-item" title="Previous item (⌥↑)">${icon('chevron-up')}</button>
      <button class="iconbtn" data-act="next-item" title="Next item (⌥↓)">${icon('chevron-down')}</button>
      <span class="spacer"></span>
      <button class="iconbtn" data-act="close-drawer" aria-label="Close">${icon('close')}</button>
    </div>
    <div class="drawer-body">
      <div class="drawer-sect" style="padding-bottom:12px">
        <div style="display:flex;align-items:center;gap:6px;color:var(--eco-${item.ecosystem})">
          ${ecoIcon(item.ecosystem)}<span class="t-body-sm">${escapeHtml(ECO_NAMES[item.ecosystem] || item.ecosystem)}</span>
        </div>
        <div class="t-page-title" style="margin-top:4px">${escapeHtml(kindLabel(item))}</div>
        ${projectLabel(item) ? `<div class="t-body-sm dim">${escapeHtml(projectLabel(item))}</div>` : ''}
      </div>

      <div class="drawer-sect">
        <div class="pathplate">${escapeHtml(String(item.path))}</div>
        <div style="display:flex;gap:8px;margin-top:8px">
          <button class="btn btn-secondary btn-sm" data-act="copy-path" data-id="${item.id}">${icon('copy', 'icon icon-sm')}Copy</button>
          <button class="btn btn-secondary btn-sm" data-act="reveal" data-id="${item.id}">${icon('external-link', 'icon icon-sm')}Reveal</button>
        </div>
      </div>

      <div class="drawer-sect">
        <dl class="facts t-body-sm">
          <dt>Size</dt><dd>${item.size_bytes ? formatBytes(item.size_bytes) : 'unknown'}</dd>
          <dt>Last modified</dt><dd>${escapeHtml(staleLong(item.days_stale, item.last_modified))}</dd>
          <dt>Kind</dt><dd>${escapeHtml(item.kind)}</dd>
          ${item.project_root ? `<dt>Project</dt><dd class="t-mono-sm">${escapeHtml(String(item.project_root))}</dd>` : ''}
        </dl>
      </div>

      <div class="drawer-sect">
        <div class="t-overline">Choose an action</div>
        ${actions.length > 6 ? `<div class="t-caption dim" style="margin-bottom:8px">${count(actions.length)} actions available</div>` : ''}
        <div style="${actions.length > 6 ? 'max-height:320px;overflow-y:auto' : ''}">
          ${actions.map((a) => actionOption(item, a, chosen && a.id === chosen.id)).join('')
            || '<div class="t-body-sm dim">No actions available for this item.</div>'}
        </div>
      </div>

      ${method ? `<div class="drawer-sect">
        <div class="t-overline">Safety</div>
        ${safetyLines(method).map((l) => `<div class="safety-line ${l.ok ? 'ok' : 'warn'}">
          ${icon(l.ok ? 'check' : 'warning')}<span>${escapeHtml(l.text)}</span></div>`).join('')}
      </div>` : ''}
    </div>
    <div class="drawer-foot">
      <button class="btn btn-secondary" style="flex:1" data-act="exclude-path" data-id="${item.id}">Exclude this path</button>
      <button class="btn btn-primary" style="flex:1" data-act="clean-one" data-id="${item.id}">Clean this item</button>
    </div>
  </aside>`;
}

/* ── Pre-clean confirmation ──────────────────────────────────────────── */

/** Everything the modal needs, derived from the current selection. */
export function buildPlan() {
  const preferTrash = store.config?.ui?.prefer_trash ?? true;
  const entries = [];

  for (const id of store.selected) {
    const item = store.items.find((i) => i.id === id);
    if (!item) continue;
    const action = selectedAction(item, store.overrides, preferTrash);
    if (!action) continue;
    entries.push({ item, action });
  }

  const effects = { directories: 0, files: 0, commands: 0 };
  const bytes = { directories: 0, files: 0, commands: 0 };
  let trashed = 0;
  let trashedBytes = 0;
  let estimated = 0;
  let hasDanger = false;

  for (const { item, action } of entries) {
    if (isRecoverable(action)) {
      trashed += 1;
      trashedBytes += item.size_bytes || 0;
    } else {
      const k = methodClass(action.method);
      effects[k] += 1;
      bytes[k] += item.size_bytes || 0;
    }
    estimated += item.size_bytes || 0;
    if (action.risk === 'danger') hasDanger = true;
  }

  // Safe items are counted, never listed. Listing fifteen safe node_modules
  // deletions trains people to scroll past the dialog.
  const attention = entries
    .filter(({ action }) => action.risk !== 'safe')
    .sort((a, b) => riskRank(b.action.risk) - riskRank(a.action.risk));

  return { entries, effects, bytes, trashed, trashedBytes, estimated, hasDanger, attention };
}

export function renderConfirm(plan) {
  const rows = [];
  if (plan.effects.directories) {
    rows.push({ ico: 'trash', txt: `${count(plan.effects.directories)} director${plan.effects.directories === 1 ? 'y' : 'ies'} deleted permanently`, amt: plan.bytes.directories });
  }
  if (plan.effects.files) {
    rows.push({ ico: 'file', txt: `${count(plan.effects.files)} file${plan.effects.files === 1 ? '' : 's'} deleted permanently`, amt: plan.bytes.files });
  }
  if (plan.effects.commands) {
    rows.push({ ico: 'terminal', txt: `${count(plan.effects.commands)} command${plan.effects.commands === 1 ? '' : 's'} run`, amt: plan.bytes.commands });
  }
  if (plan.trashed) {
    rows.push({ ico: 'refresh', txt: `${count(plan.trashed)} item${plan.trashed === 1 ? '' : 's'} moved to the Trash`, amt: plan.trashedBytes });
  }

  return `<div class="scrim" data-act="close-confirm">
    <div class="modal" role="dialog" aria-modal="true" aria-label="Confirm clean">
      <div class="modal-head">
        <div style="flex:1">
          <div class="t-page-title">Clean ${count(plan.entries.length)} item${plan.entries.length === 1 ? '' : 's'}</div>
          <div class="t-body-sm dim tnum">${formatBytes(plan.estimated)} estimated</div>
        </div>
        <button class="iconbtn" data-act="close-confirm" aria-label="Close">${icon('close')}</button>
      </div>

      <div class="modal-body">
        <div class="t-overline dim" style="margin-bottom:6px">What will happen</div>
        ${rows.map((r) => `<div class="effect-row">
          ${icon(r.ico)}<span class="txt t-body">${escapeHtml(r.txt)}</span>
          <span class="amt t-body-sm tnum">${formatBytes(r.amt)}</span></div>`).join('')}

        ${plan.attention.length ? `
          <div class="t-overline" style="color:var(--caution-text);margin:20px 0 8px;display:flex;align-items:center;gap:6px">
            ${icon('warning', 'icon icon-sm')} Needs your attention
          </div>
          <div class="attn-card">
            ${plan.attention.map(({ item, action }) => {
              const m = describeMethod(action.method);
              return `<div class="attn-item">
                <div class="row1">
                  <span class="badge badge-${action.risk}">${icon(action.risk === 'danger' ? 'alert-circle' : 'warning')} ${action.risk}</span>
                  <span class="lbl">${escapeHtml(action.label)}</span>
                  <span class="amt">${item.size_bytes ? formatBytes(item.size_bytes) : 'unknown'}</span>
                </div>
                <div class="t-mono-xs dim">${escapeHtml(String(item.path))}</div>
                ${action.method.type === 'command' || action.method.type === 'docker_prune'
                  ? `<div class="cmdline" style="margin-top:4px">${escapeHtml(m.glyph)} ${escapeHtml(m.text)}</div>`
                  : `<div class="t-body-sm dim" style="margin-top:3px">${escapeHtml(action.description)}</div>`}
              </div>`;
            }).join('')}
          </div>` : ''}

        <p class="t-body-sm dim" style="margin-top:16px">
          Deletions and commands bypass the Trash and cannot be undone.</p>

        ${plan.hasDanger ? `
          <div style="margin-top:14px">
            <div class="t-label" style="color:var(--text-primary);margin-bottom:6px">
              Type <code class="t-mono-sm" style="color:var(--danger-text)">delete</code> to confirm</div>
            <input class="input" id="confirmInput" style="width:220px;font-family:var(--font-mono)" spellcheck="false" autocomplete="off">
          </div>` : ''}
      </div>

      <div class="modal-foot">
        <span data-act="toggle-trash" style="display:flex;align-items:center;gap:8px">
          <span class="cb" aria-checked="${store.config?.ui?.prefer_trash ? 'true' : 'false'}" role="checkbox">
            ${icon('check', 'icon icon-check')}${icon('minus', 'icon icon-mixed')}</span>
          <span class="t-body-sm" style="color:var(--text-secondary)">Prefer Trash where available</span>
        </span>
        <span class="spacer"></span>
        <button class="btn btn-secondary" data-act="close-confirm">Cancel</button>
        <button class="btn btn-danger" id="confirmBtn" data-act="do-clean" ${plan.hasDanger ? 'disabled' : ''}>
          Clean ${count(plan.entries.length)} item${plan.entries.length === 1 ? '' : 's'}</button>
      </div>
    </div>
  </div>`;
}

/* ── Issues sheet ────────────────────────────────────────────────────── */

export function renderIssues() {
  const entries = [...store.issues.entries()];
  const denied = [...store.deniedPaths];

  return `<div class="scrim" data-act="close-issues">
    <div class="modal" style="width:520px" role="dialog" aria-label="Scan issues">
      <div class="modal-head">
        <div style="flex:1">
          <div class="t-page-title">Scan issues</div>
          <div class="t-body-sm dim">Problems Void hit while walking your disk.</div>
        </div>
        <button class="iconbtn" data-act="close-issues" aria-label="Close">${icon('close')}</button>
      </div>
      <div class="modal-body">
        ${denied.length ? `
          <div class="t-overline dim" style="margin-bottom:8px">Could not be read (${count(denied.length)})</div>
          <p class="t-body-sm" style="color:var(--text-secondary);margin-bottom:10px">
            Whatever these hold is missing from your results. On macOS, granting Full Disk Access
            in System Settings → Privacy &amp; Security usually fixes it.</p>
          <div class="error-plate" style="max-width:none;margin-bottom:16px">${denied.map(escapeHtml).join('\n')}</div>
        ` : ''}
        ${entries.length ? `
          <div class="t-overline dim" style="margin-bottom:8px">Errors</div>
          ${entries.map(([msg, n]) => `<div style="display:flex;gap:8px;align-items:flex-start;margin-bottom:8px">
            <span class="t-mono-xs" style="flex:1;color:var(--text-secondary);word-break:break-all">${escapeHtml(msg)}</span>
            ${n > 1 ? `<span class="countpill tnum">×${count(n)}</span>` : ''}
          </div>`).join('')}
        ` : ''}
        ${!denied.length && !entries.length ? '<div class="t-body-sm dim">No issues recorded.</div>' : ''}
      </div>
      <div class="modal-foot">
        <button class="btn btn-secondary btn-sm" data-act="copy-issues">Copy all</button>
        <span class="spacer"></span>
        <button class="btn btn-secondary" data-act="close-issues">Close</button>
      </div>
    </div>
  </div>`;
}
