// The hub's own page.
//   /            a landing page for visitors; the session list and keys for browsers
//                that hold them
//   /open/<sid>  the same, showing that session: it trades the browser's key for a ticket
//                and loads the session in a frame, which stays running behind the others
// Session labels come from agents and key names from whoever made them, so everything
// here is built with textContent.
//
// This page makes and holds keys (see single_key.md): every session is its own origin, and
// this is the one place that sees them all. The hub only ever gets a key's auth value; a
// session's page only the handshake's secret, in the fragment of the redirect into it.

import { fromKey, isKey, newKey, qrRows, qrSvg, ticketFits, toBase64Url, unseal, withLabels } from '/seal.js';

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
    return withLabels(s, label, status);
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
  if (secs < 60) return '';
  if (secs < 3600) return `${Math.floor(secs / 60)}m`;
  if (secs < 86400) return `${Math.floor(secs / 3600)}h`;
  return `${Math.floor(secs / 86400)}d`;
}

function el(tag, className, text) {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

const stateOf = (s) => (!s.online ? 'offline' : s.approval ? 'approval' : s.busy ? 'busy' : 'idle');

/** The last two parts of a path: enough to tell projects apart in a narrow list. */
function shortPath(path) {
  const parts = (path || '').split('/').filter(Boolean);
  return parts.length > 2 ? parts.slice(-2).join('/') : path || '';
}

const titleOf = (s) => (s.chef ? 'tamale chef' : s.title || shortPath(s.cwd).split('/').pop() || 'New session');

/**
 * The list's order, which only depends on where sessions are: rows that move when one gets
 * busy would be under the pointer one moment and gone the next, and would change what
 * Alt+1 means. The dots say what's going on. Gone machines sink, each to the end of its own.
 */
function ordered(sessions) {
  // A chef, which looks after all the others, comes first.
  const by = (s) => [s.chef ? 0 : 1, s.host || '', s.online ? 0 : 1, s.cwd || '', s.sid];
  return [...sessions].sort((a, b) => {
    const [x, y] = [by(a), by(b)];
    const i = x.findIndex((v, n) => v !== y[n]);
    return i < 0 ? 0 : x[i] < y[i] ? -1 : 1;
  });
}

/** A small inline icon, drawn with stroke like the rest of the page's. */
function icon(path) {
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
  svg.setAttribute('viewBox', '0 0 16 16');
  svg.setAttribute('aria-hidden', 'true');
  const p = document.createElementNS('http://www.w3.org/2000/svg', 'path');
  p.setAttribute('d', path);
  svg.append(p);
  return svg;
}
const POWER = 'M8 2.25v5.5M4.4 4.6a5 5 0 1 0 7.2 0';
const CROSS = 'm4.5 4.5 7 7M11.5 4.5l-7 7';

/** The chef's on/off pill. */
const chefPill = (on) => el('span', `switch ${on ? 'on' : 'off'}`, on ? 'On' : 'Off');

function sessionItem(s, showKey) {
  const state = stateOf(s);
  const closing = ending.has(s.sid);
  const link = el('a', `session ${state === 'offline' ? 'offline' : ''}${s.chef ? ' chef' : ''}`);
  link.href = s.open;
  link.dataset.sid = s.sid;
  link.append(s.chef ? el('span', 'chef-mark', '🫔') : el('span', `dot ${state}`), el('span', 'title', titleOf(s)));
  link.append(s.chef ? chefPill(s.online && !closing) : el('span', 'when', state === 'idle' ? ago(s.idle) : ''));
  const meta = el('span', `meta ${state}`);
  if (closing) meta.textContent = s.chef ? 'Stopping…' : 'Closing…';
  else if (s.chef) meta.textContent = state === 'offline' ? `Offline · ${s.host}` : state === 'busy' ? 'Asking around…' : `Looks after every session · ${s.host}`;
  else if (state === 'approval') meta.textContent = 'Needs approval';
  else if (state === 'busy') meta.textContent = `Working · ${shortPath(s.cwd)}`;
  else if (state === 'offline') meta.textContent = `Offline · ${shortPath(s.cwd)}`;
  else meta.textContent = [shortPath(s.cwd), s.model].filter(Boolean).join(' · ');
  if (!s.encrypted) meta.append(' ', el('span', 'tag', 'not encrypted'));
  link.append(meta);
  const details = [s.host, s.cwd, s.model];
  if (s.viewers) details.push(`${s.viewers} viewing`);
  if (showKey) details.push(`key: ${s.key}`);
  link.title = details.filter(Boolean).join('\n');
  const row = el('div', `session-row${s.chef ? ' chef' : ''}`);
  row.append(link);
  // What a session's own page does with its ⏻ button, from here: it's asked first.
  if (s.online) {
    const end = el('button', `row-end${s.chef ? ' labelled' : ''}`);
    end.type = 'button';
    end.dataset.end = s.sid;
    end.disabled = closing;
    const what = s.chef ? 'Stop the chef' : 'Close this session';
    end.title = what;
    end.setAttribute('aria-label', `${what}: ${titleOf(s)}`);
    end.append(icon(s.chef ? POWER : CROSS));
    if (s.chef) end.append(el('span', '', closing ? 'Stopping' : 'Stop'));
    row.append(end);
  }
  return row;
}

/** Where the chef goes when none is running: it's off, and how to turn it on. */
function chefOff() {
  const row = el('div', 'session-row chef off');
  const item = el('div', 'session chef chef-off');
  item.append(el('span', 'chef-mark', '🫔'), el('span', 'title', 'tamale chef'), chefPill(false));
  const meta = el('span', 'meta');
  meta.append('Start it with ', el('code', '', 'tamale chef'), ' or ', el('code', '', '/chef'), ' in any session');
  item.append(meta);
  item.title = 'The chef looks after every session on your keys. It runs on one of your machines.';
  row.append(item);
  return row;
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
/** Whether the list says which key each session is under (with more than one key). */
let listShowsKey = false;
/** Sessions being closed from the list, by sid, with when that started. */
const ending = new Map();
/** What the list shows, in order; `null` until the hub has answered. */
let sessions = null;
let hasKeys = false;
/** The session showing, its sid; `null` on the list or the keys. */
let current = null;
let drawn = '';

/** Which screen shows. The stylesheet does the rest, from `data-view` on <body>. */
function setView(view) {
  document.body.dataset.view = view;
}

function drawList(showKey = listShowsKey) {
  listShowsKey = showKey;
  const hosts = new Set(sessions.filter((s) => !s.chef).map((s) => s.host));
  const nodes = [];
  // The chef is a switch: listed first, on or off.
  if (sessions.length && !sessions.some((s) => s.chef)) nodes.push(chefOff());
  let host = null;
  for (const s of sessions) {
    // A chef stands above the machines, so it starts no group.
    if (!s.chef) {
      if (hosts.size > 1 && s.host !== host) nodes.push(el('div', 'group', s.host));
      host = s.host;
    }
    nodes.push(sessionItem(s, showKey));
  }
  // Every few seconds: don't take away the row under the keyboard if nothing changed.
  const shape = nodes.map((n) => n.outerHTML).join('');
  if (shape === drawn) return;
  drawn = shape;
  const focused = document.activeElement?.dataset?.sid;
  $('sessions').replaceChildren(...nodes);
  if (focused) $('sessions').querySelector(`[data-sid="${focused}"]`)?.focus();
}

/** Marks the session showing, and says what the window title and phone's bar should. */
function drawCurrent() {
  for (const row of $('sessions').querySelectorAll('a.session')) {
    row.classList.toggle('active', row.dataset.sid === current);
  }
  const showing = sessions?.find((s) => s.sid === current);
  const waiting = (sessions ?? []).filter((s) => s.approval && s.sid !== current).length;
  $('back-badge').textContent = waiting;
  $('back-badge').hidden = !waiting;
  $('bar-title').textContent = current ? (showing ? titleOf(showing) : '') : 'Keys';
  $('bar-dot').className = `dot ${showing ? stateOf(showing) : ''}`;
  $('bar-dot').hidden = !showing;
  document.title = `${waiting ? `(${waiting}) ` : ''}${showing ? titleOf(showing) : 'tamale-hub'}`;
}

async function refresh() {
  if (busy) return;
  const { ok, data } = await api('/api/sessions');
  if (!ok) return;
  const rows = await Promise.all(data.sessions.map(readable));
  const all = rows.filter(Boolean);
  const held = await Promise.all(data.keys.map((k) => keys.byId(k.id)));
  hasKeys = data.keys.length > 0;
  sessions = ordered(all);
  $('landing').hidden = hasKeys || !$('made').hidden || !$('opening').hidden;
  $('manage').hidden = !hasKeys;
  $('forget').hidden = !hasKeys;
  $('create').hidden = !data.signup;
  $('create-title').textContent = hasKeys ? 'Make another key' : 'Make a key';
  $('add-title').textContent = hasKeys ? 'Add another key' : 'Have a key?';
  $('empty').hidden = sessions.length > 0;
  $('key-list').replaceChildren(...data.keys.map((k, i) => keyItem(k, all, held[i])));
  drawList(data.keys.length > 1);
  // A session the hub has dropped has nothing left to reconnect to.
  for (const sid of [...frames.keys()]) {
    if (!sessions.some((s) => s.sid === sid)) closeFrame(sid);
  }
  if (current && !sessions.some((s) => s.sid === current)) {
    current = null;
    if (location.pathname.startsWith('/open/')) history.replaceState(null, '', '/');
  }
  const view = document.body.dataset.view;
  if (!hasKeys) setView('public');
  else if (hasKeys && !current && (view === 'public' || view === 'loading' || view === 'session')) setView('home');
  drawCurrent();
}

// ---- sessions' frames. Each is its session's own origin, as a tab of it would be: this page
// can't read it, and it talks to its tamale itself. Only so many stay connected at once.

const MAX_FRAMES = 4;
/** Open frames by sid, the most recently shown last. */
const frames = new Map();
/** Frames whose ticket is on its way, by sid, so a double click doesn't spend two. */
const opening = new Map();
/** When a frame last asked to be opened again, to stop if the ticket isn't taking. */
const reopened = new Map();

function closeFrame(sid) {
  frames.get(sid)?.remove();
  frames.delete(sid);
}

function showFrame(sid) {
  const frame = frames.get(sid);
  for (const other of frames.values()) {
    other.classList.toggle('active', other === frame);
    other.inert = other !== frame;
  }
  if (!frame) return;
  frames.delete(sid);
  frames.set(sid, frame);
  // Only pointers: on a phone, focus would throw the keyboard up over the session.
  if (matchMedia('(pointer: fine)').matches) frame.focus();
}

/** Why `sid` can't be shown, on the page that can take a key. */
function cantOpen(text) {
  current = null;
  $('opening-text').textContent = text;
  $('opening').hidden = false;
  setView(hasKeys ? 'keys' : 'public');
  drawCurrent();
}

/** Opens sessions as whole pages from now on, in this tab. */
function noFrames() {
  try { sessionStorage.setItem('hub.noframes', '1'); } catch {}
  framesOff = true;
}

let framesOff = (() => {
  try { return sessionStorage.getItem('hub.noframes') === '1'; } catch { return false; }
})();

/** Shows session `sid`, loading it first if its frame isn't open. */
async function showSession(sid) {
  if (framesOff) {
    const url = await ticketUrl(sid);
    if (typeof url !== 'string') return cantOpen(url.problem);
    // Back from the session should land on the list, not bounce into the session again.
    history.replaceState(null, '', '/');
    location.assign(url);
    return;
  }
  current = sid;
  $('opening').hidden = true;
  setView('session');
  drawCurrent();
  showFrame(sid);
  if (frames.has(sid)) return;
  const frame = await openFrame(sid);
  if (!(frame instanceof HTMLIFrameElement)) {
    if (current === sid) cantOpen(frame.problem);
    return;
  }
  if (current === sid) showFrame(sid);
}

/** `sid`'s frame, loading it (out of sight) if it isn't open; or `{ problem }`. */
async function openFrame(sid) {
  if (frames.has(sid)) return frames.get(sid);
  if (opening.has(sid)) return opening.get(sid);
  const load = (async () => {
    const url = await ticketUrl(sid);
    if (typeof url !== 'string') return url;
    const frame = el('iframe');
    frame.title = 'Session';
    frame.allow = 'clipboard-read; clipboard-write';
    frame.src = url;
    frame.inert = true;
    frames.set(sid, frame);
    $('frames').append(frame);
    // The oldest go, but not one that's being closed: it's what does the closing.
    const spare = [...frames.keys()].filter((s) => s !== sid && !ending.has(s));
    for (const old of spare.slice(0, Math.max(0, frames.size - MAX_FRAMES))) closeFrame(old);
    return frame;
  })();
  opening.set(sid, load);
  try {
    return await load;
  } finally {
    opening.delete(sid);
  }
}

// ---- closing a session from the list. Its page does it, over its own encrypted channel, so
// the session's frame is opened out of sight if it isn't open, and asked to.

/** How long to wait for a session to go before saying it didn't. */
const END_TIMEOUT = 30000;

async function endSession(sid) {
  const s = sessions?.find((x) => x.sid === sid);
  if (!s || ending.has(sid)) return;
  const where = s.host ? ` on ${s.host}` : '';
  const busyNote = s.busy || s.approval ? ' It\'s working, and stops mid-turn.' : '';
  const question = s.chef
    ? `Stop the tamale chef${where}?${busyNote} It stays saved, and your other sessions keep running. tamale chef or /chef starts it again.`
    : `Close “${titleOf(s)}”${where}?${busyNote} tamale ends it there, where it stays saved: tamale --continue picks it back up.`;
  if (!confirm(question)) return;
  if (framesOff) {
    alert('This browser opens sessions as whole pages, so close it from its own page: open it and use its close button.');
    return;
  }
  ending.set(sid, Date.now());
  drawn = '';
  drawList();
  const frame = await openFrame(sid);
  let timer;
  const done = (problem) => {
    clearInterval(timer);
    ending.delete(sid);
    drawn = '';
    if (sessions) drawList();
    if (problem) alert(problem);
  };
  if (!(frame instanceof HTMLIFrameElement)) return done(frame.problem);
  const origin = new URL(frame.src).origin;
  // Over and over until it's gone: the page may still be loading, and acts on the first.
  const tell = () => frame.contentWindow?.postMessage({ tamaleClose: true }, origin);
  frame.addEventListener('load', tell);
  tell();
  timer = setInterval(() => {
    if (sessions && !sessions.some((x) => x.sid === sid)) return done();
    if (Date.now() - ending.get(sid) > END_TIMEOUT) {
      return done(`${s.chef ? 'The chef' : 'That session'} didn't close in time. Open it to see why.`);
    }
    tell();
  }, 1000);
}

/** The address that lets this browser into `sid`, or `{ problem }`. */
async function ticketUrl(sid) {
  const { ok, data } = await api('/api/ticket', { sid });
  if (!ok || !data.url) return { problem: data.error || "Couldn't open that session." };
  if (!data.mid) return data.url;
  const held = await keys.byMid(data.mid);
  if (!ticketFits(held, data)) return { problem: "This browser doesn't have this session's key anymore. Scan its code or paste it." };
  return `${data.url}#k=${toBase64Url(held.psk)}`;
}

// ---- moving around.

function go(path) {
  if (location.pathname !== path) history.pushState(null, '', path);
  route();
}

/** Shows what the address says. */
function route() {
  const sid = location.pathname.match(/^\/open\/([0-9a-f]{32})$/)?.[1];
  if (sid) {
    showSession(sid);
  } else {
    current = null;
    showFrame(null);
    $('opening').hidden = true;
    if (document.body.dataset.view !== 'loading') setView(hasKeys ? 'home' : 'public');
    drawCurrent();
  }
}

function showKeys() {
  current = null;
  if (location.pathname !== '/') history.pushState(null, '', '/');
  showFrame(null);
  setView('keys');
  drawCurrent();
}

/** `Alt+↑`/`Alt+↓`, or `Alt+<digit>` as a number; the sessions' pages pass these along. */
function switchBy(move) {
  const list = sessions ?? [];
  if (!list.length) return;
  const at = list.findIndex((s) => s.sid === current);
  let next;
  if (move === 'up') next = list[at <= 0 ? list.length - 1 : at - 1];
  else if (move === 'down') next = list[(at + 1) % list.length];
  else next = list[move - 1];
  if (next) go(`/open/${next.sid}`);
}

/** The switch a key press asks for, if it does. */
function keySwitch(ev) {
  if (!ev.altKey || ev.ctrlKey || ev.metaKey) return null;
  if (ev.key === 'ArrowUp') return 'up';
  if (ev.key === 'ArrowDown') return 'down';
  // `code`, since Alt+1 types a character on a Mac.
  const digit = /^Digit([1-9])$/.exec(ev.code)?.[1];
  return digit ? Number(digit) : null;
}

addEventListener('keydown', (ev) => {
  const move = keySwitch(ev);
  if (move === null) return;
  ev.preventDefault();
  switchBy(move);
});

addEventListener('message', (ev) => {
  // Hub pages in a frame: its cookie ran out, and a session redirected it back here.
  if (ev.origin === location.origin && typeof ev.data?.hubReopen === 'string') {
    const sid = ev.data.hubReopen;
    if (frames.get(sid)?.contentWindow !== ev.source) return;
    closeFrame(sid);
    const last = reopened.get(sid) ?? 0;
    reopened.set(sid, Date.now());
    // Twice running means the browser won't keep a frame's cookie: use whole pages instead.
    if (Date.now() - last < 15000) noFrames();
    if (current === sid) showSession(sid);
    return;
  }
  // A session's page passing on a switch key, which only reaches the frame with focus.
  const move = ev.data?.tamaleHubSwitch;
  if (move === undefined) return;
  if (![...frames.values()].some((f) => f.contentWindow === ev.source)) return;
  if (move === 'up' || move === 'down' || (Number.isInteger(move) && move >= 1 && move <= 9)) switchBy(move);
});

document.addEventListener('click', (ev) => {
  const end = ev.target.closest('button[data-end]');
  if (!end) return;
  ev.preventDefault();
  endSession(end.dataset.end);
});

document.addEventListener('click', (ev) => {
  // Plain clicks stay in this page; a new tab or window gets the address as usual.
  if (ev.defaultPrevented || ev.button !== 0 || ev.metaKey || ev.ctrlKey || ev.shiftKey || ev.altKey) return;
  const row = ev.target.closest('a.session');
  const home = ev.target.closest('a[data-nav]');
  if (!row && !home) return;
  ev.preventDefault();
  go(row ? `/open/${row.dataset.sid}` : '/');
});

$('open-keys').addEventListener('click', showKeys);
$('back').addEventListener('click', () => go('/'));
addEventListener('popstate', route);

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
  $('page').scrollTop = 0;
  showKeys();
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
  await refresh();
  route();
});

$('forget').addEventListener('click', async () => {
  if (!confirm('Forget the keys on this browser? You will need to scan or paste them again.')) return;
  await api('/api/tokens', { forget: true });
  await keys.clear();
  for (const sid of [...frames.keys()]) closeFrame(sid);
  current = null;
  history.replaceState(null, '', '/');
  setView('public');
  refresh();
});

if (window !== window.top) {
  // A session's frame, sent here because its cookie ran out: the page around it starts it again.
  const sid = location.pathname.match(/^\/open\/([0-9a-f]{32})$/)?.[1];
  window.parent.postMessage({ hubReopen: sid }, location.origin);
} else {
  await takeFragment();
  await restoreCookie();
  // A link opened over this very page, from the scan screen say, only changes the fragment.
  addEventListener('hashchange', async () => {
    await takeFragment();
    await refresh();
    route();
  });
  await refresh();
  route();
  setInterval(() => { if (!document.hidden) refresh(); }, 3000);
  document.addEventListener('visibilitychange', () => { if (!document.hidden) refresh(); });
}
