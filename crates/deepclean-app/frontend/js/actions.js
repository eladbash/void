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
 * Exactly two methods in the whole product are recoverable, and they are both
 * shell one-liners, so this is matched on the command text.
 */
export function isRecoverable(action) {
  const m = action.method;
  if (m.type !== 'command') return false;
  const line = `${m.program} ${(m.args || []).join(' ')}`.toLowerCase();
  return (
    (line.includes('osascript') && line.includes('finder') && line.includes('delete')) ||
    (line.includes('shell.application') && line.includes('namespace(10)'))
  );
}

/**
 * Pick the action that runs by default.
 *
 * Deterministic and stated, replacing `available_actions[0]`:
 *   1. lowest risk           — keeps Trash over `rm -rf` for Downloads, and
 *                              avoids the Caution simulator wipe
 *   2. prefer Trash          — when the preference is on and risk ties
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

    if (preferTrash) {
      const ta = isRecoverable(candidate);
      const tb = isRecoverable(best);
      if (ta !== tb) return ta ? candidate : best;
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
    default:
      return { glyph: '▶', text: 'Unknown action', workingDir: null };
  }
}

/** Coarse grouping used by the confirmation dialog's "what will happen". */
export function methodClass(method) {
  if (method.type === 'remove_dir') return 'directories';
  if (method.type === 'remove_file') return 'files';
  return 'commands';
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
  if (method.type === 'remove_dir' || method.type === 'remove_file') {
    return [
      { ok: true, text: 'Path is outside all protected locations' },
      { ok: true, text: 'No credential files detected alongside it' },
      { ok: true, text: 'Checked against your blocklist before deleting' },
    ];
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

/** Ecosystem display names, matching the backend's `display_name()`. */
export const ECO_NAMES = {
  rust: 'Rust',
  node: 'Node.js',
  apple: 'Apple / Xcode',
  docker: 'Docker',
  go: 'Go',
  system: 'System',
  python: 'Python',
  java: 'Java / JVM',
  homebrew: 'Homebrew',
  jet_brains: 'JetBrains',
  dot_net: '.NET',
};

export const ALL_ECOSYSTEMS = Object.keys(ECO_NAMES);

/**
 * A short label for what the artifact is, used as the row's primary name.
 * Falls back to the path's last segment, because 15 kinds carry a synthetic
 * `project_name` and several carry none at all.
 */
export function kindLabel(item) {
  const KINDS = {
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
  };
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
