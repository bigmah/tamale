// Run by `cargo test` (see `the_browser_code_agrees_with_this_code` in src/web/e2e.rs)
// with vectors `snow` produced: this side must produce the same bytes and accept theirs.
// Prints what it sealed for tamale to open.
import assert from 'node:assert/strict';
import { Handshake, fromBase64, supported } from './noise.js';
import { fromKey, isKey, machineId, newKey, qrRows, qrSvg, ticketFits, toBase64Url, unseal, withLabels } from '../../hub/static/seal.js';

const v = JSON.parse(process.argv[2]);
const psk = fromBase64(v.psk);
const nonce = fromBase64(v.nonce);
const b64 = (bytes) => Buffer.from(bytes).toString('base64');

assert.equal(await supported(), true);

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
  host: 'h', cwd: '~/x', title: 't', model: 'label-model', encrypted: true,
});
assert.equal(withLabels(row, { model: 'label' }, { model: 'status' }).model, 'status');
assert.deepEqual(
  withLabels(row, { host: 7, cwd: null }, {}),
  { ...row, host: '', cwd: '', title: '', model: '', encrypted: true },
  'what isn\'t text does not break the list',
);

// A session gets a key's secret only if the hub let it in with that key. Its `mid` isn't enough.
const held = { mid: v.mid, id: v.id };
assert.equal(ticketFits(held, { mid: v.mid, id: v.id }), true);
assert.equal(ticketFits(held, { mid: v.mid, id: 'another-key' }), false, 'borrowing the mid');
assert.equal(ticketFits(held, { mid: v.mid }), false);
assert.equal(ticketFits(held, { mid: 'other', id: v.id }), false);
assert.equal(ticketFits(undefined, { mid: v.mid, id: v.id }), false);

// The handshake, from the same ephemeral key: the same bytes as snow's.
const payload = new TextEncoder().encode('{"v":1}');
const hs = await Handshake.start(psk, v.sid, nonce, fromBase64(v.ephemeral));
assert.equal(b64(await hs.first(payload)), v.hs1);

// A tampered answer isn't accepted.
const wrong = await Handshake.start(psk, v.sid, nonce, fromBase64(v.ephemeral));
await wrong.first(payload);
const bad = fromBase64(v.hs2);
bad[40] ^= 1;
await assert.rejects(wrong.finish(bad));
// Nor is the genuine answer under another key.
const other = await Handshake.start(new Uint8Array(32), v.sid, nonce, fromBase64(v.ephemeral));
await other.first(payload);
await assert.rejects(other.finish(fromBase64(v.hs2)));

const channel = await hs.finish(fromBase64(v.hs2));

// What tamale sealed, opened here in order, without awaiting each (as a socket delivers).
const opening = v.down.flat().map((sealed) => channel.open(sealed));
const opened = (await Promise.all(opening)).filter((m) => m !== null);
assert.deepEqual(opened, ['hello from tamale', '', v.long]);

// A replayed message fails, as does everything after it.
await assert.rejects(channel.open(v.down[0][0]));

// What tamale will open.
const up = await Promise.all(['hello from the browser', '', v.long].map((m) => channel.seal(m)));
console.log(JSON.stringify({ up: up.flat(), rows, fresh: newKey() }));
