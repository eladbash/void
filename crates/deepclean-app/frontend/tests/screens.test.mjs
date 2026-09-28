/**
 * Render smoke tests: the screens are pure string builders, so they can be
 * exercised without a DOM. Catches a thrown TypeError on a new field shape
 * before a user sees a blank window.
 */
import { test, beforeEach } from 'node:test';
import assert from 'node:assert/strict';

import { store, resetFilters } from '../js/store.js';
import { renderAgents, categorySlot } from '../js/screens/agents.js';
import { renderResults } from '../js/screens/results.js';
import { renderDrawer, buildPlan, renderConfirm } from '../js/screens/panels.js';
import { renderSettings } from '../js/screens/settings.js';
import { item, action, deleteAndTrash } from './fixtures.mjs';

globalThis.navigator ??= { hardwareConcurrency: 8 };

const CONFIG = {
  scan_roots: ['/u'], blocked_paths: [], enabled_ecosystems: ['worktrees', 'node'],
  staleness_threshold_days: 30, cpu_threshold_percent: 70, walker_threads: 4, max_concurrent_analyses: 8,
  ui: { theme: 'system', density: 'comfortable', grouping: 'ecosystem', confirm_before_cleaning: true,
    prefer_trash: true, launch_at_login: false, show_menu_bar_icon: true },
  ai: { worktree_idle_days: 3, agent_data_retention_days: 30, large_session_file_mb: 100, stale_project_days: 90,
    extra_worktree_roots: ['/u/wt'], detect_duplicate_models: true, min_duplicate_model_mb: 64 },
  guard: { enabled: true, check_interval_minutes: 15, warn_free_percent: 15, critical_free_percent: 5, auto_clean: true,
    policies: [{ id: 'trim-idle-worktrees', name: 'Trim idle worktrees', enabled: false }] },
};

let worktree, nm, locked;
beforeEach(() => {
  resetFilters();
  store.selected.clear();
  store.overrides.clear();
  store.config = structuredClone(CONFIG);
  store.hasScanned = true;
  worktree = item({
    path: '/u/repo/.claude/worktrees/fix', ecosystem: 'worktrees', kind: 'agent_worktree', agent: 'claude',
    size_bytes: 9000, days_stale: 6, project_name: 'repo',
    details: [{ label: 'Branch', value: 'worktree-fix' }, { label: 'Uncommitted changes', value: '2 files' }],
    available_actions: [action({ method: { type: 'remove_dirs', paths: ['/u/repo/.claude/worktrees/fix/node_modules'] }, estimated_savings_bytes: 4000 })],
  });
  nm = item({ path: '/u/repo/.claude/worktrees/fix/node_modules', size_bytes: 4000,
    available_actions: deleteAndTrash('/u/repo/.claude/worktrees/fix/node_modules') });
  locked = item({ path: '/u/repo/.claude/worktrees/locked', ecosystem: 'worktrees', kind: 'agent_worktree',
    size_bytes: 100, details: [{ label: 'Locked', value: 'yes' }], available_actions: [] });
  store.items = [nm, worktree, locked];
  store.agentUsage = [{ agent: 'claude', display_name: 'Claude Code', total_bytes: 9000, item_count: 2,
    categories: [{ label: 'Worktrees', bytes: 8000, count: 1 }, { label: 'Transcripts', bytes: 1000, count: 1 }] }];
  store.guard = { enabled: true, auto_clean: false, checked_at: new Date().toISOString(), note: 'CPU busy',
    status: { state: 'low', free_bytes: 8.1e9, total_bytes: 100e9, free_percent: 8.1, message: '8.1 GB free' },
    last_auto_clean: { at: new Date().toISOString(), succeeded: 2, failed: 0, bytes_freed: 3e9 } };
  store.hooks = { installed: false, settings_path: '/u/.claude/settings.json', command: null, cli_path: null };
  store.mcp = null;
  store.mcpError = 'void not found';
});

test('category colours follow the category, not its rank', () => {
  assert.equal(categorySlot('Worktrees'), '1');
  assert.equal(categorySlot('Transcripts'), '2');
  assert.equal(categorySlot('Models'), '3');
  assert.equal(categorySlot('Something new'), 'other');
});

test('agents screen renders usage, guard, integrations and quick filters', () => {
  const html = renderAgents();
  assert.match(html, /Claude Code/);
  assert.match(html, /data-slot="1"/);
  assert.match(html, /8\.1/);
  assert.match(html, /CPU busy/);
  assert.match(html, /claude mcp add void -- void mcp/);
  assert.match(html, /cargo install --path crates\/void-cli/);
  assert.match(html, /data-preset="idle-worktrees"/);
  assert.match(html, /you approve every plan/);
});

test('results put AI ecosystems first and show worktree chips; informational rows are disabled', () => {
  const html = renderResults();
  assert.ok(html.indexOf('Agent worktrees') < html.indexOf('Node.js'), 'AI group leads');
  assert.match(html, /worktree-fix/);
  assert.match(html, /badge-caution"[^>]*>dirty/);
  assert.match(html, /cb is-disabled/);
});

test('drawer lists details, the agent badge, and informational items', () => {
  store.drawerId = worktree.id;
  const html = renderDrawer();
  assert.match(html, /<dt>Branch<\/dt><dd>worktree-fix<\/dd>/);
  assert.match(html, /badge-agent/);
  store.drawerId = locked.id;
  const info = renderDrawer();
  assert.match(info, /Informational — nothing to clean automatically/);
  assert.match(info, /data-act="clean-one"[^>]*disabled/);
});

test('plan runs nested items first and counts their bytes once', () => {
  store.selected.add(worktree.id).add(nm.id);
  const plan = buildPlan();
  assert.deepEqual(plan.entries.map((e) => e.item.id), [nm.id, worktree.id]);
  assert.equal(plan.estimated, 9000);
  assert.match(renderConfirm(plan), /Clean 2 items/);
});

test('settings render the AI and Guard sections', () => {
  store.settingsSection = 'AI';
  const ai = renderSettings();
  assert.match(ai, /data-cfgpath="ai.worktree_idle_days"/);
  assert.match(ai, /data-cfgpath="ai.min_duplicate_model_mb"/);
  assert.match(ai, /\/u\/wt/);
  store.settingsSection = 'Guard';
  const guard = renderSettings();
  assert.match(guard, /Runs only Safe actions/);
  assert.match(guard, /data-act="toggle-policy" data-index="0"/);
  store.settingsSection = 'General';
  assert.match(renderSettings(), /data-act="toggle-login"/);
});
