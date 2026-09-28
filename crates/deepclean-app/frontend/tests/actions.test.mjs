import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  defaultAction, selectedAction, effectiveRisk, isRecoverable, worthRecovering,
  RECOVERY_WORTHY_KINDS, describeMethod, methodClass, safetyLines, isActionable,
  worktreeInfo, agentName, kindLabel, projectLabel, outcomeText,
} from '../js/actions.js';
import { action, item, deleteAndTrash } from './fixtures.mjs';

test('move_to_trash and the legacy Finder one-liner are recoverable; rm is not', () => {
  assert.equal(isRecoverable(action({ method: { type: 'move_to_trash', path: '/a' } })), true);
  assert.equal(isRecoverable(action({
    method: { type: 'command', program: 'osascript', args: ['-e', 'tell application "Finder" to delete POSIX file "/a"'] },
  })), true);
  assert.equal(isRecoverable(action({ method: { type: 'command', program: 'rm', args: ['-rf', '/a'] } })), false);
  assert.equal(isRecoverable(action({ method: { type: 'remove_dir', path: '/a' } })), false);
  assert.match(outcomeText(action({ method: { type: 'move_to_trash', path: '/a' } })).note, /Recoverable/);
});

test('recovery-worthy kinds mirror trash::worth_recovering', () => {
  assert.deepEqual([...RECOVERY_WORTHY_KINDS].sort(), [
    'agent_file_history', 'agent_transcripts', 'archives', 'downloads_dir',
    'editor_workspace_storage', 'orphan_worktree', 'stale_project',
  ]);
  assert.equal(worthRecovering('node_modules'), false);
});

test('prefer-Trash picks the Trash action for recovery-worthy kinds', () => {
  const it = item({ kind: 'stale_project', available_actions: deleteAndTrash('/p', 'caution') });
  assert.equal(defaultAction(it, true).method.type, 'move_to_trash');
});

test('with prefer-Trash off, a recovery-worthy item falls back to the listed order', () => {
  const it = item({ kind: 'agent_transcripts', available_actions: deleteAndTrash('/t') });
  assert.equal(defaultAction(it, false).method.type, 'remove_dir');
});

test('regenerable caches keep the permanent delete even when Trash is preferred', () => {
  // Trash frees nothing until it is emptied; for node_modules that defeats the point.
  const trashFirst = deleteAndTrash('/nm').reverse();
  const it = item({ kind: 'node_modules', available_actions: trashFirst });
  assert.equal(defaultAction(it, true).method.type, 'remove_dir');
  assert.equal(defaultAction(it, false).method.type, 'remove_dir');
});

test('lower risk always wins over Trash preference', () => {
  const it = item({
    kind: 'downloads_dir',
    available_actions: [
      action({ method: { type: 'move_to_trash', path: '/d' }, risk: 'caution', estimated_savings_bytes: 10 }),
      action({ method: { type: 'remove_file', path: '/d' }, risk: 'safe', estimated_savings_bytes: 10 }),
    ],
  });
  assert.equal(defaultAction(it, true).risk, 'safe');
});

test('largest estimate breaks a risk tie (Remove target/ over cargo clean)', () => {
  const it = item({
    kind: 'target_dir',
    available_actions: [
      action({ method: { type: 'command', program: 'cargo', args: ['clean'] }, estimated_savings_bytes: 0 }),
      action({ method: { type: 'remove_dir', path: '/t' }, estimated_savings_bytes: 3800 }),
    ],
  });
  assert.equal(defaultAction(it).method.type, 'remove_dir');
});

test('effectiveRisk follows the action that will run, and overrides', () => {
  const safe = action({ risk: 'safe' });
  const danger = action({ risk: 'danger', method: { type: 'remove_dir', path: '/sim' } });
  const it = item({ risk: 'caution', available_actions: [danger, safe] });
  const overrides = new Map();
  assert.equal(effectiveRisk(it, overrides), 'safe');
  overrides.set(it.id, danger.id);
  assert.equal(selectedAction(it, overrides).id, danger.id);
  assert.equal(effectiveRisk(it, overrides), 'danger');
  // An item with no actions falls back to its own risk and has no default.
  const info = item({ risk: 'caution' });
  assert.equal(defaultAction(info), null);
  assert.equal(effectiveRisk(info, new Map()), 'caution');
  assert.equal(isActionable(info), false);
  assert.equal(isActionable(it), true);
});

