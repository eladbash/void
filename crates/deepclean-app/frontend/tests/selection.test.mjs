import { test } from 'node:test';
import assert from 'node:assert/strict';

import { outermost, totalBytesOf, containsPath, innermostFirst } from '../js/selection.js';

const it = (path, size_bytes, id = path) => ({ id, path, size_bytes });

test('containsPath is component-wise', () => {
  assert.equal(containsPath('/a/foo', '/a/foo/bar'), true);
  assert.equal(containsPath('/a/foo/', '/a/foo'), true);
  assert.equal(containsPath('/a/foo', '/a/foobar'), false);
  assert.equal(containsPath('/a/foo/bar', '/a/foo'), false);
  assert.equal(containsPath('C:\\p\\x', 'C:/p/x/y'), true);
  assert.equal(containsPath('', '/a'), false);
});

test('outermost drops items inside another item', () => {
  const project = it('/u/proj', 5000);
  const nm = it('/u/proj/node_modules', 3000);
  const target = it('/u/proj/target', 1000);
  const other = it('/u/other/node_modules', 700);
  const out = outermost([nm, project, other, target]);
  assert.deepEqual(out.map((i) => i.id), ['/u/proj', '/u/other/node_modules']);
  assert.equal(totalBytesOf([nm, project, other, target]), 5700);
});

test('siblings sharing a prefix are not nested', () => {
  const a = it('/u/proj', 100);
  const b = it('/u/proj-old', 50);
  const c = it('/u/proj-old/node_modules', 40);
  const d = it('/u/proj/x', 10);
  assert.deepEqual(outermost([a, b, c, d]).map((i) => i.id), ['/u/proj', '/u/proj-old']);
});

test('a container too small to include the inner item does not swallow it', () => {
  // Stale worktree records live "at" the repo root but measure kilobytes.
  const refs = it('/u/repo', 4, 'refs');
  const nm = it('/u/repo/node_modules', 4000, 'nm');
  assert.deepEqual(outermost([refs, nm]).map((i) => i.id), ['refs', 'nm']);
  assert.equal(totalBytesOf([refs, nm]), 4004);
});

test('identical paths count once, keeping the first', () => {
  const a = it('/u/x', 10, 'a');
  const b = it('/u/x/', 10, 'b');
  assert.deepEqual(outermost([a, b]).map((i) => i.id), ['a']);
});

test('deep nesting and empty input', () => {
  const chain = [it('/a/b/c/d', 1), it('/a', 10), it('/a/b', 5), it('/a/b/c', 2)];
  assert.deepEqual(outermost(chain).map((i) => i.id), ['/a']);
  assert.deepEqual(outermost([]), []);
  assert.equal(totalBytesOf([]), 0);
});

test('innermostFirst runs nested items before their container', () => {
  const entries = [{ path: '/u/proj' }, { path: '/u/other' }, { path: '/u/proj/node_modules' }];
  assert.deepEqual(innermostFirst(entries).map((e) => e.path), ['/u/proj/node_modules', '/u/proj', '/u/other']);
});

test('outermost scales to thousands of items', () => {
  const many = [];
  for (let i = 0; i < 5000; i += 1) many.push(it(`/u/p${i}/node_modules`, 10));
  many.push(it('/u', 1e9));
  const t = Date.now();
  assert.equal(outermost(many).length, 1);
  assert.ok(Date.now() - t < 500, 'outermost must not be quadratic');
});
