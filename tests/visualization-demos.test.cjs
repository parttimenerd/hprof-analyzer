const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const test = require('node:test');

const root = path.resolve(__dirname, '..');
const read = file => fs.readFileSync(path.join(root, file), 'utf8');
const overview = read('docs/ideas-overview.html');
const demos = read('docs/visualization-demos.html');

function scripts(html) {
  return [...html.matchAll(/<script(?:\s[^>]*)?>([\s\S]*?)<\/script>/g)]
    .map(match => match[1]).filter(Boolean);
}

test('all inline scripts on both review pages parse without external DOM packages', () => {
  for (const [page, html] of [['overview', overview], ['demos', demos]]) {
    scripts(html).forEach((source, i) => {
      // Module scripts use top-level await for dynamic imports. Wrapping in
      // an async function validates their syntax without loading CDN modules.
      const wrapped = `async function checkInlineModule() {\n${source}\n}`;
      assert.doesNotThrow(() => new vm.Script(wrapped, { filename: `${page}-inline-${i}.js` }));
    });
    assert.ok(scripts(html).length > 0);
  }
});

test('overview links to real detailed demos and has one card per current idea or shared UX pattern', () => {
  const ids = [...overview.matchAll(/<article class="card(?: collapsed)?" id="([A-Z]\d+)"/g)].map(m => m[1]);
  assert.equal(ids.length, 73); // 62 retained original ideas + P1–P4 + X1–X7
  assert.equal(new Set(ids).size, ids.length);
  const anchors = new Set([...demos.matchAll(/\bid="(demo-[^"]+)"/g)].map(m => m[1]));
  for (const [, anchor] of overview.matchAll(/href="visualization-demos\.html#([^"]+)"/g)) {
    assert.ok(anchors.has(anchor), `Missing linked demo #${anchor}`);
  }
});

test('priority cards disclose source, unavailable state, and illustrative example', () => {
  const priorityIds = ['A2','B2','B4','D4','E2','E3','G1','H2','H3','I4','J5','K1','K2','K5','L3','P1','P2','P3','P4'];
  for (const id of priorityIds) {
    const card = overview.split(`<article class="card" id="${id}"`, 2)[1]?.split('</article>', 1)[0];
    assert.ok(card, `Missing ${id}`);
    for (const marker of ['class="priority-demo"', 'data-demo-state="unavailable"', 'class="demo-source"', 'class="sample-table"']) {
      assert.ok(card.includes(marker), `${id} missing ${marker}`);
    }
  }
});

test('detailed examples avoid unsupported pool and finalizer conclusions', () => {
  const pool = demos.split('<h2 id="demo-pools">', 2)[1].split('<h2 ', 1)[0];
  assert.ok(pool.includes('not decoded'));
  assert.ok(!pool.includes('Busy / idle'));
  const finalizer = demos.split('<h2 id="demo-n2">', 2)[1].split('<h2 id="demo-pools">', 1)[0];
  assert.ok(finalizer.includes('registered finalizable objects'));
  assert.ok(!finalizer.includes('objects waiting for finalization'));
});
