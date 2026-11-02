// The hub's own page.
//   /            a landing page for visitors; the session list and keys for browsers
//                that hold them
//   /open/<sid>  trades the browser's key for a ticket into that session and goes there
// Session labels come from agents and key names from whoever made them, so everything
// here is built with textContent.
//
// This page makes and holds keys (see single_key.md): every session is its own origin, and
// this is the one place that sees them all. The hub only ever gets a key's auth value; a
// session's page only the handshake's secret, in the fragment of the redirect into it.

import { fromKey, isKey, newKey, qrRows, qrSvg, toBase64Url, unseal } from '/seal.js';

const $ = (id) => document.getElementById(id);

const keys = {
  /** A copy for when IndexedDB isn't there (a private window, say), by mid. */
  memory: new Map(),
  db: null,

  async open() {
    this.db ??= await new Promise((resolve, reject) => {
      // Version 1 held per-machine pairing keys, which nothing uses now.
      const request = indexedDB.open('tamale-hub', 2);
      request.onupgradeneeded = () => {
        if (request.result.objectStoreNames.contains('keys')) request.result.deleteObjectStore('keys');
        request.result.createObjectStore('keys');
      };
      request.onsuccess = () => {
        // Don't stand in the way of a newer version of this page in another tab.
        request.result.onversionchange = () => {
          request.result.close();
          this.db = null;
        };
        resolve(request.result);
      };
      request.onerror = () => reject(request.error);
      // An old tab won't let go: carry on without storage rather than wait for it forever.
      request.onblocked = () => reject(new Error('Blocked by another tab.'));
    });
    return this.db;
  },

  async run(mode, work) {
    const db = await this.open();
    return new Promise((resolve, reject) => {
      const request = work(db.transaction('keys', mode).objectStore('keys'));
      request.onsuccess = () => resolve(request.result);
      request.onerror = () => reject(request.error);
    });
  },

  async put(key) {
    const { mid } = await fromKey(key);
    this.memory.set(mid, key);
    await this.run('readwrite', (store) => store.put(key, mid)).catch(() => {});
  },

  /** Everything this browser holds, with what's derived from each. */
  async all() {
    const stored = await this.run('readonly', (store) => store.getAll()).catch(() => []);
    const every = [...new Set([...stored, ...this.memory.values()])].filter(isKey);
    return Promise.all(every.map(async (key) => ({ key, ...await fromKey(key) })));
  },

  async byMid(mid) {
    return (await this.all()).find((k) => k.mid === mid);
  },

  async byId(id) {
    return (await this.all()).find((k) => k.id === id);
  },

  async remove(key) {
    const { mid } = await fromKey(key);
    this.memory.delete(mid);
    await this.run('readwrite', (store) => store.delete(mid)).catch(() => {});
  },

  async clear() {
    this.memory.clear();
    await this.run('readwrite', (store) => store.clear()).catch(() => {});
  },
};

/** Opened labels by their sealed text, since the list refreshes every few seconds. */
const opened = new Map();

async function openLabel(psk, sid, kind, sealed) {
  if (!sealed) return {};
  const id = `${sid} ${kind} ${sealed}`;
  if (!opened.has(id)) {
    if (opened.size > 500) opened.clear();
    opened.set(id, await unseal(psk, sid, kind, sealed));
  }
  return opened.get(id);
}

/** A session as the list shows it, its labels opened; `null` without its key. */
async function readable(s) {
  if (!s.mid) return s;
  const held = await keys.byMid(s.mid);
  if (!held) return null;
  try {
    const label = await openLabel(held.psk, s.sid, 'label', s.sealed_label);
    const status = await openLabel(held.psk, s.sid, 'status', s.sealed_status);
    return { ...s, ...label, ...status, model: status.model || label.model || '', encrypted: true };
  } catch {
    return null;
  }
}

async function api(path, body) {
  const init = body === undefined ? {} : {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  };
  const response = await fetch(path, { ...init, credentials: 'same-origin', cache: 'no-store' });
  const data = await response.json().catch(() => ({}));
  return { ok: response.ok, status: response.status, data };
}

function showError(id, message) {
  $(id).textContent = message || '';
  $(id).hidden = !message;
}

/** Gives the hub this key's auth value and keeps the key. False, with why, if it's refused. */
async function addKey(key) {
  const { auth } = await fromKey(key);
  const { ok, data } = await api('/api/tokens', { add: auth });
  if (ok) await keys.put(key);
  else showError('error', data.error || 'That key was not accepted.');
  return ok;
}

