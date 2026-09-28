/** Builders shared by the frontend tests. Shapes mirror `deepclean-core::model`. */

let n = 0;
export function action(overrides = {}) {
  n += 1;
  return {
    id: `a${n}`,
    label: 'Remove',
    description: '',
    method: { type: 'remove_dir', path: '/x' },
    risk: 'safe',
    estimated_savings_bytes: 0,
    ...overrides,
  };
}

export function item(overrides = {}) {
  n += 1;
  return {
    id: `i${n}`,
    path: `/Users/dev/item-${n}`,
    ecosystem: 'node',
    kind: 'node_modules',
    risk: 'safe',
    size_bytes: 100,
    size_display: '100 B',
    last_modified: null,
    days_stale: null,
    project_name: null,
    project_root: null,
    available_actions: [],
    details: [],
    agent: null,
    ...overrides,
  };
}

/** A permanent delete and its Trash twin, as `trash::add_trash_alternatives` emits them. */
export function deleteAndTrash(path, risk = 'safe', bytes = 1000) {
  return [
    action({ label: 'Delete', method: { type: 'remove_dir', path }, risk, estimated_savings_bytes: bytes }),
    action({ label: 'Move to Trash', method: { type: 'move_to_trash', path }, risk, estimated_savings_bytes: bytes }),
  ];
}
