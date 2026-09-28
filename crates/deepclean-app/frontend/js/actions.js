/**
 * Action selection and risk resolution.
 *
 * The central correctness fix in this design: `CleanableItem.risk` and
 * `CleanAction.risk` are different fields and they disagree. `SimulatorDevices`
 * is a Caution item holding a Danger action; `~/Downloads` is a Safe item
 * holding `rm -rf`. The old UI displayed the item's risk and then silently ran
 * `available_actions[0]`. Everything here works from the action that will
 * actually run.
 */

const RISK_ORDER = { safe: 0, caution: 1, danger: 2 };

export function riskRank(risk) {
  return RISK_ORDER[risk] ?? 3;
}

/**
 * Whether an action puts things in the Trash rather than destroying them.
 *
 * `move_to_trash` is the executor's own recoverable delete. The two legacy
 * shell one-liners (Finder / Shell.Application) predate it and are matched on
 * the command text.
 */
export function isRecoverable(action) {
  const m = action.method;
  if (m.type === 'move_to_trash') return true;
  if (m.type !== 'command') return false;
  const line = `${m.program} ${(m.args || []).join(' ')}`.toLowerCase();
  return (
    (line.includes('osascript') && line.includes('finder') && line.includes('delete')) ||
    (line.includes('shell.application') && line.includes('namespace(10)'))
  );
}

/**
 * Kinds worth a recoverable delete, mirroring `trash::worth_recovering`.
 *
 * Everything else is a rebuildable cache: moving `node_modules` to the Trash
 * frees nothing until the Trash is emptied, which defeats a disk cleaner.
 */
export const RECOVERY_WORTHY_KINDS = new Set([
  'downloads_dir',
  'orphan_worktree',
  'stale_project',
  'agent_transcripts',
  'agent_file_history',
  'editor_workspace_storage',
  'archives',
]);

export function worthRecovering(kind) {
  return RECOVERY_WORTHY_KINDS.has(kind);
}

/** Items with nothing to run are informational: shown, never selected. */
export function isActionable(item) {
  return (item.available_actions || []).length > 0;
}

/**
 * Pick the action that runs by default.
 *
 * Deterministic and stated, replacing `available_actions[0]`:
 *   1. lowest risk           — keeps Trash over `rm -rf` for Downloads, and
 *                              avoids the Caution simulator wipe
 *   2. Trash or not          — on a risk tie, the recoverable action wins
 *                              for recovery-worthy kinds when the preference
 *                              is on; for rebuildable caches the permanent
 *                              delete wins, because Trash frees nothing until
 *                              it is emptied
 *   3. largest estimate      — for Rust this picks `Remove target/` over
 *                              `cargo clean`, which is also the one that does
 *                              not need cargo on PATH and the only one whose
 *                              freed bytes are measurable
 *   4. first listed
 */
export function defaultAction(item, preferTrash = true) {
  const actions = item.available_actions || [];
  if (!actions.length) return null;

  return actions.reduce((best, candidate) => {
    const a = riskRank(candidate.risk);
    const b = riskRank(best.risk);
    if (a !== b) return a < b ? candidate : best;

    const ta = isRecoverable(candidate);
    const tb = isRecoverable(best);
    if (ta !== tb) {
      const wantTrash = preferTrash && worthRecovering(item.kind);
      if (wantTrash) return ta ? candidate : best;
      if (!worthRecovering(item.kind)) return ta ? best : candidate;
    }

    const ea = candidate.estimated_savings_bytes || 0;
    const eb = best.estimated_savings_bytes || 0;
    if (ea !== eb) return ea > eb ? candidate : best;

    return best;
  }, actions[0]);
}

/** The action currently chosen for an item, honouring any manual override. */
export function selectedAction(item, overrides, preferTrash = true) {
  const overrideId = overrides.get(item.id);
  if (overrideId) {
    const found = (item.available_actions || []).find((a) => a.id === overrideId);
    if (found) return found;
  }
  return defaultAction(item, preferTrash);
}

/** Risk to display for a row: the selected action's, never the item's. */
export function effectiveRisk(item, overrides, preferTrash = true) {
  const action = selectedAction(item, overrides, preferTrash);
  return action ? action.risk : item.risk;
}

/**
 * What the action physically does.
 *
 * For commands this is the literal command line. Void does not interpret what
 * a command removes, so the command string is the only honest disclosure the
 * user gets — which is exactly why it is shown rather than summarised.
 */