// A key's link carries it in the fragment, which never reaches the server or its logs.
async function takeFragment() {
  const key = decodeURIComponent(location.hash.slice(1));
  if (!isKey(key)) return;
  history.replaceState(null, '', location.pathname + location.search);
  await addKey(key);
}

// The cookie outlives most things, but keys here can give it back if it's gone. A key the
// hub no longer knows, because it was replaced or deleted, is no use to keep.
async function restoreCookie() {
  const { ok, data } = await api('/api/sessions');
  if (!ok) return;
  const known = new Set(data.keys.map((k) => k.id));
  for (const held of await keys.all()) {
    if (known.has(held.id)) continue;
    const added = await api('/api/tokens', { add: held.auth });
    if (added.status === 401) await keys.remove(held.key);
  }
}

function ago(secs) {
  if (secs < 60) return 'active now';
  if (secs < 3600) return `idle ${Math.floor(secs / 60)}m`;
  if (secs < 86400) return `idle ${Math.floor(secs / 3600)}h`;
  return `idle ${Math.floor(secs / 86400)}d`;
}

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

function sessionItem(s, showKey) {
  const link = el('a', 'session' + (s.online ? '' : ' offline'));
  link.href = s.open;
  const top = el('div', 'row');
  if (s.encrypted) {
    const lock = el('span', 'lock', '🔒');
    lock.title = 'End-to-end encrypted: the hub can\'t read this session';
    top.append(lock);
  }
  top.append(el('span', 'title', s.title || 'New session'));
  let state;
  if (!s.online) state = el('span', 'state offline', 'offline');
  else if (s.approval) state = el('span', 'state approval', '● needs approval');
  else if (s.busy) state = el('span', 'state busy', '● working');
  else state = el('span', 'state idle', ago(s.idle));
  top.append(state);
  const meta = el('div', 'meta');
  meta.append(el('span', 'host', s.host), document.createTextNode(` · ${s.cwd} · ${s.model}`));
  if (s.viewers) meta.append(document.createTextNode(` · ${s.viewers} viewing`));
  if (showKey) {
    meta.append(document.createTextNode(' '));
    meta.append(el('span', 'tag', s.key));
  }
  link.append(top, meta);
  const item = el('li');
  item.append(link);
  return item;
}

function made(when) {
  const days = Math.floor((Date.now() / 1000 - when) / 86400);
  return days < 1 ? 'made today' : days === 1 ? 'made yesterday' : `made ${days} days ago`;
}

function keyItem(entry, sessions, held) {
  const item = el('li', 'key-row');
  const name = el('span', 'key-name', entry.name);
  const count = sessions.filter((s) => s.key === entry.name && s.online).length;
  const missing = held ? '' : ' · key not on this browser: add it below';
  name.append(el('span', 'key-meta', `${made(entry.created)} · ${count} connected${missing}`));
  const button = (label, className, onClick) => {
    const b = el('button', `small ${className}`, label);
    b.type = 'button';
    b.addEventListener('click', onClick);
    return b;
  };
  item.append(name);
  if (held) item.append(button('Show', '', () => showKey(held.key, `Key: ${entry.name}`)));
  item.append(
    button('New key', '', () => rotate(entry.name, held)),
    button('Delete', 'danger', () => remove(entry.name, held)),
  );
  return item;
}

let busy = false;

async function refresh() {
  if (busy) return;
  const { ok, data } = await api('/api/sessions');
  if (!ok) return;
  const rows = await Promise.all(data.sessions.map(readable));
  const sessions = rows.filter(Boolean);
  const held = await Promise.all(data.keys.map((k) => keys.byId(k.id)));
  const has = data.keys.length > 0;
  const showing = !$('made').hidden;
  $('landing').hidden = has || showing;
  $('list').hidden = !has;
  $('manage').hidden = !has;
  $('forget').hidden = !has;
  $('create').hidden = !data.signup;
  $('create-title').textContent = has ? 'Make another key' : 'Make a key';
  $('add-title').textContent = has ? 'Add another key' : 'Have a key?';
  $('key-names').textContent = has ? data.keys.map((k) => k.name).join(' · ') : '';
  $('sessions').replaceChildren(...sessions.map((s) => sessionItem(s, data.keys.length > 1)));
  $('empty').hidden = sessions.length > 0;
  $('key-list').replaceChildren(...data.keys.map((k, i) => keyItem(k, sessions, held[i])));
}

/** What a machine sets: just the key on tamale-hub.com, which tamale defaults to. */
function setupLines(key) {
  const lines = [`export TAMALE_HUB_KEY=${key}`];
  if (location.origin !== 'https://tamale-hub.com') {
    const socket = location.origin.replace(/^https:/, 'wss:').replace(/^http:/, 'ws:');
    lines.unshift(`export TAMALE_HUB_URL=${socket}`);
  }
  return lines.join('\n');
}

