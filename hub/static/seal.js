// What the hub's page needs of the pairing key (see e2e.md): the machine id it files keys
// under, and opening the session list's sealed labels. The key itself never reaches the
// hub. Nothing here touches the page, so noise.test.mjs can check it against tamale.

const encode = (s) => new TextEncoder().encode(s);

export function fromBase64(value) {
  const binary = atob(value.replace(/-/g, '+').replace(/_/g, '/'));
  return Uint8Array.from(binary, (c) => c.charCodeAt(0));
}

export function toBase64Url(bytes) {
  return btoa(String.fromCharCode(...bytes)).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

async function derive(psk, info, bits) {
  const key = await crypto.subtle.importKey('raw', psk, 'HKDF', false, ['deriveBits']);
  const params = { name: 'HKDF', hash: 'SHA-256', salt: new Uint8Array(0), info: encode(info) };
  return new Uint8Array(await crypto.subtle.deriveBits(params, key, bits));
}

/** The machine id of a pairing key. */
export async function machineId(psk) {
  const bytes = await derive(psk, 'tamale-hub mid', 128);
  return [...bytes].map((b) => b.toString(16).padStart(2, '0')).join('');
}

/** The pairing key in a link's fragment (`k=<base64url>`), or null if there isn't one. */
export function keyFromFragment(fragment) {
  const match = /^#?k=([A-Za-z0-9_-]{43})$/.exec(fragment);
  if (!match) return null;
  try {
    return fromBase64(match[1]);
  } catch {
    return null;
  }
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