export function describeMethod(method) {
  switch (method.type) {
    case 'command': {
      const line = [method.program, ...(method.args || [])].join(' ');
      return { glyph: '▶', text: line, workingDir: method.working_dir || null };
    }
    case 'remove_dir':
      return { glyph: '⌫', text: 'Delete directory recursively', workingDir: null };
    case 'remove_file':
      return { glyph: '⌫', text: 'Delete file', workingDir: null };
    case 'docker_prune':
      return { glyph: '▶', text: `docker ${method.prune_type} prune -f`, workingDir: null };
    case 'move_to_trash':
      return { glyph: '↩', text: 'Move to Trash', workingDir: null };
    case 'remove_dirs': {
      const n = (method.paths || []).length;
      return { glyph: '⌫', text: `Delete ${n} director${n === 1 ? 'y' : 'ies'}`, workingDir: null, paths: method.paths || [] };
    }
    case 'remove_files': {
      const n = (method.paths || []).length;
      return { glyph: '⌫', text: `Delete ${n} file${n === 1 ? '' : 's'}`, workingDir: null, paths: method.paths || [] };
    }
    case 'git_worktree_remove': {
      const base = `git worktree remove ${method.worktree}`;
      return {
        glyph: '▶',
        text: method.delete_branch ? `${base} && git branch -d ${method.delete_branch}` : base,
        workingDir: method.repo || null,
      };
    }
    case 'git_worktree_prune':
      return { glyph: '▶', text: 'git worktree prune', workingDir: method.repo || null };
    case 'remove_ollama_orphans': {
      const n = (method.blobs || []).length;
      return {
        glyph: '⌫',
        text: `Delete ${n} unreferenced Ollama blob${n === 1 ? '' : 's'} — manifests re-checked first`,
        workingDir: method.store || null,
      };
    }
    case 'git_delete_branches':
      return { glyph: '▶', text: `git branch -d ${(method.branches || []).join(' ')}`, workingDir: method.repo || null };
    case 'dedup_files': {
      const n = (method.groups || []).reduce((s, g) => s + (g.duplicates || []).length, 0);
      return { glyph: '⧉', text: `Replace ${n} duplicate file${n === 1 ? '' : 's'} with copy-on-write clones`, workingDir: null };
    }
    default:
      return { glyph: '▶', text: 'Unknown action', workingDir: null };
  }
}

/** Coarse grouping used by the confirmation dialog's "what will happen". */
export function methodClass(method) {
  switch (method.type) {
    case 'remove_dir': case 'remove_dirs': return 'directories';
    case 'remove_file': case 'remove_files': case 'remove_ollama_orphans': return 'files';
    case 'move_to_trash': return 'trash';
    case 'dedup_files': return 'dedup';
    default: return 'commands';
  }
}

/**
 * How much an action frees.
 *
 * `Frees unknown` rather than `0 B`: every npm/yarn/pnpm project-cache action,
 * every Docker action and every per-project DerivedData action genuinely
 * reports zero, and printing "0 B" would claim it frees nothing.
 */
export function outcomeText(action) {
  const bytes = action.estimated_savings_bytes || 0;
  const recoverable = isRecoverable(action);
  return {
    frees: bytes > 0 ? bytes : null,
    recoverable,
    note: recoverable ? 'Recoverable — moves to the Trash' : 'Not recoverable',
  };
}

/**
 * Safety claims that are true for a given method.
 *
 * `SafetyChecker` gates paths; for commands it now also scans arguments, but it
 * still cannot know what an arbitrary command does internally. Saying so costs
 * one line and is the difference between a tool a senior engineer trusts and
 * one they don't.
 */
