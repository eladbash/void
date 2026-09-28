import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  splitBytes, formatBytes, magnitude, staleShort, count, percent, duration,
  escapeHtml, tildify, setHome, pathHtml, sizeCell,
} from '../js/format.js';

test('bytes use base 1000 with a consistent decimal', () => {
  assert.deepEqual(splitBytes(0), { n: '0', u: 'B' });
  assert.deepEqual(splitBytes(999), { n: '999', u: 'B' });
  assert.deepEqual(splitBytes(1500), { n: '1.5', u: 'kB' });
  assert.deepEqual(splitBytes(999_970), { n: '1.0', u: 'MB' });
  assert.deepEqual(splitBytes(123_456_789_000), { n: '123', u: 'GB' });
  assert.deepEqual(splitBytes(null), { n: '—', u: '' });
  assert.deepEqual(splitBytes(-1), { n: '—', u: '' });
  // A narrow no-break space keeps number and unit together.
  assert.equal(formatBytes(12_400_000_000), '12.4\u202fGB');
});

test('magnitude tiers and the unknown size cell', () => {
  assert.equal(magnitude(0), 'unknown');
  assert.equal(magnitude(5e7), 'sm');
  assert.equal(magnitude(2e8), 'md');
  assert.equal(magnitude(2e9), 'lg');
  assert.equal(magnitude(2e10), 'xl');
  assert.match(sizeCell(0), /unknown/);
  assert.match(sizeCell(1500), /1\.5.*kB/);
});

test('staleness, percent and duration', () => {
  assert.equal(staleShort(null), '—');
  assert.equal(staleShort(0), 'today');
  assert.equal(staleShort(12), '12d');
  assert.equal(staleShort(730), '2.0y');
  assert.equal(percent(0, 0), '0%');
  assert.equal(percent(1, 1000), '<1%');
  assert.equal(percent(999, 1000), '>99%');
  assert.equal(percent(50, 100), '50%');
  assert.equal(duration(500), '500ms');
  assert.equal(duration(1500), '1.5s');
  assert.equal(duration(125000), '2m 05s');
  assert.equal(count(1234567), new Intl.NumberFormat().format(1234567));
});

test('escaping and paths', () => {
  assert.equal(escapeHtml(`<a href="x">'&'</a>`), '&lt;a href=&quot;x&quot;&gt;&#39;&amp;&#39;&lt;/a&gt;');
  setHome('/Users/dev/');
  assert.equal(tildify('/Users/dev/git/void'), '~/git/void');
  setHome('');
  assert.equal(tildify('/home/someone/x'), '~/x');
  const html = pathHtml('/Users/dev/git/<evil>/node_modules', 'git');
  assert.ok(!html.includes('<evil>'));
  assert.match(html, /s-leaf">node_modules/);
});
