// Run by `cargo test` (see `the_browser_code_agrees_with_this_code` in src/web/e2e.rs)
// with vectors `snow` produced: this side must produce the same bytes and accept theirs.
// Prints what it sealed for tamale to open.
import assert from 'node:assert/strict';
import { Handshake, fromBase64, supported } from './noise.js';
import { keyFromFragment, machineId, toBase64Url, unseal } from '../../hub/static/seal.js';

const v = JSON.parse(process.argv[2]);
const psk = fromBase64(v.psk);
const nonce = fromBase64(v.nonce);
const b64 = (bytes) => Buffer.from(bytes).toString('base64');

assert.equal(await supported(), true);

// What the hub's page does with the key.
assert.deepEqual([...keyFromFragment(`#${v.fragment}`)], [...psk]);
assert.equal(toBase64Url(psk), v.fragment.slice(2));
assert.equal(keyFromFragment('#thv_abc'), null);
assert.equal(await machineId(psk), v.mid);
assert.deepEqual(await unseal(psk, v.sid, 'label', v.label), { host: 'h', cwd: '~/x' });
await assert.rejects(unseal(psk, v.sid, 'status', v.label), 'bound to its kind');
await assert.rejects(unseal(psk, 'f'.repeat(32), 'label', v.label), 'bound to its session');

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
console.log(JSON.stringify(up.flat()));