test('describeMethod states what every new method runs', () => {
  const cases = [
    [{ type: 'move_to_trash', path: '/a' }, /Move to Trash/],
    [{ type: 'remove_dirs', paths: ['/a', '/b'] }, /Delete 2 directories/],
    [{ type: 'remove_dirs', paths: ['/a'] }, /Delete 1 directory$/],
    [{ type: 'remove_files', paths: ['/a', '/b', '/c'] }, /Delete 3 files/],
    [{ type: 'git_worktree_remove', repo: '/r', worktree: '/r/.claude/worktrees/x', delete_branch: 'fix' },
      /^git worktree remove \/r\/\.claude\/worktrees\/x && git branch -d fix$/],
    [{ type: 'git_worktree_remove', repo: '/r', worktree: '/w', delete_branch: null }, /^git worktree remove \/w$/],
    [{ type: 'git_worktree_prune', repo: '/r' }, /^git worktree prune$/],
    [{ type: 'git_delete_branches', repo: '/r', branches: ['a', 'b'] }, /^git branch -d a b$/],
    [{ type: 'dedup_files', groups: [{ keep: '/k', duplicates: ['/d1', '/d2'], size_bytes: 5, hash: 'h' }] },
      /Replace 2 duplicate files with copy-on-write clones/],
    [{ type: 'remove_ollama_orphans', store: '/o', blobs: ['/o/b1', '/o/b2'] },
      /Delete 2 unreferenced Ollama blobs — manifests re-checked first/],
  ];
  for (const [method, re] of cases) {
    const d = describeMethod(method);
    assert.match(d.text, re, method.type);
    assert.notEqual(d.text, 'Unknown action');
    assert.ok(safetyLines(method).length > 0, `${method.type} has safety lines`);
  }
  assert.equal(describeMethod({ type: 'git_worktree_prune', repo: '/r' }).workingDir, '/r');
  assert.deepEqual(describeMethod({ type: 'remove_dirs', paths: ['/a'] }).paths, ['/a']);
});

test('methodClass groups new methods for the confirmation dialog', () => {
  assert.equal(methodClass({ type: 'remove_dirs' }), 'directories');
  assert.equal(methodClass({ type: 'remove_files' }), 'files');
  assert.equal(methodClass({ type: 'remove_ollama_orphans' }), 'files');
  assert.equal(methodClass({ type: 'move_to_trash' }), 'trash');
  assert.equal(methodClass({ type: 'dedup_files' }), 'dedup');
  assert.equal(methodClass({ type: 'git_worktree_remove' }), 'commands');
});

test('safety copy never claims commands are inspected', () => {
  const lines = safetyLines({ type: 'command', program: 'npm', args: [] });
  assert.ok(lines.some((l) => !l.ok));
});

test('worktreeInfo reads branch and state chips from details', () => {
  const it = item({
    ecosystem: 'worktrees',
    kind: 'agent_worktree',
    details: [
      { label: 'Branch', value: 'worktree-fix-login' },
      { label: 'Uncommitted changes', value: '3 files' },
      { label: 'Unpushed commits', value: '0' },
      { label: 'Merged', value: 'yes — PR #12 closed' },
      { label: 'Locked', value: 'no' },
    ],
  });
  const info = worktreeInfo(it);
  assert.equal(info.branch, 'worktree-fix-login');
  assert.deepEqual(info.chips.map((c) => c.key), ['dirty', 'merged']);

  const clean = worktreeInfo(item({ details: [{ label: 'Uncommitted changes', value: 'none' }, { label: 'Locked', value: 'yes' }] }));
  assert.deepEqual(clean.chips.map((c) => c.key), ['locked']);
  assert.deepEqual(worktreeInfo(item()).chips, []);
});

test('agent names are human, unknown ids are title-cased', () => {
  assert.equal(agentName('claude'), 'Claude Code');
  assert.equal(agentName('lmstudio'), 'LM Studio');
  assert.equal(agentName('some-new_tool'), 'Some New Tool');
  assert.equal(agentName(null), null);
});

test('kind and project labels', () => {
  assert.equal(kindLabel(item({ kind: 'agent_worktree' })), 'Agent worktree');
  assert.equal(kindLabel(item({ kind: 'downloads_dir', path: '/Users/dev/Downloads/xcode.xip' })), 'xcode.xip');
  assert.equal(kindLabel(item({ kind: 'not_a_kind', path: '/a/b/leaf/' })), 'leaf');
  assert.equal(projectLabel(item({ kind: 'homebrew_cache', project_name: 'Homebrew cache' })), null);
  assert.equal(projectLabel(item({ kind: 'ollama_model', project_name: 'llama3:8b' })), 'llama3:8b');
});
