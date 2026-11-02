// The browser's end of tamale's end-to-end encryption (see e2e.md and src/web/e2e.rs):
// Noise_NNpsk0_25519_AESGCM_SHA256 as an initiator, over WebCrypto, with the fragmenting
// of long messages above it. Written against the Noise spec (revision 34) and checked
// byte for byte against the `snow` crate by noise.test.mjs. Nothing here touches the page.

const subtle = () => globalThis.crypto.subtle;
const text = new TextEncoder();
const NAME = 'Noise_NNpsk0_25519_AESGCM_SHA256';
const PROLOGUE = 'tamale-hub e2e v1';
const MAX_NOISE = 65535;
const TAG = 16;
const MORE = 0;
const LAST = 1;
const MAX_PIECE = MAX_NOISE - TAG - 1;
const EMPTY = new Uint8Array(0);

function join(...parts) {
  const out = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const part of parts) {
    out.set(part, at);
    at += part.length;
  }
  return out;
}

export function toBase64(bytes) {
  let binary = '';
  for (let i = 0; i < bytes.length; i += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  }
  return btoa(binary);
}

export function fromBase64(value) {
  const binary = atob(value.replace(/-/g, '+').replace(/_/g, '/'));
  return Uint8Array.from(binary, (c) => c.charCodeAt(0));
}

const sha256 = async (data) => new Uint8Array(await subtle().digest('SHA-256', data));

async function hmac(key, data) {
  const k = await subtle().importKey('raw', key, { name: 'HMAC', hash: 'SHA-256' }, false, ['sign']);
  return new Uint8Array(await subtle().sign('HMAC', k, data));
}

/** Noise's HKDF: `count` 32-byte outputs. */
async function hkdf(chainKey, input, count) {
  const temp = await hmac(chainKey, input);
  const out = [];
  let last = EMPTY;
  for (let i = 1; i <= count; i++) {
    last = await hmac(temp, join(last, Uint8Array.of(i)));
    out.push(last);
  }
  return out;
}

/** AES-GCM under one key, with Noise's nonces: four zero bytes, then a big-endian counter. */
class Cipher {
  constructor(key) {
    this.raw = key;
    this.key = null;
    this.n = 0n;
  }

  async #next() {
    this.key ??= await subtle().importKey('raw', this.raw, 'AES-GCM', false, ['encrypt', 'decrypt']);
    const iv = new Uint8Array(12);
    new DataView(iv.buffer).setBigUint64(4, this.n++);
    return iv;
  }

  async encrypt(ad, plain) {
    const iv = await this.#next();
    return new Uint8Array(await subtle().encrypt({ name: 'AES-GCM', iv, additionalData: ad }, this.key, plain));
  }

  async decrypt(ad, sealed) {
    const iv = await this.#next();
    return new Uint8Array(await subtle().decrypt({ name: 'AES-GCM', iv, additionalData: ad }, this.key, sealed));
  }
}

async function newEphemeral(fixed) {
  if (!fixed) {
    const pair = await subtle().generateKey({ name: 'X25519' }, false, ['deriveBits']);
    return { privateKey: pair.privateKey, publicKey: new Uint8Array(await subtle().exportKey('raw', pair.publicKey)) };
  }
  // For tests: a raw private key, wrapped as PKCS#8 for import.
  const pkcs8 = join(Uint8Array.from([48, 46, 2, 1, 0, 48, 5, 6, 3, 43, 101, 110, 4, 34, 4, 32]), fixed);
  const usable = await subtle().importKey('pkcs8', pkcs8, { name: 'X25519' }, true, ['deriveBits']);
  const jwk = await subtle().exportKey('jwk', usable);
  return { privateKey: usable, publicKey: fromBase64(jwk.x) };
}

/** Whether this browser has what encrypted sessions need. */
export async function supported() {
  try {
    await newEphemeral();
    return true;
  } catch {
    return false;
  }
}

/**
 * The handshake, as the browser runs it:
 *   const hs = await Handshake.start(psk, sid, nonce);
 *   const hs1 = await hs.first(payload);           // to tamale
 *   const channel = await hs.finish(hs2);          // from tamale; throws if it's not genuine
 */