export function safetyLines(method) {
  const pathChecks = [
    { ok: true, text: 'Path is outside all protected locations' },
    { ok: true, text: 'No credential files detected alongside it' },
    { ok: true, text: 'Checked against your blocklist before deleting' },
  ];
  switch (method.type) {
    case 'remove_dir': case 'remove_file': case 'remove_dirs': case 'remove_files':
      return pathChecks;
    case 'move_to_trash':
      return [...pathChecks, { ok: true, text: 'Recoverable from the Trash until you empty it' }];
    case 'git_worktree_remove':
      return [
        { ok: true, text: 'Re-checked just before running: refused if the worktree has uncommitted or unpushed work, or is locked' },
        ...(method.delete_branch ? [{ ok: true, text: 'git branch -d refuses to delete a branch that is not merged' }] : []),
        { ok: true, text: 'Repository and worktree checked against protected paths' },
      ];
    case 'remove_ollama_orphans':
      return [
        { ok: true, text: 'Every manifest is re-read just before deleting; a blob any model now uses is kept' },
        { ok: true, text: 'Refused outright if any manifest cannot be parsed' },
        { ok: true, text: 'Store checked against protected paths and your blocklist' },
      ];
    case 'git_worktree_prune':
      return [{ ok: true, text: 'Only removes git’s records of worktrees that no longer exist on disk' }];
    case 'git_delete_branches':
      return [{ ok: true, text: 'git branch -d refuses to delete a branch that is not merged' }];
    case 'dedup_files':
      return [
        { ok: true, text: 'Every path keeps its file — duplicates become clones of one copy' },
        { ok: true, text: 'Contents are re-hashed before replacing; any mismatch aborts that group' },
      ];
    default:
      break;
  }
  const lines = [];
  if (method.type === 'command' && method.working_dir) {
    lines.push({ ok: true, text: 'Working directory checked against protected paths' });
  }
  lines.push({ ok: true, text: 'Command arguments checked against protected paths' });
  lines.push({
    ok: false,
    text: 'Void runs this command as-is; it does not inspect what the command deletes',
  });
  return lines;
}

/**
 * Ecosystem display names, matching the backend's `display_name()`, in the
 * backend's `Ecosystem::ALL` display order — AI ecosystems first.
 */
export const ECO_NAMES = {
  worktrees: 'Agent worktrees',
  agent_data: 'AI agent data',
  models: 'Local AI models',
  rust: 'Rust',
  node: 'Node.js',
  python: 'Python',
  apple: 'Apple / Xcode',
  docker: 'Docker',
  go: 'Go',
  java: 'Java / JVM',
  dot_net: '.NET',
  homebrew: 'Homebrew',
  jet_brains: 'JetBrains',
  system: 'System',
  projects: 'Stale projects',
};

export const ALL_ECOSYSTEMS = Object.keys(ECO_NAMES);

/** Ecosystems about AI tools, grouped ahead of the rest in Results. */
export const AI_ECOSYSTEMS = ['worktrees', 'agent_data', 'models'];

/** Human names for `item.agent` ids. Unknown ids are title-cased. */
export const AGENT_NAMES = {
  claude: 'Claude Code',
  cursor: 'Cursor',
  codex: 'Codex',
  conductor: 'Conductor',
  windsurf: 'Windsurf',
  copilot: 'GitHub Copilot',
  gemini: 'Gemini CLI',
  aider: 'Aider',
  zed: 'Zed',
  vscode: 'VS Code',
  ollama: 'Ollama',
  lmstudio: 'LM Studio',
  huggingface: 'Hugging Face',
  comfyui: 'ComfyUI',
  playwright: 'Playwright',
  puppeteer: 'Puppeteer',
};

export function agentName(id) {
  if (!id) return null;
  return AGENT_NAMES[id] || String(id).replace(/[-_]+/g, ' ').replace(/\b\w/g, (c) => c.toUpperCase());
}

