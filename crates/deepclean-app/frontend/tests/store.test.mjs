import { test, beforeEach } from 'node:test';
import assert from 'node:assert/strict';

import {
  store, selectionSummary, totalBytes, visibleItems, presetFor, matchesPreset,
  filtersActive, resetFilters,
} from '../js/store.js';
import { item as bare, action } from './fixtures.mjs';

/** A result with one delete action — the normal, cleanable case. */
const item = (overrides = {}) => bare({
  available_actions: [action({ method: { type: 'remove_dir', path: overrides.path || '/x' } })],
  ...overrides,
});

beforeEach(() => {
  store.items = [];
  store.selected.clear();
  resetFilters();
  store.config = { staleness_threshold_days: 30, ai: { worktree_idle_days: 3, agent_data_retention_days: 30 } };
});

test('selection totals count a project and its node_modules once', () => {
  const project = item({ path: '/u/proj', kind: 'stale_project', ecosystem: 'projects', size_bytes: 5000 });
  const nm = item({ path: '/u/proj/node_modules', size_bytes: 3000 });
  const lone = item({ path: '/u/other/node_modules', size_bytes: 700 });
  store.items = [project, nm, lone];

  store.selected.add(nm.id);
  assert.equal(selectionSummary().bytes, 3000);

  store.selected.add(project.id);
  store.selected.add(lone.id);
  const s = selectionSummary();
  assert.equal(s.count, 3);
  assert.equal(s.bytes, 5700);
  assert.equal(s.nested, 1);
  assert.equal(totalBytes(), 5700);
});

test('reclaimable total skips informational items such as a Docker VM disk', () => {
  // The bug: a 220 GB Docker.raw (which holds the Docker data) was added on
  // top of the Docker data itself, pushing "reclaimable" past the disk size.
  const docker = item({ path: '/var/run/docker.sock', kind: 'docker_data', ecosystem: 'docker', size_bytes: 220 });
  const vmDisk = item({
    path: '/u/Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw',
    kind: 'container_vm_disk', ecosystem: 'docker', size_bytes: 220, available_actions: [],
  });
  const nm = item({ path: '/u/app/node_modules', size_bytes: 5 });
  store.items = [docker, vmDisk, nm];
  assert.equal(totalBytes(), 225);
});

test('selection ignores ids no longer in the results and counts hidden ones', () => {
  const a = item({ size_bytes: 10, ecosystem: 'rust' });
  const b = item({ size_bytes: 20, ecosystem: 'node' });
  store.items = [a, b];
  store.selected.add(a.id).add(b.id).add('gone');
  store.ecoFilter.add('node');
  const s = selectionSummary();
  assert.equal(s.count, 2);
  assert.equal(s.bytes, 30);
  assert.equal(s.hidden, 1);
});

test('idle-worktree preset uses the configured idle days', () => {
  const preset = presetFor('idle-worktrees');
  assert.equal(preset.minDays, 3);
  const idle = item({ kind: 'agent_worktree', ecosystem: 'worktrees', days_stale: 5 });
  const fresh = item({ kind: 'agent_worktree', ecosystem: 'worktrees', days_stale: 1 });
  const unknown = item({ kind: 'agent_worktree', ecosystem: 'worktrees', days_stale: null });
  const other = item({ kind: 'node_modules', days_stale: 50 });
  assert.equal(matchesPreset(idle, preset), true);
  assert.equal(matchesPreset(fresh, preset), false);
  assert.equal(matchesPreset(unknown, preset), false);
  assert.equal(matchesPreset(other, preset), false);

  store.items = [idle, fresh, other];
  store.preset = preset;
  assert.equal(filtersActive(), true);
  assert.deepEqual(visibleItems().map((i) => i.id), [idle.id]);
  resetFilters();
  assert.equal(store.preset, null);
  assert.equal(visibleItems().length, 3);
});

test('other presets', () => {
  assert.equal(presetFor('old-transcripts').kinds[0], 'agent_transcripts');
  assert.equal(presetFor('duplicate-models').minDays, null);
  assert.equal(presetFor('nope'), null);
});