export class Handshake {
  static async start(psk, sid, nonce, fixedEphemeral) {
    const hs = new Handshake();
    // The spec hashes a protocol name only if it's longer than a hash; this one is exactly
    // 32 bytes, so it's used as is.
    hs.ck = text.encode(NAME);
    hs.h = hs.ck;
    await hs.#mixHash(join(text.encode(PROLOGUE), text.encode(sid), nonce));
    // psk: MixKeyAndHash.
    const [ck, temp, k] = await hkdf(hs.ck, psk, 3);
    hs.ck = ck;
    await hs.#mixHash(temp);
    hs.cipher = new Cipher(k);
    hs.e = await newEphemeral(fixedEphemeral);
    return hs;
  }

  async #mixHash(data) {
    this.h = await sha256(join(this.h, data));
  }

  async #mixKey(input) {
    const [ck, k] = await hkdf(this.ck, input, 2);
    this.ck = ck;
    this.cipher = new Cipher(k);
  }

  /** A public key in a message: with a PSK in play, it's mixed into the key too. */
  async #mixEphemeral(publicKey) {
    await this.#mixHash(publicKey);
    await this.#mixKey(publicKey);
  }

  async #encryptAndHash(plain) {
    const sealed = await this.cipher.encrypt(this.h, plain);
    await this.#mixHash(sealed);
    return sealed;
  }

  async #decryptAndHash(sealed) {
    const plain = await this.cipher.decrypt(this.h, sealed);
    await this.#mixHash(sealed);
    return plain;
  }

  /** Message 1: `psk, e`. */
  async first(payload) {
    await this.#mixEphemeral(this.e.publicKey);
    return join(this.e.publicKey, await this.#encryptAndHash(payload));
  }

  /** Message 2: `e, ee`. */
  async finish(message) {
    if (message.length < 32 + TAG) throw new Error('A short handshake message.');
    const theirs = message.subarray(0, 32);
    await this.#mixEphemeral(theirs);
    const key = await subtle().importKey('raw', theirs, { name: 'X25519' }, false, []);
    const shared = new Uint8Array(await subtle().deriveBits({ name: 'X25519', public: key }, this.e.privateKey, 256));
    await this.#mixKey(shared);
    await this.#decryptAndHash(message.subarray(32));
    const [send, receive] = await hkdf(this.ck, EMPTY, 2);
    return new Channel(new Cipher(send), new Cipher(receive));
  }
}

/**
 * One channel after its handshake. Both directions run in the order they were called: the
 * counters leave no room for anything else, and WebCrypto is asynchronous.
 */
export class Channel {
  #sending = Promise.resolve();
  #receiving = Promise.resolve();
  #partial = [];
  #partialLength = 0;

  constructor(send, receive) {
    this.send = send;
    this.receive = receive;
  }

  /** Seals one message; resolves to its transport messages, in base64. */
  seal(message) {
    const work = this.#sending.then(async () => {
      const bytes = text.encode(message);
      const sealed = [];
      let at = 0;
      do {
        const piece = bytes.subarray(at, at + MAX_PIECE);
        at += piece.length;
        const flag = at < bytes.length ? MORE : LAST;
        sealed.push(toBase64(await this.send.encrypt(EMPTY, join(Uint8Array.of(flag), piece))));
      } while (at < bytes.length);
      return sealed;
    });
    this.#sending = work.catch(() => {});
    return work;
  }

  /** Opens one transport message; resolves to the message once its last piece is in, else null. */
  open(sealed) {
    const work = this.#receiving.then(async () => {
      const plain = await this.receive.decrypt(EMPTY, fromBase64(sealed));
      if (!plain.length) throw new Error('An empty message.');
      const flag = plain[0];
      if (flag !== MORE && flag !== LAST) throw new Error('A bad fragment flag.');
      this.#partial.push(plain.subarray(1));
      this.#partialLength += plain.length - 1;
      if (flag === MORE) return null;
      const whole = join(...this.#partial);
      this.#partial = [];
      this.#partialLength = 0;
      return new TextDecoder('utf-8', { fatal: true }).decode(whole);
    });
    this.#receiving = work.catch(() => {});
    return work;
  }
}
