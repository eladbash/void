/**
 * Drift guard: every `Ecosystem` and `ArtifactKind` in the Rust model must
 * have a frontend label, and the ecosystem order must match `Ecosystem::ALL`.
 * Adding a variant without a label fails here, not in front of a user.
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

import { ECO_NAMES, ALL_ECOSYSTEMS, KIND_LABELS, AI_ECOSYSTEMS } from '../js/actions.js';

const MODEL = readFileSync(
  fileURLToPath(new URL('../../../deepclean-core/src/model.rs', import.meta.url)), 'utf8',
);

/** serde `rename_all = "snake_case"`: an underscore before every inner capital. */
function snake(name) {
  return name.replace(/(?<!^)([A-Z])/g, '_$1').toLowerCase();
}

function enumVariants(name) {
  const m = MODEL.match(new RegExp(`pub enum ${name} \\{([\\s\\S]*?)\\n\\}`));
  assert.ok(m, `enum ${name} not found in model.rs`);
  return m[1]
    .split('\n')
    .map((l) => l.trim())
    .filter((l) => l && !l.startsWith('//') && !l.startsWith('#'))
    .map((l) => l.match(/^([A-Z][A-Za-z0-9]*)\s*[,{(]?/))
    .filter(Boolean)
    .map((m2) => m2[1]);
}

test('snake-casing matches serde', () => {
  assert.equal(snake('JetBrains'), 'jet_brains');
  assert.equal(snake('NuGetCache'), 'nu_get_cache');
  assert.equal(snake('HuggingFaceModel'), 'hugging_face_model');
  assert.equal(snake('ComfyUiModels'), 'comfy_ui_models');
});

test('every ArtifactKind has a label', () => {
  const kinds = enumVariants('ArtifactKind').map(snake);
  assert.ok(kinds.length >= 60, `parsed only ${kinds.length} kinds`);
  const missing = kinds.filter((k) => !KIND_LABELS[k]);
  assert.deepEqual(missing, [], `kinds without a label: ${missing.join(', ')}`);
  const stale = Object.keys(KIND_LABELS).filter((k) => !kinds.includes(k));
  assert.deepEqual(stale, [], `labels for kinds that no longer exist: ${stale.join(', ')}`);
});

test('every Ecosystem has a name and the order matches Ecosystem::ALL', () => {
  const variants = enumVariants('Ecosystem').map(snake);
  const missing = variants.filter((e) => !ECO_NAMES[e]);
  assert.deepEqual(missing, []);

  const all = MODEL.match(/pub const ALL: \[Ecosystem; \d+\] = \[([\s\S]*?)\];/);
  assert.ok(all, 'Ecosystem::ALL not found');
  const order = [...all[1].matchAll(/Self::([A-Za-z]+)/g)].map((m) => snake(m[1]));
  assert.deepEqual(ALL_ECOSYSTEMS, order);
});

test('ecosystem names match display_name()', () => {
  const body = MODEL.match(/pub fn display_name[\s\S]*?match self \{([\s\S]*?)\n\s*\}/);
  assert.ok(body);
  for (const [, variant, name] of body[1].matchAll(/Self::([A-Za-z]+) => "([^"]+)"/g)) {
    assert.equal(ECO_NAMES[snake(variant)], name, variant);
  }
});

test('AI ecosystems lead the list', () => {
  assert.deepEqual(ALL_ECOSYSTEMS.slice(0, AI_ECOSYSTEMS.length), AI_ECOSYSTEMS);
});

test('every ActionMethod is described', async () => {
  const { describeMethod } = await import('../js/actions.js');
  const methods = enumVariants('ActionMethod').map(snake);
  assert.ok(methods.includes('move_to_trash'));
  for (const type of methods) {
    assert.notEqual(describeMethod({ type, paths: [], branches: [], groups: [], blobs: [] }).text, 'Unknown action', type);
  }
});
