// tamale's web UI: the conversation, and pullspace for the code, beside it or instead of it.
//
// One WebSocket carries everything (see src/web.rs). tamale sends the session as JSON:
// `entries` (from an index on, replacing what was there), `status`, `approval` and `question`.
// The page sends prompts, interrupts and approvals, and asks for what pullspace and the
// artifacts gallery need, whose replies carry the request's `seq`.

import { Handshake, fromBase64, supported, toBase64 } from '/noise.js';

const $ = (id) => document.getElementById(id);

const ESC = { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' };
const esc = (s) => String(s ?? '').replace(/[&<>"']/g, (c) => ESC[c]);

function h(tag, props = {}, ...children) {
  const node = document.createElement(tag);
  for (const [k, v] of Object.entries(props)) {
    if (v == null || v === false) continue;
    if (k === 'class') node.className = v;
    else if (k === 'html') node.innerHTML = v;
    else if (k === 'text') node.textContent = v;
    else if (k.startsWith('on')) node.addEventListener(k.slice(2), v);
    else node.setAttribute(k, v === true ? '' : v);
  }
  for (const c of children.flat()) if (c != null && c !== false) node.append(c);
  return node;
}

const ICON = {
  check: '<svg viewBox="0 0 16 16"><path d="m3.5 8.5 3 3 6-7"/></svg>',
  cross: '<svg viewBox="0 0 16 16"><path d="m4.5 4.5 7 7M11.5 4.5l-7 7"/></svg>',
  chev: '<svg viewBox="0 0 16 16"><path d="m6 3.5 4.5 4.5L6 12.5"/></svg>',
  open: '<svg viewBox="0 0 16 16"><path d="M9.5 2.5h4v4M13.5 2.5 7.5 8.5M12 9.5v3.25c0 .41-.34.75-.75.75h-8a.75.75 0 0 1-.75-.75v-8c0-.41.34-.75.75-.75H6.5"/></svg>',
  close: '<svg viewBox="0 0 16 16"><path d="m4 4 8 8M12 4l-8 8"/></svg>',
  spark: '<svg viewBox="0 0 16 16"><path d="M8 1.5 9.6 6.4 14.5 8 9.6 9.6 8 14.5 6.4 9.6 1.5 8l4.9-1.6Z"/></svg>',
  reload: '<svg viewBox="0 0 16 16"><path d="M13.2 6.3A5.3 5.3 0 1 0 13 10M13.5 2.8v3.7H9.8"/></svg>',
  expand: '<svg viewBox="0 0 16 16"><path d="M2.5 6V2.5H6M10 2.5h3.5V6M13.5 10v3.5H10M6 13.5H2.5V10"/></svg>',
  edit: '<svg viewBox="0 0 16 16"><path d="M10.5 2.5 13.5 5.5 6 13H3v-3ZM9 4l3 3"/></svg>',
  download: '<svg viewBox="0 0 16 16"><path d="M8 2.5v8M4.5 7.5 8 11l3.5-3.5M3 13.5h10"/></svg>',
  shrink: '<svg viewBox="0 0 16 16"><path d="M6 2.5V6H2.5M13.5 6H10V2.5M10 13.5V10h3.5M2.5 10H6v3.5"/></svg>',
  chat: '<svg viewBox="0 0 16 16"><path d="M2.5 3.75c0-.69.56-1.25 1.25-1.25h8.5c.69 0 1.25.56 1.25 1.25v6c0 .69-.56 1.25-1.25 1.25H7l-3 2.5V11h-.25c-.69 0-1.25-.56-1.25-1.25Z"/></svg>',
  back: '<svg viewBox="0 0 16 16"><path d="M10 3.5 5.5 8l4.5 4.5"/></svg>',
};

// ---------------------------------------------------------------- touch screens

/** A phone or tablet: no hover, a finger for a pointer, an on-screen keyboard. */
const touch = matchMedia('(hover: none) and (pointer: coarse)');
/** Too narrow for side-by-side panes. */
const narrow = matchMedia('(max-width: 760px)');
/** Too narrow for the chat and the code at once: it's one or the other. */
const tight = matchMedia('(max-width: 980px)');

/**
 * Sizes the app to the visible part of the page. iOS doesn't shrink the layout viewport
 * for the on-screen keyboard (it pans the page instead), so without this the composer
 * hides behind the keyboard and the top bar scrolls away.
 */
function fitViewport() {
  const vv = window.visualViewport;
  if (!vv || Math.abs(vv.scale - 1) > 0.01) return; // pinch-zoomed: leave the layout alone
  const height = `${Math.round(vv.height)}px`;
  const root = document.documentElement.style;
  if (root.getPropertyValue('--app-h') !== height) {
    const stick = nearBottom();
    root.setProperty('--app-h', height);
    if (stick) requestAnimationFrame(() => scrollToBottom());
  }
  if (scrollY) scrollTo(0, 0);
}

/**
 * Overlays the browser's Back button closes (the artifacts gallery, a full-screen
 * artifact), innermost first. On a phone Back is the natural way out; leaving the page
 * by accident would drop the session view.
 */
const layers = [];
/** History entries pushed for layers and not yet gone back over. */
let entries = 0;
let skipPops = 0;
let syncing = false;
/**
 * Brings the history entries in line with the open layers once the current change is done,
 * so closing one overlay and opening another reuses the entry rather than racing Back
 * (which is asynchronous) against a push.
 */
function syncHistory() {
  if (syncing) return;
  syncing = true;
  queueMicrotask(() => {
    syncing = false;
    while (entries < layers.length) window.history.pushState({ tamaleLayer: ++entries }, '');
    if (entries > layers.length) {
      skipPops++;
      window.history.go(layers.length - entries);
      entries = layers.length;
    }
  });
}
function pushLayer(close) {
  layers.push(close);
  syncHistory();
}
/**
 * Forgets `close` when its overlay is closed some other way, rewinding the entry it added.
 * When Back closed it, popstate has already taken it off, so this does nothing.
 */
function dropLayer(close) {
  const at = layers.lastIndexOf(close);
  if (at < 0) return;
  layers.splice(at, 1);
  syncHistory();
}
addEventListener('popstate', () => {
  if (skipPops) return void skipPops--;
  if (!entries) return;
  entries--;
  layers.pop()?.();
});

/** Focuses the prompt, except on touch screens, where that would pop up the keyboard. */
function focusPrompt() {
  if (!touch.matches) input.focus();
}

const fmtTokens = (n) =>
  n < 1000 ? `${n}` : n < 1e6 ? `${(n / 1000).toFixed(n < 1e4 ? 1 : 0)}k` : `${(n / 1e6).toFixed(1)}M`;

function fmtElapsed(s) {
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  return m < 60 ? `${m}m ${s % 60}s` : `${Math.floor(m / 60)}h ${m % 60}m`;
}

/** Plain text with `code` spans and links, for info lines, errors and toasts. */
function inline(text) {
  return esc(text)
    .replace(/`([^`\n]+)`/g, '<code>$1</code>')
    .replace(/\bhttps?:\/\/[^\s<)]+/g, (url) => `<a href="${url}" target="_blank" rel="noopener noreferrer">${url}</a>`);
}

/** A user message: its text, with fenced blocks (quoted code) set as code. */
function userHtml(text) {
  return text
    .split(/^```[^\n]*\n([\s\S]*?)^```[ \t]*$/m)
    .map((part, i) => (i % 2 ? `<pre><code>${esc(part.replace(/\n$/, ''))}</code></pre>` : inline(part)))
    .join('')
    .replace(/\n*(<pre>)/g, '$1')
    .replace(/(<\/pre>)\n*/g, '$1');
}

const store = {
  get: (key, fallback) => {
    try {
      const v = localStorage.getItem(`tamale.${key}`);
      return v == null ? fallback : JSON.parse(v);
    } catch {
      return fallback;
    }
  },
  set: (key, value) => {
    try {
      localStorage.setItem(`tamale.${key}`, JSON.stringify(value));
    } catch {}
  },
};

let toastTimer;
/** Says `text` for a while: above the prompt, and over the code when that has the page. */
function toast(text, ms = 5000) {
  const nodes = [$('toast'), $('toast-float')];
  for (const node of nodes) {
    node.innerHTML = inline(text);
    node.hidden = false;
  }
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => nodes.forEach((node) => (node.hidden = true)), ms);
}

async function copyText(text, what = 'Copied.') {
  try {
    await navigator.clipboard.writeText(text);
    toast(what, 1800);
  } catch {
    toast('The browser blocked the clipboard.');
  }
}

// ---------------------------------------------------------------- state

const state = {
  entries: [],
  /** Each entry's element, by index. */
  nodes: [],
  status: null,
  approval: null,
  approvalAt: 0,
  /** The agent's questions: { description, questions }, the answers so far and which is shown. */
  question: null,
  questionAt: 0,
  answers: [],
  asking: 0,
  /** When the current run began (unix seconds), and whether one finished out of sight. */
  runSince: null,
  doneUnseen: false,
};

// ---------------------------------------------------------------- connection

const token = new URLSearchParams(location.search).get('t') ?? '';
let ws;
let seq = 0;
let retry = 0;
const latest = {};
/** The next connection attempt, while one is waiting. */
let retryTimer = null;
/** When this connection was started, and when the socket last brought anything. */
let connectedAt = 0;
let lastHeard = 0;
/** Shows a dropped connection, if it's still down by then: a blip shouldn't flash. */
let downTimer = null;

/**
 * End-to-end encryption, on a session opened through a tamale-hub (see e2e.md). The hub's
 * redirect says so with `?e2e=1`, and the handshake's secret arrives in the fragment, which never
 * reaches a server; it's kept for this tab's reloads. `null` when this page talks plaintext,
 * as it does served locally. A hub that drops `e2e=1` gains nothing: tamale still refuses
 * unsealed messages, and answers with `hs0`, which this page takes as "you need a key".
 */
const e2e = (() => {
  const STORED = 'tamale.e2e.key';
  const given = /^#k=([A-Za-z0-9_-]{43})$/.exec(location.hash)?.[1];
  if (given) window.history.replaceState(null, '', location.pathname + location.search);
  let key = given;
  try {
    if (given) sessionStorage.setItem(STORED, given);
    else key = sessionStorage.getItem(STORED);
  } catch {}
  if (!key && !new URLSearchParams(location.search).has('e2e')) return null;
  return { key: key ? fromBase64(key) : null, forget: () => { try { sessionStorage.removeItem(STORED); } catch {} } };
})();
/** This connection's keys, once its handshake is done. */
let channel = null;
/** Set when reconnecting wouldn't help. */
let locked = false;

/** Whether the hub's page has heard this one has something to show. */
let toldShown = false;

/**
 * In a tamale-hub's page, which keeps this frame out of sight until it's drawn rather than
 * flash an empty page: says it is, once, after the next paint.
 */
function shown() {
  if (toldShown || window.parent === window) return;
  toldShown = true;
  const hub = `${location.protocol}//${location.host.split('.').slice(1).join('.')}`;
  requestAnimationFrame(() => setTimeout(() => window.parent.postMessage({ tamaleShown: true }, hub)));
}

/** Replaces the page with why it can't show this session, and a way on. */
function lock(title, text) {
  shown();
  locked = true;
  setConn('stopped', title);
  const sid = location.hostname.split('.')[0];
  const hub = location.host.split('.').slice(1).join('.');
  document.body.append(
    h('div', { class: 'locked' },
      h('div', { class: 'locked-box' },
        h('h1', { text: title }),
        h('p', { text }),
        hub && h('p', {}, h('a', { href: `${location.protocol}//${hub}/open/${sid}`, target: '_top', text: 'Open it through the hub' })))),
  );
}

// Inside a tamale-hub's page, which lists the sessions and says this page's name. Keys go to
// the frame with focus, so the ones that switch sessions are passed up to it.
if (window.parent !== window) {
  document.documentElement.classList.add('framed');
  const hub = `${location.protocol}//${location.host.split('.').slice(1).join('.')}`;
  addEventListener('keydown', (ev) => {
    if (!ev.altKey || ev.ctrlKey || ev.metaKey) return;
    const digit = /^Digit([1-9])$/.exec(ev.code)?.[1];
    const move = ev.key === 'ArrowUp' ? 'up' : ev.key === 'ArrowDown' ? 'down' : digit && Number(digit);
    if (!move) return;
    ev.preventDefault();
    ev.stopPropagation();
    window.parent.postMessage({ tamaleHubSwitch: move }, hub);
  }, true);
  // Back on this session from the hub's list: carry on typing.
  addEventListener('focus', () => {
    if (matchMedia('(pointer: fine)').matches) $('input')?.focus();
  });
  // The hub's list closing this session (it asked first): as soon as it's connected.
  addEventListener('message', (ev) => {
    if (ev.source !== window.parent || ev.origin !== hub || ev.data?.tamaleClose !== true) return;
    endSession();
  });
}

const NEEDS_KEY = "Scan this session's key on the hub's page, or paste it there.";

/** Handles one frame of an encrypted connection; throws if it isn't what's due. */
async function sealedFrame(link, data) {
  const frame = JSON.parse(data);
  if (frame.t === 'hs0' && !link.handshake && !link.sealed) {
    const sid = location.hostname.split('.')[0];
    link.handshake = await Handshake.start(e2e.key, sid, fromBase64(frame.n));
    const first = await link.handshake.first(new TextEncoder().encode('{"v":1}'));
    link.socket.send(JSON.stringify({ t: 'hs1', m: toBase64(first) }));
  } else if (frame.t === 'hs2' && link.handshake) {
    link.sealed = await link.handshake.finish(fromBase64(frame.m));
    link.handshake = null;
    if (link.socket !== ws) return;
    channel = link.sealed;
    ready();
  } else if (frame.t === 'box' && link.sealed) {
    const message = await link.sealed.open(frame.m);
    if (message === null) return;
    try {
      onMessage(JSON.parse(message));
    } catch (e) {
      console.error(e);
    }
  } else {
    throw new Error(`Refused an unexpected ${frame.t} frame.`);
  }
}

/** The connection is up and, if it's to be, encrypted. */
function ready() {
  retry = 0;
  clearTimeout(downTimer);
  downTimer = null;
  if (ending.due) return endSession();
  setConn('live', e2e ? 'Connected to tamale, end-to-end encrypted' : 'Connected to tamale');
  askChanges();
  for (const asked of review.held.splice(0)) reviewAsk(...asked);
  for (const id of artifactsDue.keys()) send({ t: 'artifact', id }, true);
}

function connect() {
  // wss: when the page came over https, as it does through a tamale-hub.
  const scheme = location.protocol === 'https:' ? 'wss' : 'ws';
  clearTimeout(retryTimer);
  retryTimer = null;
  const socket = new WebSocket(`${scheme}://${location.host}/ws?t=${encodeURIComponent(token)}`);
  ws = socket;
  channel = null;
  connectedAt = lastHeard = Date.now();
  ws.onopen = () => {
    if (!e2e) ready();
    else if (!downTimer) setConn('', 'Encrypting the connection…');
  };
  if (e2e) {
    // One at a time and in order: the handshake and the counters depend on it. Anything
    // that fails here drops the connection, and with it whatever was forged.
    let due = Promise.resolve();
    const link = { socket, handshake: null, done: false, sealed: null };
    ws.onmessage = (ev) => {
      lastHeard = Date.now();
      due = due.then(() => sealedFrame(link, ev.data)).catch((e) => {
        console.error(e);
        socket.close();
      });
    };
  } else {
    ws.onmessage = (ev) => {
      lastHeard = Date.now();
      try {
        const message = JSON.parse(ev.data);
        if (message.t === 'hs0') {
          socket.onclose = null;
          socket.close();
          lock('This session is encrypted', NEEDS_KEY);
          return;
        }
        onMessage(message);
      } catch (e) {
        console.error(e);
      }
    };
  }
  ws.onclose = (ev) => {
    if (socket === ws) channel = null;
    if (locked) return;
    if (e2e && ev.code === 4000 && ev.reason === 'e2e: wrong key') {
      e2e.forget();
      lock("This browser's key doesn't fit this session", NEEDS_KEY);
      return;
    }
    // A broken or throttled encrypted channel: a new connection starts a new handshake.
    if (ev.code === 4000 && ev.reason.startsWith('e2e: ')) {
      return reconnectLater(ev.reason.includes('too many') ? 30000 : 0);
    }
    if (ev.code === 4000) {
      // Over for good (the session ended, say): nothing to reconnect to.
      locked = true;
      clearTimeout(downTimer);
      setConn('stopped', ev.reason);
      toast(ev.reason || 'tamale stopped serving this page.', 1e9);
      return;
    }
    reconnectLater();
  };
}

/**
 * Tries again after a while, quietly: tamale's machine may be asleep for hours, or the hub
 * restarting. A page out of sight tries less often; coming back into view tries at once.
 */
function reconnectLater(at = 0) {
  clearTimeout(retryTimer);
  const cap = document.hidden ? 30000 : 5000;
  retryTimer = setTimeout(connect, Math.max(at, Math.min(cap, 250 * 2 ** retry++)));
  showDown();
}

function showDown() {
  if (downTimer) return;
  setConn('', 'Reconnecting…');
  downTimer = setTimeout(() => setConn('down', 'Reconnecting…'), 8000);
}

/** Drops this connection, which may have died without a word, and starts another. */
function reconnectNow() {
  if (locked || !ws) return;
  const old = ws;
  old.onclose = old.onmessage = null;
  try { old.close(); } catch {}
  channel = null;
  retry = 0;
  showDown();
  connect();
}

/** Whether this connection carries messages yet (on an encrypted session, once sealed). */
const isReady = () => ws?.readyState === WebSocket.OPEN && (!e2e || channel);

// A connection can die without closing: a phone that slept, a laptop that changed networks.
// Nothing arrives on one that's quiet, so while the page is in view it's checked now and
// then, with a ping that goes all the way to tamale and back.
// Generous, since a big message (a long session's history on a slow phone) arrives all at once.
const QUIET = 20000;
const GONE = 90000;
setInterval(() => {
  if (document.hidden || locked || !ws || retryTimer) return;
  const now = Date.now();
  if (!isReady()) {
    if (now - connectedAt > GONE) reconnectNow();
    return;
  }
  if (now - lastHeard > GONE) reconnectNow();
  else if (now - lastHeard > QUIET) send({ t: 'ping' }, true);
}, 10000);

/** Back in view or online: makes sure the connection still works, and reconnects if not. */
function checkConnection() {
  if (document.hidden || locked || !ws) return;
  if (retryTimer) {
    retry = 0;
    clearTimeout(retryTimer);
    connect();
    return;
  }
  // Something arrived just now, so it works.
  if (!isReady() || Date.now() - lastHeard < 10000) return;
  const asked = Date.now();
  send({ t: 'ping' }, true);
  setTimeout(() => {
    if (ws && isReady() && lastHeard < asked) reconnectNow();
  }, 8000);
}
addEventListener('visibilitychange', checkConnection);
addEventListener('online', checkConnection);
addEventListener('pageshow', (ev) => { if (ev.persisted) checkConnection(); });

function setConn(kind, title) {
  const node = $('conn');
  node.className = `conn ${kind}`;
  node.title = title;
}

/** Sends `message`; false if not connected. Unless `quiet`, says so when it isn't. */
function send(message, quiet = false) {
  if (e2e) {
    if (ws?.readyState === WebSocket.OPEN && channel) {
      const socket = ws;
      channel.seal(JSON.stringify(message)).then((boxes) => {
        for (const m of boxes) {
          if (socket.readyState === WebSocket.OPEN) socket.send(JSON.stringify({ t: 'box', m }));
        }
      }, (e) => {
        console.error(e);
        socket.close();
      });
      return true;
    }
  } else if (ws?.readyState === WebSocket.OPEN) {
    ws.send(JSON.stringify(message));
    return true;
  }
  if (!quiet) toast('Reconnecting to tamale. Try again in a moment.');
  return false;
}

/** Sends a request whose reply is only used if no newer one of its kind was made. */
function request(t, body = {}, kind = t) {
  const s = ++seq;
  latest[kind] = s;
  send({ t, seq: s, ...body }, true);
  return s;
}

function onMessage(m) {
  // The welcome's first message: drawn by the time it's painted.
  shown();
  switch (m.t) {
    case 'pong':
      return;
    case 'entries':
      return applyEntries(m.from, m.items);
    case 'status':
      return applyStatus(m.status);
    case 'approval':
      return applyApproval(m.approval);
    case 'question':
      return applyQuestion(m.question);
    case 'roots':
      askChanges();
      return reviewChanged();
    case 'diff_stat':
      if (m.seq === latest.diff_stat) applyChanges(m);
      return;
    case 'ps_snapshot':
    case 'ps_blobs':
      return reviewReply(m);
    case 'artifacts':
      if (m.seq === latest.artifacts) applyArtifacts(m.sessions);
      return;
    case 'artifact':
      return onArtifact(m);
    case 'kitchen':
      return applyKitchen(m.kitchen);
    case 'models':
      if (m.seq === latest.models) applyModels(m);
      return;
  }
}

// ---------------------------------------------------------------- chat

const scroller = $('scroller');
const thread = $('thread');

const nearBottom = () => scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < 80;

function scrollToBottom(smooth = false) {
  scroller.scrollTo({ top: scroller.scrollHeight, behavior: smooth ? 'smooth' : 'auto' });
}

scroller.addEventListener('scroll', () => ($('jump').hidden = nearBottom()), { passive: true });
$('jump').onclick = () => scrollToBottom(true);

function applyEntries(from, items) {
  const stick = nearBottom();
  const old = state.entries;
  let i = from;
  let k = 0;
  // Entries that kept their kind are updated in place (streaming text, tool output),
  // which keeps what the reader opened open.
  while (k < items.length && i < old.length && old[i].kind === items[k].kind) {
    old[i] = items[k];
    patchEntry(i);
    i++;
    k++;
  }
  for (let j = old.length - 1; j >= i; j--) removeEntry(j);
  old.length = i;
  state.nodes.length = i;
  for (; k < items.length; k++, i++) {
    old.push(items[k]);
    renderEntry(i);
  }
  $('hero').hidden = old.some((e) => e.kind !== 'info');
  updateGroups();
  updateActivity();
  if (stick) scrollToBottom();
  if (items.some((e) => e.kind === 'tool' && e.output != null)) afterToolResult();
}

/** The startup greeting, which the empty page says better. */
const isGreeting = (e, i) => i === 0 && e.kind === 'info' && e.text.startsWith('Welcome!');

function renderEntry(i) {
  const e = state.entries[i];
  let node;
  if (e.kind === 'tool') {
    node = h('div', { class: 'tool' });
    const prev = state.nodes[i - 1];
    let group = prev?.classList.contains('tool') ? prev.closest('.tools') : null;
    if (!group) {
      group = h(
        'div',
        { class: 'tools collapsed' },
        h('button', { class: 'tools-more', hidden: true, onclick: () => group.classList.toggle('collapsed') }),
        h('div', { class: 'tool-list' }),
      );
      thread.append(group);
    }
    group.querySelector('.tool-list').append(node);
  } else {
    node = h('div', { class: `msg ${e.kind}` });
    thread.append(node);
  }
  state.nodes[i] = node;
  patchEntry(i);
}

function removeEntry(i) {
  const node = state.nodes[i];
  if (!node) return;
  const group = node.closest('.tools');
  node.remove();
  if (group && !group.querySelector('.tool')) group.remove();
}

function patchEntry(i) {
  const e = state.entries[i];
  const node = state.nodes[i];
  switch (e.kind) {
    case 'user':
      node.innerHTML = userHtml(e.text);
      break;
    case 'assistant':
    case 'note':
      node.classList.add('prose');
      patchProse(node, e.html);
      break;
    case 'info':
      node.hidden = isGreeting(e, i);
      node.innerHTML = inline(e.text);
      break;
    case 'error':
      node.innerHTML = inline(e.text);
      break;
    case 'tool':
      patchTool(node, e);
      break;
  }
}

/** Shows "N earlier steps" on long runs of tool calls. */
function updateGroups() {
  for (const group of thread.querySelectorAll('.tools')) {
    const count = group.querySelectorAll('.tool').length;
    const more = group.querySelector('.tools-more');
    more.hidden = count <= 4;
    more.textContent = group.classList.contains('collapsed')
      ? `Show ${count - 4} earlier step${count - 4 === 1 ? '' : 's'}`
      : 'Show fewer steps';
  }
}
thread.addEventListener('click', (ev) => {
  if (ev.target.closest('.tools-more')) queueMicrotask(updateGroups);
});

/** Steps whose summary is the path of the file they read or changed. */
const FILE_TOOLS = new Set(['read_file', 'write_file', 'edit_file']);

function patchTool(node, e) {
  const pending = e.output == null;
  node._entry = e;
  node.classList.toggle('chef-step', CHEF_TOOLS.has(e.name));
  node.classList.toggle('pending', pending);
  node.classList.toggle('error', !pending && e.error);
  node.classList.toggle('done', !pending && !e.error);
  const summary = e.summary.split('\n')[0];
  // The summary is usually the description's subject ("Read src/main.rs"); skip it then.
  const showSummary = summary && !e.description.includes(summary);
  // A step on a file leads to it in the code view: what it changed, or the file it read.
  const opens = FILE_TOOLS.has(e.name) && summary && !state.status?.chef;
  const changes = opens && e.name !== 'read_file';
  let head = node.querySelector('.tool-head');
  if (!head) {
    head = h('button', {
      class: 'tool-head',
      onclick: (ev) => ev.target.closest('.tool-open') || toggleTool(node),
    });
    node.append(head);
  }
  head.title = `${e.name}: ${e.summary}`;
  head.innerHTML =
    `<span class="tool-icon">${e.error ? ICON.cross : ICON.check}</span>` +
    `<span class="tool-desc">${esc(e.description)}</span>` +
    `<span class="tool-sum">${showSummary ? esc(summary) : ''}</span>` +
    (opens
      ? `<span class="tool-open" data-path="${esc(summary)}"${changes ? ' data-diff' : ''} title="${changes ? 'See what changed in this file' : 'Open this file'}">` +
        `${ICON.open}${changes ? 'diff' : 'open'}</span>`
      : '') +
    `<span class="tool-chev">${ICON.chev}</span>`;
  if (node.classList.contains('open')) renderToolOutput(node);
}

function toggleTool(node) {
  node.classList.toggle('open');
  if (node.classList.contains('open')) renderToolOutput(node);
  else node.querySelector('.tool-out')?.remove();
}

function renderToolOutput(node) {
  const e = node._entry;
  let out = node.querySelector('.tool-out');
  if (!out) {
    out = h('div', { class: 'tool-out' });
    node.append(out);
  }
  const parts = [];
  if (e.name === 'bash' || e.summary.includes('\n')) parts.push(`<pre class="cmd">${esc(e.summary)}</pre>`);
  if (e.output == null) parts.push('<pre>Running…</pre>');
  else {
    const looksLikeDiff = /^(@@ |[+-]{3} )/m.test(e.output);
    const body = looksLikeDiff
      ? `<div class="difflines">${e.output
          .split('\n')
          .map((l) => {
            const cls = l.startsWith('@@') ? 'hunk' : /^\+(?!\+\+)/.test(l) ? 'add' : /^-(?!--)/.test(l) ? 'del' : '';
            return `<span class="${cls}">${esc(l) || ' '}</span>`;
          })
          .join('')}</div>`
      : esc(e.output || '(no output)');
    parts.push(`<pre>${body}${e.cut ? '<span class="cut">… output cut here</span>' : ''}</pre>`);
  }
  out.innerHTML = parts.join('');
}

/**
 * Sets a reply's HTML, replacing only the top-level blocks that changed, so streaming
 * text doesn't reload artifacts above it or undo what the reader did in them.
 */
function patchProse(node, html) {
  const fresh = h('template', { html }).content;
  const blocks = [...fresh.children];
  // Text outside elements (rare) can't be matched up; start over then.
  if (fresh.childNodes.length !== blocks.length) {
    node.innerHTML = html;
    decorate(node);
    mountArtifacts(node);
    return;
  }
  [...node.childNodes].forEach((c) => c.nodeType !== Node.ELEMENT_NODE && c.remove());
  const old = [...node.children];
  blocks.forEach((block, i) => {
    const source = block.outerHTML;
    if (old[i]?._source === source) return;
    block._source = source;
    if (old[i]) old[i].replaceWith(block);
    else node.append(block);
    decorate(block);
    mountArtifacts(block);
  });
  for (let i = blocks.length; i < old.length; i++) old[i].remove();
}

/** A file name from an artifact's title. */
const slug = (title) =>
  title.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 60) || 'artifact';

/** The artifact shown full screen, and how to close it. */
let fullArtifact = null;
function closeFullArtifact() {
  const box = fullArtifact;
  if (!box) return;
  fullArtifact = null;
  box.classList.remove('full');
  document.body.classList.remove('artifact-open');
  const btn = box.querySelector('.artifact-expand');
  if (btn) {
    btn.innerHTML = ICON.expand;
    btn.title = 'Full screen';
  }
  dropLayer(closeFullArtifact);
}
function openFullArtifact(box) {
  if (fullArtifact === box) return;
  closeFullArtifact();
  fullArtifact = box;
  box.classList.add('full');
  document.body.classList.add('artifact-open');
  const btn = box.querySelector('.artifact-expand');
  if (btn) {
    btn.innerHTML = ICON.shrink;
    btn.title = 'Exit full screen (esc)';
  }
  pushLayer(closeFullArtifact);
}

// ---- artifacts on an encrypted session. They can't be fetched through the hub by URL, so
// they come over the socket, and `/artifact-frame` (an empty page with the artifact CSP,
// see artifact-frame.html) is sent the document once it's loaded and says so.

/** The document at each artifact URL, once asked for. */
const artifactPages = new Map();
/** What's waiting for each artifact id. */
const artifactsDue = new Map();

function artifactHtml(url) {
  let page = artifactPages.get(url);
  if (!page) {
    const id = url.slice('/artifact/'.length);
    page = new Promise((resolve) => artifactsDue.set(id, resolve));
    artifactPages.set(url, page);
    send({ t: 'artifact', id }, true);
  }
  return page;
}

function onArtifact(m) {
  const url = `/artifact/${m.id}`;
  if (m.html == null) artifactPages.delete(url);
  artifactsDue.get(m.id)?.(m.html ?? '<p>No such artifact; it may be from an earlier run.</p>');
  artifactsDue.delete(m.id);
}

/** Shows the artifact at `url` in `frame`. */
function showInFrame(frame, url) {
  if (!e2e) {
    frame.src = url;
    return;
  }
  frame._artifact = url;
  frame.src = '/artifact-frame';
}

/** Tabs opened on an artifact, until they've been sent it. */
const artifactTabs = new Map();

function openArtifactTab(url) {
  if (!e2e) {
    open(url, '_blank', 'noopener,noreferrer');
    return;
  }
  // The frame page clears its opener before it writes the artifact.
  const tab = open('/artifact-frame', '_blank');
  if (tab) artifactTabs.set(tab, url);
  else toast('The browser blocked opening a new tab.');
}

async function saveArtifact(url, title) {
  const link = h('a', {
    href: URL.createObjectURL(new Blob([await artifactHtml(url)], { type: 'text/html' })),
    download: `${slug(title)}.html`,
  });
  link.click();
  setTimeout(() => URL.revokeObjectURL(link.href), 10_000);
}

/** Click handlers for an artifact's links, which on an encrypted session can't just go there. */
const onOpenLink = (url) => e2e && ((ev) => {
  ev.preventDefault();
  openArtifactTab(url);
});
const onSaveLink = (url, title) => e2e && ((ev) => {
  ev.preventDefault();
  saveArtifact(url, title);
});

addEventListener('message', async (ev) => {
  if (!e2e || ev.data?.tamaleFrameReady !== true || !ev.source) return;
  const frame = [...document.querySelectorAll('iframe')].find((f) => f.contentWindow === ev.source);
  const url = frame?._artifact ?? artifactTabs.get(ev.source);
  if (!url) return;
  artifactTabs.delete(ev.source);
  // The target can't be narrower: the frame's origin is opaque. It's the user's own
  // artifact, going to a frame this page just made.
  ev.source.postMessage({ tamaleArtifactHtml: await artifactHtml(url) }, '*');
});

/** Turns `.artifact[data-src]` placeholders into sandboxed embeds with a toolbar. */
function mountArtifacts(scope) {
  const found = scope.matches?.('.artifact[data-src]') ? [scope] : scope.querySelectorAll('.artifact[data-src]');
  for (const box of found) {
    if (box.querySelector('iframe')) continue;
    const src = box.dataset.src;
    const frame = h('iframe', {
      title: box.dataset.title,
      sandbox: 'allow-scripts allow-popups allow-modals allow-forms',
      referrerpolicy: 'no-referrer',
      loading: 'lazy',
    });
    showInFrame(frame, src);
    const toggleFull = () => (box.classList.contains('full') ? closeFullArtifact() : openFullArtifact(box));
    const bar = h(
      'div',
      { class: 'artifact-bar' },
      h('span', { class: 'artifact-icon', html: ICON.spark }),
      // On a phone the title is the biggest target there is: tap it for full screen.
      h('button', { class: 'artifact-title', text: box.dataset.title, title: box.dataset.title, onclick: toggleFull }),
      h('span', { class: 'spacer' }),
      h('button', {
        class: 'artifact-btn',
        title: 'Revise this artifact',
        html: ICON.edit,
        onclick: () => {
          closeFullArtifact();
          setInput(`Revise the "${box.dataset.title}" artifact: `);
        },
      }),
      h('a', {
        class: 'artifact-btn wide-only',
        title: 'Download as HTML (also saved with the session; /artifacts lists them)',
        href: src,
        download: `${slug(box.dataset.title)}.html`,
        onclick: onSaveLink(src, box.dataset.title),
        html: ICON.download,
      }),
      h('button', {
        class: 'artifact-btn wide-only',
        title: 'Reload',
        html: ICON.reload,
        onclick: () => showInFrame(frame, src),
      }),
      h('a', {
        class: 'artifact-btn',
        title: 'Open in a new tab',
        href: src,
        target: '_blank',
        rel: 'noopener noreferrer',
        onclick: onOpenLink(src),
        html: ICON.open,
      }),
      h('button', {
        class: 'artifact-btn artifact-expand',
        title: 'Full screen',
        html: ICON.expand,
        onclick: toggleFull,
      }),
    );
    box.append(bar, frame);
  }
}

// Artifacts report their height (see ARTIFACT_RESIZER in src/web.rs). Narrow screens make
// them tall, so the cap is generous: a frame that scrolls inside a scrolling page is
// awkward to use with a finger.
//
// A page sized in viewport units (`min-height: 100vh` plus a margin) grows a little every
// time its frame does; after a run of small, quick steps the frame stops following it.
addEventListener('message', (ev) => {
  const height = ev.data?.tamaleArtifactHeight;
  if (typeof height !== 'number') return;
  for (const frame of thread.querySelectorAll('.artifact iframe')) {
    if (frame.contentWindow !== ev.source) continue;
    const now = performance.now();
    const step = height - (parseFloat(frame.style.height) || 0);
    const run = (frame._growth ??= { count: 0, at: 0 });
    if (step > 0 && step <= 48 && now - run.at < 400) run.count++;
    else if (step !== 0) run.count = 0;
    run.at = now;
    if (step > 0 && run.count >= 6) continue;
    const stick = nearBottom();
    frame.style.height = `${Math.max(120, Math.min(height, 8000))}px`;
    if (stick) scrollToBottom();
  }
});

addEventListener('keydown', (ev) => {
  if (ev.key === 'Escape' && fullArtifact) {
    closeFullArtifact();
    ev.stopPropagation();
  }
}, true);

/** Copy buttons on code blocks, and file paths in `code` linked to the code view. */
function decorate(node) {
  const blocks = node.matches('.codeblock') ? [node] : node.querySelectorAll('.codeblock');
  for (const block of blocks) {
    const button = h('button', { class: 'copybtn', text: 'Copy' });
    button.onclick = () => {
      copyText(block.querySelector('code').textContent, 'Copied code.');
      button.textContent = 'Copied';
      button.classList.add('done');
      setTimeout(() => {
        button.textContent = 'Copy';
        button.classList.remove('done');
      }, 1400);
    };
    block.append(button);
  }
  for (const code of node.querySelectorAll('code:not(pre code)')) {
    const target = filePath(code.textContent);
    code.classList.toggle('pathlink', !!target);
    if (target) code.title = `Open ${target.path}${target.line ? `, line ${target.line}` : ''} in the code view`;
  }
}

/** Extensions that make a bare name (`app.js`, no directory) a file rather than `a.b`. */
const FILE_EXT =
  /^(rs|js|mjs|cjs|ts|tsx|jsx|py|go|rb|java|kt|swift|c|h|cc|cpp|hpp|cs|php|sh|bash|zsh|toml|yaml|yml|json|md|txt|html|css|scss|sql|lock|xml|svg|proto|vue|svelte|lua|ex|exs|zig|nix|tf|ini|cfg|conf)$/i;

/**
 * The file some text names, if it reads as one: a path with an extension, optionally with
 * `:line`, `:line-line` or `#Lline`. Whether there is such a file is pullspace's to say.
 */
function filePath(raw) {
  const text = raw.trim().replace(/^['"`]|['"`]$/g, '');
  const m = /^((?:~|\.{1,2})?\/?(?:[\w.@+-]+\/)*[\w.@+-]+\.(\w{1,8}))(?::(\d+)(?:[-:]\d+)*|#L(\d+)(?:-L?\d+)?)?$/.exec(text);
  if (!m || (!m[1].includes('/') && !FILE_EXT.test(m[2]))) return null;
  const line = Number(m[3] ?? m[4]) || null;
  return { path: m[1], line };
}

thread.addEventListener('click', (ev) => {
  const link = ev.target.closest('.pathlink, .tool-open');
  if (!link || state.status?.chef) return;
  ev.stopPropagation();
  const target = filePath(link.dataset.path ?? link.textContent) ?? { path: link.dataset.path, line: null };
  if (target.path) openInCode(target.path, { line: target.line, diff: link.hasAttribute('data-diff') });
});

// ---------------------------------------------------------------- activity

/** After this long on one step, the activity line says what it's waiting on. */
const LONG_STEP = 180;

/** Why a long step is still going, in words. */
function longStepNote(step, t) {
  switch (step.kind) {
    case 'think':
      return `No output from the model for ${t}. It may be reasoning through a big step; interrupt it if it looks stuck.`;
    case 'subagent':
      return `The subagent has been on its own for ${t}${step.steps ? ` (${step.steps} step${step.steps === 1 ? '' : 's'} so far)` : ''}. Only its final report goes back to the agent.`;
    case 'tool':
      return `This step has been running for ${t}. Its output shows up when it finishes.`;
    case 'wait':
      return `Waiting on you for ${t}. The run is paused until you answer.`;
    default:
      return '';
  }
}

function updateActivity() {
  const s = state.status;
  const node = $('activity');
  const running = !!s?.running;
  const stick = nearBottom();
  node.hidden = !running;
  if (!running) return;
  const now = Math.floor(Date.now() / 1000);
  const total = s.since ? Math.max(0, now - s.since) : null;
  // The server says what the run is doing (see `app::activity::step` in tamale).
  const step = { label: s.step || 'Thinking', kind: s.step_kind || 'think', steps: s.sub_steps || 0 };
  const stepTime = s.step_since ? Math.max(0, now - s.step_since) : total;
  node.classList.toggle('waiting', step.kind === 'wait');
  node.classList.toggle('long', stepTime != null && stepTime >= LONG_STEP);
  $('act-label').textContent = step.label;
  $('act-time').textContent = stepTime == null ? '' : fmtElapsed(stepTime);
  // The run's totals, once there's more to it than this one step.
  const steps = s.steps || 0;
  const meta = [];
  if (steps) meta.push(`${steps} step${steps === 1 ? '' : 's'}`);
  if (total != null && stepTime != null && total - stepTime >= 5) meta.push(`${fmtElapsed(total)} total`);
  $('act-meta').textContent = meta.join(' · ');
  const note = stepTime != null && stepTime >= LONG_STEP ? longStepNote(step, fmtElapsed(stepTime)) : '';
  $('act-note').textContent = note;
  $('act-note').hidden = !note;
  if (stick) scrollToBottom();
}
setInterval(() => state.status?.running && updateActivity(), 1000);

// ---------------------------------------------------------------- status

const MODES = ['ask', 'accept-edits', 'auto'];

function applyStatus(s) {
  const wasRunning = state.status?.running;
  state.status = s;
  if (s.chef) setUpChef();
  $('project-name').textContent = s.project;
  $('project-dirs').textContent = s.dirs.length ? `+${s.dirs.length}` : '';
  $('project').title = [s.cwd, ...s.dirs].join('\n');
  const heroSub = $('hero-sub');
  if (heroSub) heroSub.textContent = s.cwd;
  const mode = $('mode');
  mode.textContent = s.mode;
  mode.className = `pill mode ${s.mode}`;
  $('model').textContent = s.model + (s.effort ? ` · ${s.effort}` : '');
  $('model').title = `${s.model} via ${s.backend}${s.signed_in ? '' : ' (not signed in)'} · thinking ${s.thinking}\nClick to switch models`;
  const pct = s.window ? Math.min(100, Math.round((s.context * 100) / s.window)) : 0;
  $('ctx-fill').style.width = `${pct}%`;
  $('ctx').className = `ctx${pct >= 80 ? ' full' : pct >= 60 ? ' warn' : ''}`;
  $('ctx-label').textContent = `${fmtTokens(s.context)} / ${fmtTokens(s.window)}`;
  $('ctx').title = `Context ${pct}% full · ↑${fmtTokens(s.input_tokens)} ↓${fmtTokens(s.output_tokens)} this session`;
  $('cost').textContent = s.cost > 0 ? `$${s.cost.toFixed(s.cost < 1 ? 3 : 2)}${s.cost_partial ? '+' : ''}` : '';
  $('hint-left').textContent = s.goal ? `◎ ${s.goal_state || 'goal'}: ${s.goal}` : `via ${s.backend}`;
  $('queued').replaceChildren(...s.queued.map((q) => h('div', { class: 'queued-item', text: q })));
  // A long run that finished while you were away says so in the tab until you look.
  const ranLong = wasRunning && !s.running && state.runSince && Date.now() / 1000 - state.runSince >= 60;
  if (ranLong && !document.hasFocus()) state.doneUnseen = true;
  if (s.running) state.doneUnseen = false;
  state.runSince = s.running ? s.since : null;
  const name = s.chef ? 'chef' : s.project;
  document.title = `${s.running ? '● ' : state.doneUnseen ? '✓ ' : ''}${name} · tamale`;
  updateSend();
  updateActivity();
  // The agent may have changed files while it ran.
  if (wasRunning && !s.running) afterToolResult();
}

addEventListener('focus', () => {
  if (!state.doneUnseen) return;
  state.doneUnseen = false;
  document.title = document.title.replace(/^✓ /, '');
});

$('mode').onclick = () => {
  const next = MODES[(MODES.indexOf(state.status?.mode) + 1) % MODES.length];
  send({ t: 'send', text: `/permissions ${next}` });
};
$('model').onclick = () => openModels();

// ---------------------------------------------------------------- closing the session
//
// Ends the session on its machine, as `tamale kill` would there: it stays saved, so
// `tamale --continue` picks it back up. In the chef, this is how it's stopped.

const ending = { due: false, sent: false };

const connected = () => ws?.readyState === WebSocket.OPEN && (!e2e || !!channel);

/** Sends the close, now if connected, or as soon as it is. */
function endSession() {
  if (ending.sent) return;
  if (!connected()) {
    ending.due = true;
    return;
  }
  ending.due = false;
  ending.sent = true;
  send({ t: 'close' });
  $('end-session').disabled = true;
  setConn('down', 'Closing…');
  toast(state.status?.chef ? 'Stopping the chef…' : 'Closing the session…', 1e9);
}

function openEndDialog() {
  if (ending.sent) return;
  const s = state.status;
  const chef = !!s?.chef;
  $('end-title').textContent = chef ? 'Stop the tamale chef?' : 'Close this session?';
  const busy = s?.running ? (chef ? 'It\'s asking around, and stops mid-turn. ' : 'It\'s working, and stops mid-turn. ') : '';
  $('end-text').innerHTML = esc(busy) + (chef
    ? 'It stays saved, and your other sessions keep running. <code>tamale chef</code> or <code>/chef</code> starts it again.'
    : 'tamale ends it on its machine, where it stays saved: <code>tamale --continue</code> picks it back up.');
  $('end-go').textContent = chef ? 'Stop chef' : busy ? 'Stop and close' : 'Close session';
  $('ending').hidden = false;
  $('end-go').focus();
}

function closeEndDialog() {
  $('ending').hidden = true;
}

$('end-session').onclick = openEndDialog;
$('end-cancel').onclick = closeEndDialog;
$('end-go').onclick = () => {
  closeEndDialog();
  endSession();
};
$('ending').addEventListener('keydown', (ev) => {
  if (ev.key === 'Escape') {
    ev.preventDefault();
    ev.stopPropagation();
    closeEndDialog();
  }
});

// ---------------------------------------------------------------- chef
//
// A tamale chef (src/chef.rs) looks after the other sessions. Its page is this one in amber,
// with a kitchen panel of the sessions it can reach, kept current by `kitchen` messages.

const CHEF_TOOLS = new Set(['list_sessions', 'read_session', 'ask_sessions', 'message_session']);
const KITCHEN_STATES = {
  approval: { label: 'needs you', rank: 0 },
  working: { label: 'working', rank: 1 },
  idle: { label: 'idle', rank: 2 },
  offline: { label: 'offline', rank: 3 },
};
const TOQUE =
  '<svg class="toque" viewBox="0 0 64 64" aria-hidden="true">' +
  '<path class="toque-puff" d="M19 41c-6.6-1.4-11.5-7.2-11.5-14 0-7.9 6.4-14.3 14.3-14.3 1.9 0 3.7.4 5.4 1.1C29.6 8.9 34.5 5.5 40.2 5.5c7.9 0 14.3 6.4 14.3 14.3 0 .6 0 1.1-.1 1.7 2.9 2 4.6 5.2 4.6 8.8 0 5.4-3.9 9.9-9 10.7"/>' +
  '<path class="toque-body" d="M19 41h31v9H19z"/>' +
  '<path class="toque-pleat" d="M27 31v10M34.5 29v12M42 31v10"/>' +
  '<rect class="toque-band" x="17" y="50" width="35" height="8" rx="2"/>' +
  '</svg>';

let chefReady = false;

/** What the empty prompt says: who you're talking to, and on a keyboard, about `/`. */
function promptPlaceholder() {
  const who = chefReady ? 'Ask the chef about your sessions…' : 'Ask tamale anything…';
  return touch.matches ? who : `${who}   / for commands`;
}
let kitchenSeen = new Map();

function setUpChef() {
  if (chefReady) return;
  chefReady = true;
  document.body.classList.add('chef');
  // A chef has sessions to look after, not a repository to read.
  setView('chat', { focus: false });
  document.querySelector('meta[name="theme-color"]')?.setAttribute('content', '#1f1a12');
  document.querySelector('.brand-name').innerHTML = 'tamale <span class="brand-chef">chef</span>';
  $('end-session').title = 'Stop the chef  /quit';
  $('end-session').setAttribute('aria-label', 'Stop the chef');
  $('kitchen').hidden = narrow.matches;
  $('toggle-kitchen').hidden = false;
  input.placeholder = promptPlaceholder();
  const hero = $('hero');
  hero.classList.add('chef-hero');
  hero.innerHTML =
    `<div class="hero-mark chef-mark">${TOQUE}<span class="chef-tamale">🫔</span></div>` +
    '<h1>What\'s cooking?</h1>' +
    '<p class="hero-lede">I keep an eye on every tamale session on your machines. Ask me what they\'re doing, ' +
    'ask them questions, or pass them instructions.</p>' +
    '<div class="hero-kitchen" id="hero-kitchen"></div>' +
    '<div class="hero-chips">' +
    '<button class="chip" data-send="What is everyone working on?">What is everyone working on?</button>' +
    '<button class="chip" data-send="Which sessions need me right now, and for what?">Which sessions need me?</button>' +
    '<button class="chip" data-send="Ask all sessions whether their tests pass, and summarize.">Do the tests pass everywhere?</button>' +
    '<button class="chip" data-send="Are any two sessions working on the same files or likely to conflict?">Any conflicts brewing?</button>' +
    '</div>';
  bindChips(hero);
  if (state.kitchen) applyKitchen(state.kitchen);
}

function applyKitchen(k) {
  state.kitchen = k;
  if (!chefReady) return;
  const sessions = [...(k.sessions || [])].sort(
    (a, b) => (KITCHEN_STATES[a.state]?.rank ?? 9) - (KITCHEN_STATES[b.state]?.rank ?? 9),
  );
  const count = (st) => sessions.filter((s) => s.state === st).length;
  const needs = count('approval');
  const stats = [
    ['approval', needs, 'need you'],
    ['working', count('working'), 'working'],
    ['idle', count('idle'), 'idle'],
    ['offline', count('offline'), 'offline'],
  ].filter(([, n]) => n > 0);
  const statHtml = stats.map(([st, n, label]) => `<span class="k-stat st-${st}"><b>${n}</b> ${label}</span>`).join('');

  $('k-live').className = `k-live${k.loading ? ' loading' : k.problem ? ' stale' : ''}`;
  $('k-live').title = k.problem ? k.problem : 'Live: refreshes every few seconds';
  $('k-stats').innerHTML = statHtml;
  $('k-stats').hidden = !statHtml;
  $('kitchen-count').textContent = needs ? String(needs) : sessions.length ? String(sessions.length) : '';
  $('toggle-kitchen').classList.toggle('alert', needs > 0);

  const heroK = $('hero-kitchen');
  if (heroK) {
    heroK.innerHTML = k.loading
      ? '<span class="k-stat">Looking around the kitchen…</span>'
      : sessions.length
        ? `<span class="k-stat total"><b>${sessions.length}</b> session${sessions.length === 1 ? '' : 's'}</span>${statHtml}`
        : `<span class="k-stat">${esc(k.problem || 'No other sessions on the hub yet.')}</span>`;
  }

  const list = $('k-list');
  if (!sessions.length) {
    list.replaceChildren(
      h('div', { class: 'k-empty' },
        h('div', { class: 'k-empty-mark', html: TOQUE }),
        h('p', { text: k.loading ? 'Looking around…' : k.problem || 'No other sessions yet.' }),
        !k.loading && !k.problem &&
          h('p', { class: 'k-empty-hint', html: 'Turn on <kbd>/remote</kbd> in a session (with the same hub key) to bring it into the kitchen.' }),
      ),
    );
    return;
  }
  const seen = new Map();
  list.replaceChildren(
    ...sessions.map((s) => {
      seen.set(s.id, s.state);
      const changed = kitchenSeen.size && kitchenSeen.get(s.id) !== s.state;
      return kitchenCard(s, changed);
    }),
  );
  kitchenSeen = seen;
}

function kitchenCard(s, changed) {
  const place = [s.host, s.cwd].filter(Boolean).join(':') || s.id;
  const label = KITCHEN_STATES[s.state]?.label ?? s.state;
  const card = h('div', {
    class: `k-card st-${s.state}${changed ? ' changed' : ''}`,
    tabindex: 0,
    title: `${place}\nClick to ask what it's doing`,
  });
  card.innerHTML =
    `<div class="k-top"><span class="k-dot"></span><span class="k-host">${esc(s.host || '?')}</span>` +
    `<span class="k-id">${esc(s.id)}</span></div>` +
    `<div class="k-cwd">${esc(s.cwd)}</div>` +
    `<div class="k-what">${s.title ? esc(s.title) : '<i>Nothing asked yet</i>'}</div>` +
    `<div class="k-meta"><span class="k-state">${esc(label)}</span>` +
    (s.model ? `<span class="k-model">${esc(s.model)}</span>` : '') +
    (s.viewers ? `<span>${s.viewers} viewing</span>` : '') +
    '</div>' +
    '<div class="k-actions">' +
    (s.state === 'approval' ? '<button data-act="needs" class="primary">What does it need?</button>' : '') +
    '<button data-act="catchup">Catch me up</button>' +
    '<button data-act="ask">Ask…</button>' +
    '</div>';
  const who = `${s.id} (${place})`;
  const act = {
    needs: () => ask(`What is session ${who} waiting for me to approve, and should I?`),
    catchup: () => ask(`Catch me up on session ${who}: what was it asked, what has it done, and where is it now?`),
    ask: () => {
      setInput(`Ask ${s.id}: `);
      input.focus();
    },
    open: () => ask(`What is session ${who} working on right now?`),
  };
  card.onclick = (ev) => {
    const which = ev.target.closest('button')?.dataset.act ?? 'open';
    act[which]?.();
    if (narrow.matches) closeKitchen();
  };
  card.onkeydown = (ev) => {
    if (ev.key === 'Enter' && ev.target === card) act.open();
  };
  return card;
}

function openKitchen() {
  $('kitchen').hidden = false;
  $('kitchen').classList.add('drawer');
  $('toggle-kitchen').classList.add('on');
  pushLayer(closeKitchen);
}

function closeKitchen() {
  if (!narrow.matches) return;
  $('kitchen').hidden = true;
  $('kitchen').classList.remove('drawer');
  $('toggle-kitchen').classList.remove('on');
  dropLayer(closeKitchen);
}

$('toggle-kitchen').onclick = () => {
  if (!narrow.matches) {
    // Wide enough to sit beside the chat: the button shows or hides it.
    $('kitchen').hidden = !$('kitchen').hidden;
    $('toggle-kitchen').classList.toggle('on', !$('kitchen').hidden);
  } else if ($('kitchen').hidden) openKitchen();
  else closeKitchen();
};
$('k-close').onclick = () => {
  if (narrow.matches) closeKitchen();
  else {
    $('kitchen').hidden = true;
    $('toggle-kitchen').classList.remove('on');
  }
};
$('k-askall').onclick = () => {
  setInput('Ask all sessions: ');
  input.focus();
  if (narrow.matches) closeKitchen();
};
narrow.addEventListener('change', () => {
  if (!chefReady) return;
  $('kitchen').classList.remove('drawer');
  $('kitchen').hidden = narrow.matches;
  $('toggle-kitchen').classList.toggle('on', !narrow.matches);
});

// ---------------------------------------------------------------- approval

function applyApproval(a) {
  const had = !!state.approval;
  state.approval = a;
  const node = $('approval');
  node.hidden = !a;
  updateActivity();
  if (!a) {
    node.replaceChildren();
    if (had && !document.hasFocus()) document.title = document.title.replace(/^⚠ /, '');
    return;
  }
  state.approvalAt = Date.now();
  if (!had) showChat();
  const choose = (choice, reason) => send({ t: 'approve', choice, reason });
  const reasonRow = h(
    'div',
    { class: 'ap-reason', hidden: true },
    h('input', {
      placeholder: 'Tell the model what to do instead (optional)',
      onkeydown: (ev) => {
        if (ev.key === 'Enter') choose('no', ev.target.value);
        if (ev.key === 'Escape') {
          reasonRow.hidden = true;
          ev.stopPropagation();
        }
      },
    }),
    h('button', { class: 'btn danger', text: 'Deny', onclick: () => choose('no', reasonRow.firstChild.value) }),
  );
  const deny = () => {
    reasonRow.hidden = false;
    reasonRow.firstChild.focus();
  };
  node._deny = deny;
  node.replaceChildren(
    h('div', { class: 'ap-title', html: `Allow <code>${esc(a.name)}</code>?` }),
    h('div', { class: 'ap-desc', text: a.description }),
    h('div', { class: `ap-call${a.name === 'bash' ? ' bash' : ''}`, text: a.summary }),
    h(
      'div',
      { class: 'ap-actions' },
      h('button', { class: 'btn primary', onclick: () => choose('yes') }, h('span', { text: 'Allow' }), h('kbd', { text: 'Y' })),
      a.always.length
        ? h(
            'button',
            { class: 'btn', title: a.always.join(', '), onclick: () => choose('always') },
            h('span', { text: `Always allow ${a.always.join(', ')}` }),
            h('kbd', { text: 'A' }),
          )
        : null,
      h('button', { class: 'btn danger', onclick: deny }, h('span', { text: 'Deny…' }), h('kbd', { text: 'N' })),
    ),
    reasonRow,
  );
  if (!document.hasFocus()) document.title = `⚠ ${document.title}`;
  if (nearBottom()) scrollToBottom();
}

/** y / a / n answer the approval, unless they are being typed into something. */
function approvalKey(ev) {
  if (!state.approval || ev.metaKey || ev.ctrlKey || ev.altKey) return false;
  if (Date.now() - state.approvalAt < 300) return false;
  const t = ev.target;
  const typing = t.matches?.('input') || (t === input && input.value !== '');
  if (typing) return false;
  const key = ev.key.toLowerCase();
  if (key === 'y') send({ t: 'approve', choice: 'yes' });
  else if (key === 'a' && state.approval.always.length) send({ t: 'approve', choice: 'always' });
  else if (key === 'n') $('approval')._deny?.();
  else return false;
  ev.preventDefault();
  return true;
}

// ---------------------------------------------------------------- questions

const answered = (a) => a.chosen.length > 0 || a.other.trim() !== '';

function applyQuestion(q) {
  const had = state.question;
  const node = $('question');
  // A resync sends the same questions again; keep what's been picked.
  if (q && had && JSON.stringify(q) === JSON.stringify(had)) return;
  state.question = q;
  node.hidden = !q;
  updateActivity();
  input.placeholder = q ? 'Or reply in your own words…' : promptPlaceholder();
  if (!q) {
    node.replaceChildren();
    if (had && !document.hasFocus()) document.title = document.title.replace(/^⚠ /, '');
    return;
  }
  state.questionAt = Date.now();
  showChat();
  state.answers = q.questions.map(() => ({ chosen: [], other: '' }));
  state.asking = 0;
  renderQuestion();
  if (!document.hasFocus()) document.title = `⚠ ${document.title}`;
  if (nearBottom()) scrollToBottom();
}

/** Builds the card for the question being shown. */
function renderQuestion() {
  const set = state.question;
  const node = $('question');
  const i = state.asking;
  const q = set.questions[i];
  const many = set.questions.length > 1;
  const tabs = many
    ? h(
        'div',
        { class: 'q-tabs' },
        set.questions.map((qq, j) =>
          h(
            'button',
            { class: 'q-tab', 'data-i': j, onclick: () => goQuestion(j) },
            h('span', { class: 'q-mark' }),
            h('span', { text: qq.header || `Q${j + 1}` }),
          ),
        ),
      )
    : null;
  const options = q.options.map((o, row) =>
    h(
      'button',
      {
        class: `q-opt${q.multi_select ? ' multi' : ''}`,
        role: q.multi_select ? 'checkbox' : 'radio',
        'data-label': o.label,
        onclick: () => chooseOption(row),
      },
      h('span', { class: 'q-box' }),
      h('span', { class: 'q-text' }, h('span', { class: 'q-label', text: o.label }), o.description ? h('span', { class: 'q-desc', text: o.description }) : null),
      row < 9 ? h('kbd', { text: String(row + 1) }) : null,
    ),
  );
  const other = h('input', {
    class: 'q-other',
    placeholder: q.options.length ? 'Or type your own answer' : 'Your answer',
    oninput: (ev) => {
      const a = state.answers[state.asking];
      a.other = ev.target.value;
      if (!q.multi_select && a.other.trim()) a.chosen = [];
      refreshQuestion();
    },
    onkeydown: (ev) => {
      if (ev.key === 'Enter') {
        ev.preventDefault();
        nextQuestion();
      } else if (ev.key === 'Escape') {
        ev.target.blur();
        ev.preventDefault();
        ev.stopPropagation();
      }
    },
  });
  other.value = state.answers[i].other;
  const last = i === set.questions.length - 1;
  const parts = [
    h(
      'div',
      { class: 'q-head' },
      h('span', { class: 'q-title', text: set.description || 'tamale has a question' }),
      many ? h('span', { class: 'q-count', text: `${i + 1} of ${set.questions.length}` }) : null,
    ),
    tabs,
    h('div', { class: 'q-question', text: q.question }),
    q.multi_select ? h('div', { class: 'q-hint', text: 'Pick any that apply.' }) : null,
    h('div', { class: 'q-opts' }, options),
    other,
    h(
      'div',
      { class: 'ap-actions q-actions' },
      i > 0 ? h('button', { class: 'btn', onclick: () => goQuestion(i - 1) }, h('span', { text: 'Back' })) : null,
      h(
        'button',
        { class: 'btn primary q-next', onclick: nextQuestion },
        h('span', { text: last ? (many ? 'Send answers' : 'Send') : 'Next' }),
        h('kbd', { text: '⏎' }),
      ),
    ),
  ];
  node.replaceChildren(...parts.filter(Boolean));
  refreshQuestion();
}

/** Marks what's picked and answered, without rebuilding the card (or losing the field's focus). */
function refreshQuestion() {
  const node = $('question');
  const a = state.answers[state.asking];
  for (const opt of node.querySelectorAll('.q-opt')) {
    const on = a.chosen.includes(opt.dataset.label);
    opt.classList.toggle('on', on);
    opt.setAttribute('aria-checked', on);
  }
  for (const tab of node.querySelectorAll('.q-tab')) {
    const j = Number(tab.dataset.i);
    tab.classList.toggle('current', j === state.asking);
    tab.classList.toggle('done', answered(state.answers[j]));
  }
  const next = node.querySelector('.q-next');
  if (next) next.disabled = !answered(a);
}

function chooseOption(row) {
  const q = state.question.questions[state.asking];
  const label = q.options[row]?.label;
  if (label == null) return;
  const a = state.answers[state.asking];
  if (q.multi_select) {
    a.chosen = a.chosen.includes(label) ? a.chosen.filter((l) => l !== label) : [...a.chosen, label];
    // Keep the options' order.
    const order = q.options.map((o) => o.label);
    a.chosen.sort((x, y) => order.indexOf(x) - order.indexOf(y));
  } else {
    a.chosen = [label];
    a.other = '';
    const field = $('question').querySelector('.q-other');
    if (field) field.value = '';
  }
  refreshQuestion();
}

function goQuestion(i) {
  state.asking = Math.max(0, Math.min(i, state.question.questions.length - 1));
  renderQuestion();
}

/** Moves on once the shown question is answered; from the last, sends them all. */
function nextQuestion() {
  if (!state.question || !answered(state.answers[state.asking])) return;
  if (state.asking < state.question.questions.length - 1) return goQuestion(state.asking + 1);
  const missing = state.answers.findIndex((a) => !answered(a));
  if (missing >= 0) return goQuestion(missing);
  send({
    t: 'answer',
    answers: state.answers.map((a) => ({ chosen: a.chosen, other: a.other.trim() || undefined })),
  });
}

/** 1–9 pick, ⏎ moves on, ←/→ go between questions, unless they're being typed into something. */
function questionKey(ev) {
  if (!state.question || ev.metaKey || ev.ctrlKey || ev.altKey) return false;
  if (Date.now() - state.questionAt < 300) return false;
  const t = ev.target;
  if (t.matches?.('input') || (t === input && input.value !== '')) return false;
  if (/^[1-9]$/.test(ev.key)) chooseOption(Number(ev.key) - 1);
  else if (ev.key === 'Enter' && !ev.shiftKey) nextQuestion();
  else if (ev.key === 'ArrowLeft' && state.question.questions.length > 1) goQuestion(state.asking - 1);
  else if (ev.key === 'ArrowRight' && state.question.questions.length > 1) goQuestion(state.asking + 1);
  else return false;
  ev.preventDefault();
  return true;
}

// ---------------------------------------------------------------- composer

const input = $('input');
const history = store.get('history', []);
let historyAt = history.length;
let draft = '';

function autosize() {
  input.style.height = 'auto';
  input.style.height = `${Math.min(input.scrollHeight + 2, innerHeight * 0.4)}px`;
}

function setInput(text, caret = text.length) {
  input.value = text;
  autosize();
  input.focus();
  input.setSelectionRange(caret, caret);
  updateSend();
  updateSlash();
}

function updateSend() {
  const button = $('send');
  const empty = !input.value.trim();
  const running = !!state.status?.running;
  button.classList.toggle('ready', !empty);
  button.classList.toggle('stop', empty && running);
  button.title = empty && running ? 'Interrupt (esc)' : running ? 'Queue for the next step (⏎)' : 'Send (⏎)';
  // A prompt typed often enough to get a button, as in the iOS app; out of the way while typing.
  $('make-artifact').hidden = !empty;
  $('composer').classList.toggle('empty', empty);
}

/** The iOS app's artifact button sends the same words. */
const ARTIFACT_PROMPT = 'Can you generate an artifact for this?';

$('make-artifact').onclick = () => {
  if (send({ t: 'send', text: ARTIFACT_PROMPT })) scrollToBottom(true);
};

$('send').onclick = () => {
  if (!input.value.trim() && state.status?.running) send({ t: 'interrupt' });
  else submit();
};

function submit() {
  const text = input.value.trim();
  if (!text) return;
  if (history[history.length - 1] !== text) {
    history.push(text);
    if (history.length > 200) history.shift();
    store.set('history', history);
  }
  historyAt = history.length;
  draft = '';
  if (text.startsWith('/') && runLocal(text)) {
    setInput('');
    return;
  }
  if (send({ t: 'send', text })) {
    setInput('');
    // Put the keyboard away so the reply has the screen.
    if (touch.matches) input.blur();
    scrollToBottom(true);
  }
}

/** Commands, in the order the menu offers them; `local` ones run in the page. */
const COMMANDS = [
  { name: '/review', desc: 'Show the code and its uncommitted changes; /review <path> opens a file there', local: true },
  { name: '/help', desc: 'Commands and shortcuts', local: true },
  { name: '/model', args: '[id]', desc: 'Pick a model from the list, or switch to one: /model <id>' },
  { name: '/effort', args: '<level>', desc: 'low, medium, high, xhigh, max or default' },
  { name: '/thinking', args: '[on|off]', desc: 'Toggle adaptive thinking' },
  { name: '/permissions', args: '[mode]', desc: 'Show the rules, or set ask, accept-edits, auto' },
  { name: '/compact', args: '[focus]', desc: 'Summarize the conversation to free up context' },
  { name: '/goal', args: '[<condition>|pause|resume|clear]', desc: 'Keep working until the condition holds' },
  { name: '/undo', desc: 'Restore files the last turn changed and drop it' },
  { name: '/add-dir', args: '[path]', desc: "Add a directory to the agent's scope" },
  { name: '/resume', args: '<n|id>', desc: 'Continue a saved session' },
  { name: '/copy', args: '[all]', desc: 'Copy the latest reply to the clipboard' },
  { name: '/artifacts', desc: 'Browse artifacts from this and earlier sessions', local: true },
  { name: '/clear', desc: 'Clear the conversation and start fresh' },
  { name: '/web off', desc: 'Stop serving this page' },
  { name: '/quit', desc: 'Close this session (it stays saved); in the chef, stop it', local: true },
];

/** Commands that open a picker or screen in the terminal when given no arguments. */
const TERMINAL_ONLY = {
  '/effort': 'Set it with `/effort <level>`: low, medium, high, xhigh, max or default.',
  '/resume': 'Resume with `/resume <n|id>` here, or `/resume` in the terminal for the list.',
  '/continue': 'Resume with `/resume <n|id>` here, or `/resume` in the terminal for the list.',
  '/backend': 'Use `/backend api|claude-code|codex` here, or `/backend` in the terminal for the list.',
  '/rewind': '`/rewind` picks a turn in the terminal; `/undo` works here.',
  '/login': 'Sign-in runs in the terminal tamale is running in.',
  '/paste': 'Paste with ⌘V here.',
  '/mouse': '`/mouse` only affects the terminal.',
  '/browser': '`/browser` mirrors the terminal; you are already in the web UI.',
};

function runLocal(text) {
  const [name, ...rest] = text.split(/\s+/);
  const arg = text.slice(name.length).trim();
  switch (name) {
    // pullspace is the page's code browser and diff viewer, so these open it too.
    case '/review':
    case '/code':
    case '/browse':
    case '/diff':
    case '/files': {
      const target = arg && (filePath(arg) ?? { path: arg, line: null });
      if (target) openInCode(target.path, { line: target.line });
      else toggleReview();
      return true;
    }
    case '/help':
      openHelp();
      return true;
    case '/quit':
    case '/exit':
      openEndDialog();
      return true;
    case '/artifacts':
      openArtifacts();
      return true;
    case '/model':
    case '/models':
      if (arg) return false;
      openModels();
      return true;
  }
  if (TERMINAL_ONLY[name] && (!rest.length || ['/rewind', '/login', '/paste', '/browser'].includes(name))) {
    toast(TERMINAL_ONLY[name], 7000);
    return true;
  }
  return false;
}

// The slash menu: commands matching what is typed, while the first word is a command.
let slashItems = [];
let slashAt = 0;

function updateSlash() {
  const menu = $('slash');
  const value = input.value;
  const word = value.match(/^\/\S*$/)?.[0];
  slashItems = word ? COMMANDS.filter((c) => c.name.startsWith(word)) : [];
  if (word && !slashItems.length) slashItems = COMMANDS.filter((c) => c.name.includes(word.slice(1)));
  menu.hidden = !slashItems.length;
  if (menu.hidden) return;
  slashAt = Math.min(slashAt, slashItems.length - 1);
  menu.replaceChildren(
    ...slashItems.map((c, i) =>
      h(
        'button',
        {
          class: `slash-row${i === slashAt ? ' on' : ''}`,
          onmousedown: (ev) => {
            ev.preventDefault();
            completeSlash(i);
          },
          onmousemove: () => {
            if (slashAt !== i) {
              slashAt = i;
              updateSlash();
            }
          },
        },
        h('span', { class: 'slash-name', text: c.name }),
        c.args ? h('span', { class: 'slash-args', text: c.args }) : null,
        h('span', { class: 'slash-desc', text: c.desc }),
        c.local ? h('span', { class: 'slash-tag', text: 'page' }) : null,
      ),
    ),
  );
  menu.children[slashAt]?.scrollIntoView({ block: 'nearest' });
}

function completeSlash(i) {
  const c = slashItems[i];
  if (!c) return;
  if (c.args) setInput(`${c.name} `);
  else {
    setInput(c.name);
    submit();
  }
}

input.addEventListener('input', () => {
  autosize();
  updateSend();
  slashAt = 0;
  updateSlash();
});
input.addEventListener('blur', () => setTimeout(() => ($('slash').hidden = true), 100));
input.addEventListener('focus', updateSlash);

input.addEventListener('keydown', (ev) => {
  if (ev.isComposing) return;
  // With nothing typed, the keys answer the agent's questions.
  if (questionKey(ev)) return;
  if (!$('slash').hidden) {
    if (ev.key === 'ArrowDown' || ev.key === 'ArrowUp') {
      ev.preventDefault();
      slashAt = (slashAt + (ev.key === 'ArrowDown' ? 1 : -1) + slashItems.length) % slashItems.length;
      updateSlash();
      return;
    }
    if (ev.key === 'Tab' || (ev.key === 'Enter' && !ev.shiftKey)) {
      ev.preventDefault();
      // Enter on a command typed in full runs it; otherwise the choice is completed.
      if (ev.key === 'Enter' && slashItems[slashAt]?.name === input.value.trim()) submit();
      else completeSlash(slashAt);
      return;
    }
    if (ev.key === 'Escape') {
      ev.preventDefault();
      $('slash').hidden = true;
      return;
    }
  }
  // On a touch keyboard Return is the only way to start a new line; the button sends.
  if (ev.key === 'Enter' && !ev.shiftKey && !ev.altKey && !touch.matches) {
    ev.preventDefault();
    submit();
    return;
  }
  // Earlier prompts, from the first line up or the last line down.
  const before = input.value.slice(0, input.selectionStart);
  const after = input.value.slice(input.selectionEnd);
  if (ev.key === 'ArrowUp' && !before.includes('\n') && historyAt > 0) {
    if (historyAt === history.length) draft = input.value;
    ev.preventDefault();
    setInput(history[--historyAt]);
  } else if (ev.key === 'ArrowDown' && !after.includes('\n') && historyAt < history.length) {
    ev.preventDefault();
    historyAt++;
    setInput(historyAt === history.length ? draft : history[historyAt]);
  }
});

function bindChips(root = document) {
  for (const chip of root.querySelectorAll('.chip')) {
    chip.onclick = () => {
      if (chip.dataset.command) runLocal(chip.dataset.command);
      else if (chip.dataset.send) ask(chip.dataset.send);
      else setInput(chip.dataset.prompt);
    };
  }
}
bindChips();

/** Sends `text` as a prompt straight away, as if typed and sent. */
function ask(text) {
  setInput(text);
  submit();
}

/** Adds `text` to the prompt on a paragraph of its own, ready for a question. */
function appendToInput(text) {
  let value = input.value;
  if (value.trim() && !value.endsWith('\n\n')) value += value.endsWith('\n') ? '\n' : '\n\n';
  setInput(value + text);
  input.scrollTop = input.scrollHeight;
}

/** After the agent ran a tool: files may have changed. */
let refreshTimer;
function afterToolResult() {
  clearTimeout(refreshTimer);
  refreshTimer = setTimeout(() => {
    askChanges();
    reviewChanged();
  }, 400);
}

// ---------------------------------------------------------------- the code view

/**
 * pullspace (src/web/pullspace.rs) in a frame, on the working tree against HEAD. The frame
 * asks for snapshots and blobs over postMessage; this forwards them over the socket (sealed,
 * on an encrypted session) and posts the replies back, tells it when files may have changed,
 * and asks it to show the files the chat links to. Its files come from tamale like this
 * page's, through a hub too.
 *
 * It has no chrome here: pullspace's own bar is the pane's header, and the switch in the top
 * bar says which of the chat and the code is on screen.
 */
const review = {
  /** What's on screen: 'chat', 'split' (both) or 'code'. */
  view: 'chat',
  /** Which of the two the code was last shown in, for ⌘B and /review to come back to. */
  last: store.get('reviewFull', true) ? 'code' : 'split',
  /** How the page was left: a reload comes back to the chat and the code side by side. */
  resume: store.get('view', 'chat') === 'split',
  frame: null,
  /** The frame is listening for events; until then they wait in `queue`. */
  ready: false,
  queue: [],
  /** Socket seq → the frame's request id. */
  pending: new Map(),
  /** What the frame asked for while there was no connection to ask over. */
  held: [],
  /** Files may have changed while the code was out of sight. */
  stale: false,
};
const REVIEW_TIMEOUT = 60_000;
const VIEWS = { chat: $('view-chat'), split: $('view-split'), code: $('view-code') };

function setView(view, { focus = true } = {}) {
  if (view === 'split' && tight.matches) view = 'code';
  if (view !== 'chat' && state.status?.chef) view = 'chat';
  const stick = nearBottom();
  review.view = view;
  const open = view !== 'chat';
  store.set('view', view);
  if (open) {
    review.last = view;
    store.set('reviewFull', view === 'code');
    if (!review.frame) loadReview();
    else if (review.stale) {
      review.stale = false;
      reviewPost({ event: 'changed' });
    }
  }
  $('review').hidden = !open;
  $('divider').hidden = view !== 'split';
  $('main').classList.toggle('split', view === 'split');
  $('main').classList.toggle('code', view === 'code');
  for (const [name, button] of Object.entries(VIEWS)) {
    button.classList.toggle('on', name === view);
    button.setAttribute('aria-pressed', name === view);
  }
  // The chat is a different width now; stay at the end of it.
  if (stick && view !== 'code') scrollToBottom();
  if (!focus) return;
  if (view === 'code') review.frame.focus();
  else focusPrompt();
}

/** Brings the chat back beside the code (or in place of it, with no room for both): the agent is waiting on an answer there. */
function showChat() {
  if (review.view === 'code') setView(tight.matches ? 'chat' : 'split', { focus: false });
}

/** The code, away and back: ⌘B, /review. */
const toggleReview = () => setView(review.view === 'chat' ? review.last : 'chat');
for (const [name, button] of Object.entries(VIEWS)) button.onclick = () => setView(name);
tight.addEventListener('change', () => {
  if (tight.matches && review.view === 'split') setView('code', { focus: false });
});

function loadReview() {
  review.stale = false;
  review.frame = h('iframe', { class: 'review-frame', src: '/pullspace/?embed', title: 'Code, in pullspace' });
  $('review').append(review.frame);
  // Keys go to the frame while it has the focus; this one is the page's.
  review.frame.addEventListener('load', () => {
    try {
      review.frame.contentWindow.addEventListener('keydown', (ev) => {
        if ((ev.metaKey || ev.ctrlKey) && !ev.shiftKey && !ev.altKey && ev.key.toLowerCase() === 'b') {
          ev.preventDefault();
          toggleReview();
        }
      }, true);
    } catch {}
  });
  // Shown once it has something to show; a module that never arrives gets to say so.
  setTimeout(() => {
    if ($('review').classList.contains('shown')) return;
    $('review-wait-note').textContent = 'Still loading pullspace…';
    setTimeout(reviewShown, 20_000);
  }, 10_000);
}

function reviewShown() {
  $('review').classList.add('shown');
}

function reviewPost(message) {
  review.frame?.contentWindow?.postMessage({ ps: 1, ...message }, location.origin);
}

/** Posts an event to the frame, once it's listening. */
function reviewSend(message) {
  if (review.ready) reviewPost(message);
  else review.queue.push(message);
}

function reviewReady() {
  if (review.ready) return;
  review.ready = true;
  for (const message of review.queue.splice(0)) reviewPost(message);
}

/** After the agent changed files: the code view reads them again (when it's looking). */
function reviewChanged() {
  if (!review.frame) return;
  if (review.view !== 'chat') reviewSend({ event: 'changed' });
  else review.stale = true;
}

/** Shows a file in the code view, opening it if need be: its changes, or a line of it. */
function openInCode(path, { line = null, diff = false } = {}) {
  if (state.status?.chef) return;
  if (review.view === 'chat') setView(review.last, { focus: false });
  reviewSend({ event: 'open', body: { path, line: line ?? undefined, diff } });
}

function reviewAsk(id, t, body) {
  // Asked before the connection is up (a reload that comes back side by side), or while
  // it's down: it goes once there is one, rather than failing a read that only has to wait.
  if (!isReady()) return void review.held.push([id, t, body]);
  const s = ++seq;
  review.pending.set(s, id);
  if (!send({ t, seq: s, ...body }, true)) {
    review.pending.delete(s);
    reviewShown();
    return reviewPost({ id, error: 'Not connected to tamale.' });
  }
  setTimeout(() => {
    if (!review.pending.delete(s)) return;
    reviewShown();
    reviewPost({ id, error: 'tamale took too long to answer.' });
  }, REVIEW_TIMEOUT);
}

function reviewReply(m) {
  const id = review.pending.get(m.seq);
  if (id == null) return;
  review.pending.delete(m.seq);
  reviewPost(m.error ? { id, error: m.error } : { id, body: m.body });
  if (m.t !== 'ps_snapshot') return;
  // It has the workspace (or why not), so it has something to draw. A bundle from before
  // `ready` is listening by now too.
  setTimeout(reviewShown, 60);
  reviewReady();
}

/** What the reader asked about in the code (a line, a selection, or the file), quoted into the prompt. */
function reviewQuote(body) {
  const path = String(body?.path ?? '');
  if (!path) return;
  const line = body.line == null ? null : Number(body.line);
  if (line != null && !Number.isFinite(line)) return;
  const end = line != null && Number(body.end) > line ? Number(body.end) : null;
  const text = typeof body.text === 'string' ? body.text.replace(/\r/g, '').replace(/\n+$/, '').slice(0, 20_000) : '';
  const where = line == null ? '' : end ? ` (lines ${line}–${end})` : ` (line ${line})`;
  // A fence longer than any run of backticks in what it quotes.
  const fence = '`'.repeat(Math.max(3, ...(text.match(/`+/g) ?? []).map((run) => run.length + 1)));
  const quote = text.trim() ? `In \`${path}\`${where}:\n${fence}\n${text}\n${fence}\n\n` : `In \`${path}\`${where}:\n\n`;
  // The prompt has to be on screen to be typed into.
  showChat();
  appendToInput(quote);
}

addEventListener('message', (ev) => {
  if (!review.frame || ev.source !== review.frame.contentWindow || ev.origin !== location.origin) return;
  const m = ev.data;
  if (!m || typeof m !== 'object' || m.ps !== 1) return;
  if (m.t === 'ready') return reviewReady();
  if (m.t === 'quote') return reviewQuote(m.body);
  if (m.t === 'missing') return toast(`\`${String(m.body?.path ?? '').slice(0, 200)}\` isn't a file in this repository.`);
  if (typeof m.id !== 'number') return;
  if (m.t === 'snapshot') return reviewAsk(m.id, 'ps_snapshot', {});
  if (m.t === 'blobs') {
    const wants = (Array.isArray(m.body?.wants) ? m.body.wants : [])
      .slice(0, 1024)
      .filter((w) => typeof w?.sha === 'string' && typeof w?.path === 'string')
      .map(({ sha, path }) => ({ sha, path }));
    return reviewAsk(m.id, 'ps_blobs', { wants });
  }
  reviewPost({ id: m.id, error: `tamale doesn't answer "${m.t}"` });
});

// ---- how much has changed, on the switch's code button

function askChanges() {
  if (!state.status?.chef) request('diff_stat');
}

function applyChanges(m) {
  const files = Number(m.files) || 0;
  const count = $('changes-n');
  count.hidden = !files;
  count.textContent = files > 99 ? '99+' : String(files);
  VIEWS.code.title = files
    ? `Code · ${files} file${files === 1 ? '' : 's'} changed, +${m.added} −${m.removed}  ⌘B`
    : 'Code  ⌘B';
}

// ---- seams: the lines between panes, which drag to resize them, as in pullspace

/**
 * Makes `handle` a seam that resizes a pane. `at(ev)` turns the pointer into a size,
 * `apply(size)` shows one (`null` for the stylesheet's), `clamp` keeps it sensible,
 * `now()` measures the pane for the arrow keys, which move it by `step`. The size is kept
 * in localStorage under `key`; a double-click goes back to the stylesheet's.
 */
function seam(handle, { key, at, apply, clamp, now, step }) {
  let size = store.get(key, null);
  const set = (value, keep = true) => {
    size = value == null ? null : clamp(value);
    apply(size);
    if (keep) store.set(key, size);
  };
  if (typeof size === 'number') set(size, false);
  else size = null;
  handle.addEventListener('pointerdown', (ev) => {
    if (ev.button !== 0) return;
    ev.preventDefault();
    // Over everything while dragging: a frame would take the pointer as soon as it crossed
    // into it.
    const mask = h('div', { class: 'dragmask' });
    mask.style.cursor = getComputedStyle(handle).cursor;
    document.body.append(mask);
    handle.classList.add('on');
    const move = (e) => set(at(e), false);
    const done = () => {
      mask.remove();
      handle.classList.remove('on');
      removeEventListener('pointermove', move);
      removeEventListener('pointerup', done);
      removeEventListener('pointercancel', done);
      store.set(key, size);
    };
    addEventListener('pointermove', move);
    addEventListener('pointerup', done);
    addEventListener('pointercancel', done);
  });
  handle.addEventListener('dblclick', () => set(null));
  handle.addEventListener('keydown', (ev) => {
    if (ev.key !== 'ArrowLeft' && ev.key !== 'ArrowRight') return;
    ev.preventDefault();
    set((size ?? now()) + (ev.key === 'ArrowLeft' ? -step : step));
  });
  // A smaller window may no longer have room for what was kept.
  addEventListener('resize', () => { if (size != null && handle.offsetParent) set(size, false); });
  return { reset: () => set(null) };
}

/** Between the chat and the code: the chat's share of the width. */
seam($('divider'), {
  key: 'chatShare',
  at: (e) => {
    const box = $('main').getBoundingClientRect();
    return (e.clientX - box.left) / box.width;
  },
  apply: (share) => {
    if (share == null) $('main').style.removeProperty('--chat-w');
    else $('main').style.setProperty('--chat-w', `${(share * 100).toFixed(2)}%`);
  },
  clamp: (share) => {
    const width = $('main').getBoundingClientRect().width || 1;
    // Room for a conversation on one side and a file on the other.
    const least = 240 / width;
    const most = Math.max(least, 1 - 280 / width);
    return Math.min(most, Math.max(least, share));
  },
  now: () => $('scroller').getBoundingClientRect().width / ($('main').getBoundingClientRect().width || 1),
  step: 0.02,
});

/** Before a chef's kitchen: its width. */
seam($('k-divider'), {
  key: 'kitchenWidth',
  at: (e) => $('main').getBoundingClientRect().right - e.clientX,
  apply: (px) => {
    if (px == null) $('kitchen').style.removeProperty('--k-w');
    else $('kitchen').style.setProperty('--k-w', `${Math.round(px)}px`);
  },
  clamp: (px) => Math.max(220, Math.min(px, ($('main').getBoundingClientRect().width || Infinity) - 300)),
  now: () => $('kitchen').getBoundingClientRect().width,
  step: 24,
});

/** In the artifacts gallery, between the list and the artifact: the list's width. */
seam($('art-divider'), {
  key: 'artListWidth',
  at: (e) => e.clientX - $('art-list').getBoundingClientRect().left,
  apply: (px) => {
    if (px == null) $('art-box').style.removeProperty('--art-w');
    else $('art-box').style.setProperty('--art-w', `${Math.round(px)}px`);
  },
  // Hidden until the gallery opens, so with no width to go by until then.
  clamp: (px) => Math.max(160, Math.min(px, ($('art-box').getBoundingClientRect().width || Infinity) - 320)),
  now: () => $('art-list').getBoundingClientRect().width,
  step: 24,
});

for (const mask of document.querySelectorAll('.pickmask')) {
  mask.addEventListener('mousedown', (ev) => {
    if (ev.target === mask) mask.hidden = true;
  });
}

// ---------------------------------------------------------------- artifacts gallery

/** Saved artifacts by session, from the server, and which one is shown. */
const gallery = { sessions: [], rows: [], at: -1, viewing: false };

function ago(secs) {
  const d = Math.max(0, Date.now() / 1000 - secs);
  if (d < 60) return 'just now';
  if (d < 3600) return `${Math.floor(d / 60)}m ago`;
  if (d < 86400) return `${Math.floor(d / 3600)}h ago`;
  if (d < 86400 * 30) return `${Math.floor(d / 86400)}d ago`;
  return new Date(secs * 1000).toLocaleDateString();
}

function openArtifacts() {
  if (!$('artifacts').hidden) return;
  closeFullArtifact();
  $('artifacts').hidden = false;
  $('art-find').value = '';
  $('art-list').innerHTML = '<div class="pickempty">Loading…</div>';
  showArtifactPane(false);
  if (gallery.at < 0) showArtifact(null);
  request('artifacts');
  if (!touch.matches) $('art-find').focus();
  pushLayer(closeArtifacts);
}

function closeArtifacts() {
  if ($('artifacts').hidden) return;
  // Back from an artifact on a phone returns to the list first.
  if (gallery.viewing) {
    dropLayer(closeArtifactPane);
    gallery.viewing = false;
  }
  $('artifacts').hidden = true;
  $('art-view').innerHTML = '';
  gallery.at = -1;
  dropLayer(closeArtifacts);
  focusPrompt();
}

/**
 * On narrow screens the gallery shows the list or one artifact, not both; this switches
 * between them. Wide screens always show both.
 */
function showArtifactPane(on) {
  $('art-box').classList.toggle('viewing', on);
}
function closeArtifactPane() {
  if (!gallery.viewing) return;
  gallery.viewing = false;
  showArtifactPane(false);
  dropLayer(closeArtifactPane);
}

function applyArtifacts(sessions) {
  gallery.sessions = sessions;
  renderArtifactList();
}

function renderArtifactList() {
  const query = $('art-find').value.trim().toLowerCase();
  const list = $('art-list');
  const shown = gallery.rows[gallery.at]?.url;
  list.innerHTML = '';
  gallery.rows = [];
  gallery.at = -1;
  for (const session of gallery.sessions) {
    const items = session.items.filter((a) => !query || a.title.toLowerCase().includes(query));
    if (!items.length) continue;
    const name = session.current ? 'This session' : session.title || 'Untitled session';
    list.append(
      h('div', { class: 'art-group', title: name },
        h('span', { text: name }),
        h('span', { text: session.current ? '' : ago(session.updated) })),
    );
    for (const item of items) {
      const index = gallery.rows.length;
      gallery.rows.push({ ...item, current: session.current });
      list.append(
        h('button', { class: 'art-row', 'data-i': index, title: item.title, onclick: () => selectArtifact(index, true) },
          h('span', { html: ICON.spark }),
          h('span', { class: 't', text: item.title }),
          h('span', { class: 'ago', text: ago(item.modified) })),
      );
      if (item.url === shown) gallery.at = index;
    }
  }
  if (!gallery.rows.length) {
    list.innerHTML = `<div class="pickempty">${query ? 'No artifacts match.' : 'No saved artifacts in this project yet.'}</div>`;
    showArtifact(null);
    return;
  }
  // A phone shows the list first; loading the first artifact behind it would be wasted.
  if (gallery.at < 0 && !narrow.matches) selectArtifact(0);
  else markArtifact();
}

function markArtifact() {
  for (const row of $('art-list').querySelectorAll('.art-row')) {
    const on = Number(row.dataset.i) === gallery.at;
    row.classList.toggle('on', on);
    if (on) row.scrollIntoView({ block: 'nearest' });
  }
}

function selectArtifact(index, tapped = false) {
  if (index < 0 || index >= gallery.rows.length) return;
  if (index !== gallery.at) {
    gallery.at = index;
    showArtifact(gallery.rows[index]);
  }
  markArtifact();
  if (tapped && narrow.matches && !gallery.viewing) {
    gallery.viewing = true;
    showArtifactPane(true);
    pushLayer(closeArtifactPane);
  }
}

/** The chat's embed of an artifact, if it's in the conversation on screen. */
const inChat = (url) => [...thread.querySelectorAll('.artifact[data-src]')].find((b) => b.dataset.src === url);

function showArtifact(item) {
  const view = $('art-view');
  view.innerHTML = '';
  if (!item) {
    view.append(h('div', {
      class: 'art-empty',
      text: 'Artifacts from /web replies in this project show up here, including ones from earlier sessions.',
    }));
    return;
  }
  const embed = inChat(item.url);
  const btn = (title, icon, onclick, extra = '') =>
    h('button', { class: `artifact-btn ${extra}`, title, html: icon, onclick });
  view.append(
    h('div', { class: 'artifact-bar' },
      btn('Back to the list', ICON.back, closeArtifactPane, 'art-back'),
      h('span', { class: 'artifact-icon', html: ICON.spark }),
      h('span', { class: 'artifact-title', text: item.title, title: item.title }),
      h('span', { class: 'art-path', title: item.file, text: item.file }),
      h('span', { class: 'spacer' }),
      embed && btn('Show in the chat', ICON.chat, () => {
        closeArtifacts();
        embed.scrollIntoView({ block: 'start', behavior: 'smooth' });
      }),
      btn('Revise this artifact', ICON.edit, () => {
        closeArtifacts();
        setInput(item.current
          ? `Revise the "${item.title}" artifact: `
          : `Revise the "${item.title}" artifact from an earlier session (saved at ${item.file}; read it first): `);
      }),
      h('a', { class: 'artifact-btn wide-only', title: 'Download as HTML', href: item.url, download: `${slug(item.title)}.html`, onclick: onSaveLink(item.url, item.title), html: ICON.download }),
      h('a', { class: 'artifact-btn', title: 'Open in a new tab', href: item.url, target: '_blank', rel: 'noopener noreferrer', onclick: onOpenLink(item.url), html: ICON.open }),
    ),
    (() => {
      const frame = h('iframe', {
        title: item.title,
        sandbox: 'allow-scripts allow-popups allow-modals allow-forms',
        referrerpolicy: 'no-referrer',
      });
      showInFrame(frame, item.url);
      return frame;
    })(),
  );
}

$('art-find').addEventListener('input', renderArtifactList);
$('art-find').addEventListener('keydown', (ev) => {
  if (ev.key === 'ArrowDown' || ev.key === 'ArrowUp') {
    ev.preventDefault();
    selectArtifact(gallery.at + (ev.key === 'ArrowDown' ? 1 : -1));
  } else if (ev.key === 'Enter') {
    ev.preventDefault();
    const item = gallery.rows[gallery.at];
    if (item) openArtifactTab(item.url);
  }
});
$('art-close').onclick = closeArtifacts;
$('toggle-artifacts').onclick = () => ($('artifacts').hidden ? openArtifacts() : closeArtifacts());

// ---------------------------------------------------------------- model picker

/**
 * The models the session's backend offers (the list `/model` shows in the terminal), which
 * is in use, and the rows the filter leaves. Picking one sends `/model <id>`.
 */
const picker = { open: false, models: [], current: '', rows: [], at: 0 };

function openModels() {
  if (picker.open) return;
  picker.open = true;
  $('models').hidden = false;
  $('model-find').value = '';
  $('model-note').hidden = true;
  $('model-list').innerHTML = '<div class="pickempty">Loading models…</div>';
  picker.models = [];
  request('models');
  // After whatever opened it is done: sending `/model` from the prompt clears and focuses it.
  if (!touch.matches) queueMicrotask(() => picker.open && $('model-find').focus());
  pushLayer(closeModels);
}

function closeModels() {
  if (!picker.open) return;
  picker.open = false;
  $('models').hidden = true;
  dropLayer(closeModels);
  focusPrompt();
}

function applyModels(m) {
  picker.models = m.models || [];
  picker.current = m.current || '';
  const note = $('model-note');
  note.hidden = !m.error;
  note.textContent = m.error ? `Couldn't fetch the provider's list (${m.error}); these are the built-in ones. /model <id> takes any id.` : '';
  renderModels(true);
}

/** Each word of the filter, somewhere in the model's id or name. */
function modelMatches(model, words) {
  const hay = `${model.id} ${model.name}`.toLowerCase();
  return words.every((w) => hay.includes(w));
}

function renderModels(toCurrent = false) {
  const query = $('model-find').value.trim();
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  picker.rows = picker.models.filter((m) => modelMatches(m, words));
  // An id that isn't listed can still be used, as `/model <id>` would.
  if (query && !/\s/.test(query) && !picker.rows.length && picker.models.length) {
    picker.rows.push({ id: query, name: `Use “${query}”`, typed: true });
  }
  const current = picker.rows.findIndex((m) => m.id === picker.current);
  picker.at = toCurrent && current >= 0 ? current : 0;
  const list = $('model-list');
  list.innerHTML = '';
  if (!picker.rows.length) {
    list.append(h('div', { class: 'pickempty', text: picker.models.length ? 'No models match.' : 'No models.' }));
    return;
  }
  const listed = picker.rows.filter((m) => !m.typed).length;
  if (picker.models.length > 1) {
    list.append(h('div', { class: 'model-count', text: words.length ? `${listed} of ${picker.models.length} models` : `${picker.models.length} models` }));
  }
  picker.rows.forEach((m, i) => {
    const on = m.id === picker.current;
    list.append(
      h('button', {
        class: 'model-row',
        role: 'option',
        id: `model-${i}`,
        'aria-selected': i === picker.at ? 'true' : 'false',
        title: m.id,
        onclick: () => chooseModel(i),
      },
        h('span', { class: 'tick', html: on ? ICON.check : '' }),
        h('span', { class: 'names' },
          h('span', { class: 'n', text: m.name || m.id }),
          m.name && m.name !== m.id && !m.typed ? h('span', { class: 'id', text: m.id }) : null)),
    );
  });
  selectModel(picker.at);
}

function selectModel(i) {
  if (!picker.rows.length) return;
  picker.at = Math.max(0, Math.min(i, picker.rows.length - 1));
  for (const row of $('model-list').querySelectorAll('.model-row')) {
    const on = row.id === `model-${picker.at}`;
    row.classList.toggle('on', on);
    row.setAttribute('aria-selected', on ? 'true' : 'false');
    if (on) row.scrollIntoView({ block: 'nearest' });
  }
  $('model-find').setAttribute('aria-activedescendant', `model-${picker.at}`);
}

function chooseModel(i) {
  const model = picker.rows[i];
  if (!model) return;
  closeModels();
  if (model.id !== picker.current) send({ t: 'send', text: `/model ${model.id}` });
}

$('model-find').addEventListener('input', () => renderModels());
$('model-find').addEventListener('keydown', (ev) => {
  if (ev.key === 'ArrowDown' || ev.key === 'ArrowUp') {
    ev.preventDefault();
    selectModel(picker.at + (ev.key === 'ArrowDown' ? 1 : -1));
  } else if (ev.key === 'PageDown' || ev.key === 'PageUp') {
    ev.preventDefault();
    selectModel(picker.at + (ev.key === 'PageDown' ? 8 : -8));
  } else if (ev.key === 'Enter') {
    ev.preventDefault();
    chooseModel(picker.at);
  }
});
$('model-close').onclick = closeModels;
$('models').addEventListener('mousedown', (ev) => {
  if (ev.target === $('models')) closeModels();
});

// ---------------------------------------------------------------- help

const KEYS = [
  ['⏎ / ⇧⏎', 'Send / new line (while the agent runs, ⏎ queues for its next step)'],
  ['esc', 'Interrupt the agent; close a menu'],
  ['↑ / ↓', 'Earlier prompts, from the first or last line'],
  ['⌘B / Ctrl+B', 'Show or hide the code'],
  ['⌘⏎ / Ctrl+⏎', 'In the code: ask about the selection, the picked line or the file'],
  ['y / a / n', 'Answer a tool approval (when not typing)'],
  ['1–9 / ⏎ / ← →', "Pick an answer to the agent's questions, move on, switch questions (when not typing)"],
  ['/', 'Start a command from anywhere'],
];

function openHelp() {
  const row = (k, v) => `<div class="helprow"><code>${esc(k)}</code><span>${esc(v)}</span></div>`;
  $('help-body').innerHTML =
    '<h2>tamale on the web</h2><p>The same session as the terminal, chat first. The switch in the top bar shows the code beside the chat or instead of it: pullspace, on your uncommitted changes and the whole repository. Steps that touch a file, and file paths in replies, open there.</p>' +
    '<h3>Commands</h3>' +
    COMMANDS.map((c) => row(`${c.name}${c.args ? ' ' + c.args : ''}`, c.desc)).join('') +
    '<h3>Keys</h3>' +
    KEYS.map(([k, v]) => row(k, v)).join('');
  $('help').hidden = false;
}

// ---------------------------------------------------------------- keys

addEventListener('keydown', (ev) => {
  // Already handled by the field it was typed in (the slash menu, the reason box…).
  if (ev.defaultPrevented) return;
  const mod = ev.metaKey || ev.ctrlKey;
  if (mod && ev.key.toLowerCase() === 'b') {
    ev.preventDefault();
    return toggleReview();
  }
  if (ev.key === 'Escape') {
    if (!$('ending').hidden) return closeEndDialog();
    if (!$('help').hidden) return ($('help').hidden = true);
    if (!$('artifacts').hidden) return closeArtifacts();
    if (picker.open) return closeModels();
    if (state.status?.running) {
      ev.preventDefault();
      return send({ t: 'interrupt' });
    }
    return;
  }
  if (approvalKey(ev)) return;
  if (questionKey(ev)) return;
  // Typing anywhere that isn't a field goes to the prompt.
  const t = ev.target;
  if (t.matches?.('input, textarea') || mod || ev.altKey || ev.key.length !== 1) return;
  if (!$('help').hidden || !$('artifacts').hidden || !$('ending').hidden || picker.open) return;
  input.focus();
});

// ---------------------------------------------------------------- start

window.visualViewport?.addEventListener('resize', fitViewport);
window.visualViewport?.addEventListener('scroll', fitViewport);
fitViewport();
// Side by side is a way of working, so it's how the page comes back; the code alone is
// somewhere you went, and the chat is where a reload starts.
setView(review.resume && !tight.matches ? 'split' : 'chat', { focus: false });
input.placeholder = promptPlaceholder();
autosize();
updateSend();
if (!e2e) connect();
else if (!e2e.key) lock('This session is encrypted', NEEDS_KEY);
else if (!(await supported())) {
  lock('This browser is too old for encrypted sessions', 'Open this session in a current version of Chrome, Firefox or Safari.');
} else connect();