/** Row labels per `ArtifactKind` (snake_case, as serde writes them). */
export const KIND_LABELS = {
  target_dir: 'target/',
  cargo_registry: 'Cargo registry',
  cargo_git_checkouts: 'Cargo git checkouts',
  node_modules: 'node_modules/',
  npm_cache: 'npm cache',
  yarn_cache: 'Yarn cache',
  pnpm_store: 'pnpm store',
  derived_data: 'DerivedData/',
  archives: 'Archives/',
  device_support: 'Device support',
  cocoa_pods_cache: 'CocoaPods cache',
  swift_package_cache: 'Swift package cache',
  dangling_images: 'Dangling images',
  unused_images: 'Unused images',
  build_cache: 'Build cache',
  stopped_containers: 'Stopped containers',
  unused_volumes: 'Unused volumes',
  go_build_cache: 'Go build cache',
  go_mod_cache: 'Go module cache',
  go_test_cache: 'Go test cache',
  downloads_dir: 'Downloads',
  trash_bin: 'Trash',
  system_logs: 'System logs',
  pip_cache: 'pip cache',
  pycache_dir: '__pycache__/',
  venv_dir: '.venv/',
  conda_cache: 'Conda cache',
  gradle_cache: 'Gradle cache',
  maven_repository: 'Maven repository',
  gradle_build_dir: 'build/',
  homebrew_cache: 'Homebrew cache',
  jet_brains_cache: 'JetBrains cache',
  nu_get_cache: 'NuGet cache',
  dot_net_bin_obj: 'bin/ obj/',
  simulator_devices: 'iOS Simulators',
  simulator_caches: 'Simulator caches',
  simulator_runtimes: 'Simulator runtimes',
  uv_cache: 'uv cache',
  python_tool_cache: 'Python tool cache',
  bun_cache: 'Bun cache',
  framework_build_cache: 'Framework build cache',
  playwright_browsers: 'Playwright browsers',
  puppeteer_browsers: 'Puppeteer browsers',
  maven_target: 'target/',
  container_vm_disk: 'Container VM disk',
  docker_data: 'Docker data',
  agent_worktree: 'Agent worktree',
  orphan_worktree: 'Orphaned worktree',
  prunable_worktree_refs: 'Stale worktree records',
  merged_agent_branches: 'Merged agent branches',
  agent_transcripts: 'Agent transcripts',
  agent_file_history: 'File-history snapshots',
  agent_debug_logs: 'Agent debug logs',
  agent_cache: 'Agent cache',
  editor_state_db: 'Editor state database',
  editor_workspace_storage: 'Editor workspace storage',
  ollama_model: 'Ollama model',
  ollama_orphan_blobs: 'Orphaned Ollama blobs',
  hugging_face_model: 'Hugging Face model',
  lm_studio_model: 'LM Studio model',
  torch_hub_cache: 'Torch hub cache',
  comfy_ui_models: 'ComfyUI models',
  duplicate_model_files: 'Duplicate model files',
  stale_project: 'Stale project',
};

/**
 * A short label for what the artifact is, used as the row's primary name.
 * Falls back to the path's last segment, because 15 kinds carry a synthetic
 * `project_name` and several carry none at all.
 */
export function kindLabel(item) {
  const KINDS = KIND_LABELS;
  // A Downloads entry is a specific file the user recognises by name; the
  // kind is the least useful thing to call it.
  if (item.kind === 'downloads_dir') {
    return String(item.path).replace(/\/$/, '').split('/').pop() || 'Downloads';
  }
  if (KINDS[item.kind]) return KINDS[item.kind];
  const parts = String(item.path).replace(/\/$/, '').split('/');
  return parts[parts.length - 1] || item.kind;
}

/**
 * Secondary label, suppressed when it merely restates the kind.
 * Several scanners set `project_name` to a synthetic label like
 * "Trash (412 items)" or "Homebrew" rather than a real project.
 */
export function projectLabel(item) {
  const name = item.project_name;
  if (!name) return null;
  const kind = kindLabel(item);
  if (name === kind || name.startsWith(kind)) return null;
  return name;
}

/** A detail value that means "no": `no`, `none`, `0`, `clean`, `false`, `—`. */
function isNegative(value) {
  return /^\s*(no|none|0|clean|false|—|-)\b/i.test(String(value ?? '')) || String(value ?? '').trim() === '';
}

/**
 * Branch and state chips for a worktree row, read from `item.details`.
 *
 * The scanner states facts as label/value pairs ("Branch", "Uncommitted
 * changes: 3 files", "Merged: yes"); this turns the ones that decide whether
 * a worktree is safe to drop into chips. Labels are matched loosely so a
 * reworded detail degrades to "no chip", never a wrong one.
 */
export function worktreeInfo(item) {
  const details = item.details || [];
  let branch = null;
  const chips = [];
  const add = (key, label, tone, title) => {
    if (!chips.some((c) => c.key === key)) chips.push({ key, label, tone, title });
  };

  for (const d of details) {
    const label = String(d.label || '').toLowerCase();
    const value = d.value;
    if (!branch && /^branch\b/.test(label)) { branch = String(value); continue; }
    if (/uncommitted|dirty|modified|changes/.test(label)) {
      if (!isNegative(value)) add('dirty', 'dirty', 'caution', `${d.label}: ${value}`);
    } else if (/unpushed|ahead|not pushed/.test(label)) {
      if (!isNegative(value)) add('unpushed', 'unpushed', 'caution', `${d.label}: ${value}`);
    } else if (/merged/.test(label)) {
      if (/^\s*(yes|true|merged)\b/i.test(String(value))) add('merged', 'merged', 'safe', `${d.label}: ${value}`);
    } else if (/locked/.test(label)) {
      if (!isNegative(value)) add('locked', 'locked', 'neutral', `${d.label}: ${value}`);
    }
  }
  return { branch, chips };
}