// Shows a key and what to do with it. The code is drawn here: the hub must not see the key.
async function showKey(key, title) {
  const link = `${location.origin}/#${key}`;
  $('made-title').textContent = title;
  $('made-env').textContent = setupLines(key);
  $('made-link').textContent = link;
  $('made-qr').src = 'data:image/svg+xml,' + encodeURIComponent(qrSvg(await qrRows(link)));
  $('made').hidden = false;
  $('landing').hidden = true;
  $('made').scrollIntoView({ behavior: 'smooth', block: 'start' });
}

$('made-done').addEventListener('click', () => {
  // Nothing secret stays on the page.
  $('made-env').textContent = '';
  $('made-link').textContent = '';
  $('made-qr').removeAttribute('src');
  $('made').hidden = true;
  refresh();
});

document.addEventListener('click', async (ev) => {
  const button = ev.target.closest('button.copy');
  if (!button) return;
  await navigator.clipboard.writeText($(button.dataset.copy).textContent);
  const label = button.textContent;
  button.textContent = 'Copied';
  setTimeout(() => { button.textContent = label; }, 1500);
});

$('create-form').addEventListener('submit', async (ev) => {
  ev.preventDefault();
  const button = $('create-button');
  button.disabled = true;
  busy = true;
  const key = newKey();
  const { auth } = await fromKey(key);
  const { ok, data } = await api('/api/keys', { auth, label: $('label').value.trim() || null });
  if (ok) await keys.put(key);
  busy = false;
  button.disabled = false;
  showError('create-error', ok ? '' : data.error || "Couldn't make a key.");
  if (!ok) return;
  $('label').value = '';
  await showKey(key, `Your key: ${data.name}`);
  refresh();
});

async function rotate(name, held) {
  if (!confirm(`Replace the key ${name}? The old one stops working, so every machine and browser using it needs the new one.`)) return;
  const key = newKey();
  const { auth } = await fromKey(key);
  const { ok, data } = await api('/api/keys/rotate', { name, auth });
  if (!ok) {
    alert(data.error || "Couldn't replace that.");
    return;
  }
  if (held) await keys.remove(held.key);
  await keys.put(key);
  await showKey(key, `New key: ${name}`);
  refresh();
}

async function remove(name, held) {
  if (!confirm(`Delete the key ${name}? Its sessions disconnect, and it stops working everywhere.`)) return;
  const { ok, data } = await api('/api/keys/delete', { name });
  if (!ok) alert(data.error || "Couldn't delete that.");
  else if (held) await keys.remove(held.key);
  refresh();
}

function needsKey(text) {
  $('opening-text').textContent = text;
  $('opening-back').hidden = false;
  $('add').hidden = false;
}

async function open(sid) {
  $('opening').hidden = false;
  for (const id of ['add', 'trust', 'create']) $(id).hidden = true;
  const { ok, data } = await api('/api/ticket', { sid });
  if (!ok || !data.url) {
    needsKey(data.error || "Couldn't open that session.");
    return;
  }
  if (!data.mid) {
    location.replace(data.url);
    return;
  }
  const held = await keys.byMid(data.mid);
  if (!held) {
    needsKey("This browser doesn't have this session's key anymore. Scan its code or paste it.");
    return;
  }
  location.replace(`${data.url}#k=${toBase64Url(held.psk)}`);
}

$('add-form').addEventListener('submit', async (ev) => {
  ev.preventDefault();
  const key = $('key').value.trim();
  if (!key) return;
  showError('error', '');
  if (!isKey(key)) {
    showError('error', "That isn't a key: they start with thk_.");
    return;
  }
  if (!await addKey(key)) return;
  $('key').value = '';
  if (location.pathname.startsWith('/open/')) location.reload();
  else refresh();
});

$('forget').addEventListener('click', async () => {
  if (!confirm('Forget the keys on this browser? You will need to scan or paste them again.')) return;
  await api('/api/tokens', { forget: true });
  await keys.clear();
  refresh();
});

await takeFragment();
await restoreCookie();
const opening = location.pathname.match(/^\/open\/([0-9a-f]{32})$/);
// A link opened over this very page, from the scan screen say, only changes the fragment.
addEventListener('hashchange', async () => {
  await takeFragment();
  if (opening) open(opening[1]);
  else refresh();
});
if (opening) {
  open(opening[1]);
} else {
  refresh();
  setInterval(() => { if (!document.hidden) refresh(); }, 5000);
  document.addEventListener('visibilitychange', () => { if (!document.hidden) refresh(); });
}
