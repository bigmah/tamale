// What the hub's page needs of a key (see single_key.md): making one, the values derived from
// it, and opening the session list's sealed labels. The key itself never reaches the hub.
// Nothing here touches the page, so noise.test.mjs can check it against tamale.

const encode = (s) => new TextEncoder().encode(s);

const KEY = /^thk_[a-z2-7]{52}$/;
const BASE32 = 'abcdefghijklmnopqrstuvwxyz234567';

export function fromBase64(value) {
  const binary = atob(value.replace(/-/g, '+').replace(/_/g, '/'));
  return Uint8Array.from(binary, (c) => c.charCodeAt(0));
}

export function toBase64Url(bytes) {
  return btoa(String.fromCharCode(...bytes)).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

const toHex = (bytes) => [...bytes].map((b) => b.toString(16).padStart(2, '0')).join('');

function encodeKey(bytes) {
  let out = 'thk_';
  let buffer = 0;
  let bits = 0;
  for (const byte of bytes) {
    buffer = ((buffer << 8) | byte) & 0xfff;
    bits += 8;
    while (bits >= 5) {
      bits -= 5;
      out += BASE32[(buffer >> bits) & 31];
    }
  }
  return out + BASE32[(buffer << (5 - bits)) & 31];
}

/** The bytes of a key, or null if `text` isn't one. A key has one spelling, as in tamale. */
function decodeKey(text) {
  if (!KEY.test(text)) return null;
  const out = new Uint8Array(32);
  let buffer = 0;
  let bits = 0;
  let at = 0;
  for (const c of text.slice(4)) {
    buffer = ((buffer << 5) | BASE32.indexOf(c)) & 0xfff;
    bits += 5;
    if (bits >= 8) {
      bits -= 8;
      out[at++] = (buffer >> bits) & 0xff;
    }
  }
  return (buffer & ((1 << bits) - 1)) === 0 ? out : null;
}

/** A new key, made here so the hub never sees it. */
export function newKey() {
  return encodeKey(crypto.getRandomValues(new Uint8Array(32)));
}

/** Whether `text` is a key. */
export function isKey(text) {
  return decodeKey(text) !== null;
}

async function derive(secret, info, bits) {
  const key = await crypto.subtle.importKey('raw', secret, 'HKDF', false, ['deriveBits']);
  const params = { name: 'HKDF', hash: 'SHA-256', salt: new Uint8Array(0), info: encode(info) };
  return new Uint8Array(await crypto.subtle.deriveBits(params, key, bits));
}

/** The id a session names its key by. */
export async function machineId(psk) {
  return toHex(await derive(psk, 'tamale-hub mid', 128));
}

/**
 * What a key means here: the auth value that proves it to the hub, the handshake's secret,
 * the id sessions name it by, and the id the hub's key list names it by.
 */
export async function fromKey(text) {
  const bytes = decodeKey(text);
  if (!bytes) throw new Error('Not a key.');
  const auth = toHex(await derive(bytes, 'tamale-hub auth', 256));
  const psk = await derive(bytes, 'tamale-hub e2e', 256);
  const hash = await crypto.subtle.digest('SHA-256', encode(auth));
  return {
    auth,
    psk,
    mid: await machineId(psk),
    id: toHex(new Uint8Array(hash)).slice(0, 16),
  };
}

/** Opens a label sealed for session `sid`; throws if the key or anything else is wrong. */
export async function unseal(psk, sid, kind, sealed) {
  const bytes = fromBase64(sealed);
  const key = await crypto.subtle.importKey('raw', await derive(psk, 'tamale-hub meta', 256), 'AES-GCM', false, ['decrypt']);
  const plain = await crypto.subtle.decrypt(
    { name: 'AES-GCM', iv: bytes.subarray(0, 12), additionalData: encode(`${sid}${kind}`) },
    key,
    bytes.subarray(12),
  );
  return JSON.parse(new TextDecoder().decode(plain));
}

/**
 * A session as the list shows it, with its opened labels. Only what labels are for comes from
 * them: they're written by whichever machine holds the key, and shouldn't get to say what the
 * session's address is, or anything else the hub vouches for.
 */
export function withLabels(session, label, status) {
  const text = (value) => (typeof value === 'string' ? value : '');
  return {
    ...session,
    host: text(label.host),
    cwd: text(label.cwd),
    title: text(status.title),
    model: text(status.model) || text(label.model),
    chef: label.chef === true,
    encrypted: true,
  };
}

/**
 * Whether a session is really under `held`, the key whose `mid` it gives. A session's `mid` is
 * only what it says of itself, and a page handed `held`'s secret could use it. The hub says
 * which key it let the session in with.
 */
export function ticketFits(held, ticket) {
  return Boolean(held) && held.mid === ticket.mid && held.id === ticket.id;
}

/** `text` as a QR code's rows of booleans, true for dark. */
export async function qrRows(text) {
  const { default: qrcode } = await import('./qrcode.js');
  const code = qrcode(0, 'M');
  code.addData(text, 'Byte');
  code.make();
  const size = code.getModuleCount();
  return Array.from({ length: size }, (_, y) => Array.from({ length: size }, (_, x) => code.isDark(y, x)));
}

/** QR rows as an SVG document, with the quiet zone a scanner needs. */
export function qrSvg(rows) {
  const quiet = 4;
  const size = rows.length + quiet * 2;
  let path = '';
  rows.forEach((row, y) => row.forEach((dark, x) => {
    if (dark) path += `M${x + quiet} ${y + quiet}h1v1h-1z`;
  }));
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${size} ${size}" shape-rendering="crispEdges">`
    + `<rect width="${size}" height="${size}" fill="#fff"/><path d="${path}" fill="#000"/></svg>`;
}
