// Run by `cargo test` (see `the_hub_page_agrees_with_this_code` in tamale's src/web/e2e.rs)
// with vectors tamale produced: the hub's page must derive and open what tamale does. (The
// session page's end of the channel is Rust, in web/src/e2e.rs, checked by its own tests.)
// Prints the QR code it drew and a key it made, for tamale to check.
import assert from 'node:assert/strict';
import { fromKey, isKey, machineId, newKey, qrRows, qrSvg, ticketFits, toBase64Url, unseal, withLabels } from './seal.js';

const fromBase64 = (value) => Uint8Array.from(Buffer.from(value.replace(/-/g, '+').replace(/_/g, '/'), 'base64'));

const v = JSON.parse(process.argv[2]);
const psk = fromBase64(v.psk);

// What the hub's page derives from a key: the same as tamale and the hub.
const derived = await fromKey(v.key);
assert.equal(derived.auth, v.auth);
assert.deepEqual([...derived.psk], [...psk]);
assert.equal(derived.mid, v.mid);
assert.equal(derived.id, v.id);
assert.equal(toBase64Url(psk), v.psk);
assert.equal(await machineId(psk), v.mid);

// What counts as a key, as tamale decides it.
assert.equal(isKey(v.key), true);
assert.equal(isKey(v.key.slice(0, -1)), false);
assert.equal(isKey(`${v.key}a`), false);
assert.equal(isKey(v.key.toUpperCase()), false);
assert.equal(isKey(`thk_${'a'.repeat(51)}b`), false, 'spare bits set');
assert.equal(isKey('thv_abc'), false);
await assert.rejects(fromKey('thv_abc'));
assert.equal(v.spelling, v.key);
for (let i = 0; i < 50; i += 1) {
  const key = newKey();
  assert.equal(key.length, 56);
  assert.equal(isKey(key), true);
}

// The code the page draws for a key's link, which tamale's side reads back.
const rows = await qrRows(`https://tamale-hub.com/#${v.key}`);
assert.match(qrSvg(rows), /^<svg .*<\/svg>$/);

assert.deepEqual(await unseal(psk, v.sid, 'label', v.label), { host: 'h', cwd: '~/x' });
await assert.rejects(unseal(psk, v.sid, 'status', v.label), 'bound to its kind');
await assert.rejects(unseal(psk, 'f'.repeat(32), 'label', v.label), 'bound to its session');

// Labels say what they're for, and nothing else. They come from any machine with the key.
const row = { sid: 'real', open: '/open/real', online: true, approval: false, host: '', title: '' };
const shown = withLabels(
  row,
  { host: 'h', cwd: '~/x', model: 'label-model', sid: 'forged', open: 'javascript:alert(1)' },
  { title: 't', online: false, approval: true, encrypted: false },
);
assert.deepEqual(shown, {
  sid: 'real', open: '/open/real', online: true, approval: false,
  host: 'h', cwd: '~/x', title: 't', model: 'label-model', chef: false, encrypted: true,
});
// Only a label that says so makes a chef, and only with `true`.
assert.equal(withLabels(row, { chef: true }, {}).chef, true);
assert.equal(withLabels(row, { chef: 'yes' }, {}).chef, false);
assert.equal(withLabels(row, { model: 'label' }, { model: 'status' }).model, 'status');
assert.deepEqual(
  withLabels(row, { host: 7, cwd: null }, {}),
  { ...row, host: '', cwd: '', title: '', model: '', chef: false, encrypted: true },
  'what isn\'t text does not break the list',
);

// A session gets a key's secret only if the hub let it in with that key. Its `mid` isn't enough.
const held = { mid: v.mid, id: v.id };
assert.equal(ticketFits(held, { mid: v.mid, id: v.id }), true);
assert.equal(ticketFits(held, { mid: v.mid, id: 'another-key' }), false, 'borrowing the mid');
assert.equal(ticketFits(held, { mid: v.mid }), false);
assert.equal(ticketFits(held, { mid: 'other', id: v.id }), false);
assert.equal(ticketFits(undefined, { mid: v.mid, id: v.id }), false);

console.log(JSON.stringify({ rows, fresh: newKey() }));
