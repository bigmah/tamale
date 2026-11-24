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
  wave: '<svg viewBox="0 0 16 16"><path d="M2.5 6.5v3M5 4.5v7M7.75 2.5v11M10.5 5v6M13.25 6.75v2.5"/></svg>',
  mic: '<svg viewBox="0 0 16 16"><rect x="5.75" y="1.75" width="4.5" height="8" rx="2.25"/><path d="M3.25 7.75a4.75 4.75 0 0 0 9.5 0M8 12.5v2"/></svg>',
  micOff: '<svg viewBox="0 0 16 16"><path d="M10.25 6.5V4a2.25 2.25 0 0 0-4.4-.67M5.75 6.75v1a2.25 2.25 0 0 0 3.6 1.8M3.25 7.75a4.75 4.75 0 0 0 7.9 3.55M12.75 7.75c0 .5-.08 1-.22 1.45M8 12.5v2M2.5 2.5l11 11"/></svg>',
  gear: '<svg viewBox="0 0 16 16"><circle cx="8" cy="8" r="2.25"/><path d="M8 1.75v1.5M8 12.75v1.5M14.25 8h-1.5M3.25 8h-1.5M12.42 3.58l-1.06 1.06M4.64 11.36l-1.06 1.06M12.42 12.42l-1.06-1.06M4.64 4.64 3.58 3.58"/></svg>',
  hangup: '<svg viewBox="0 0 16 16"><path d="M1.75 9.25c3.5-3 9-3 12.5 0l-1.5 2-2.5-1v-1.75a7 7 0 0 0-4.5 0v1.75l-2.5 1Z"/></svg>',
  play: '<svg viewBox="0 0 16 16"><path d="M5 3.25v9.5L12.75 8Z"/></svg>',
  up: '<svg viewBox="0 0 16 16"><path d="M8 13V3M3.5 7.5 8 3l4.5 4.5"/></svg>',
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

/** Replies waited for, by the request's `seq`. */
const awaited = new Map();

/** Sends a request and resolves with its reply; rejects if not connected or not answered in time. */
function requestReply(t, body = {}, ms = 20000) {
  const s = ++seq;
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      awaited.delete(s);
      reject(new Error("tamale didn't answer in time."));
    }, ms);
    awaited.set(s, (m) => {
      clearTimeout(timer);
      resolve(m);
    });
    if (!send({ t, seq: s, ...body }, true)) {
      clearTimeout(timer);
      awaited.delete(s);
      reject(new Error("The session isn't connected right now."));
    }
  });
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
  // The reply to a request something is waiting on (see `requestReply`).
  if (m.seq != null && awaited.has(m.seq)) {
    const done = awaited.get(m.seq);
    awaited.delete(m.seq);
    return done(m);
  }
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
    mountVideos(node);
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
    mountVideos(block);
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
        title: 'Talk about this artifact with Gemini',
        html: ICON.wave,
        onclick: () => {
          closeFullArtifact();
          startVoice({ artifact: { title: box.dataset.title, url: src } });
        },
      }),
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
  if (wasRunning && !s.running) {
    afterToolResult();
    voiceTurnEnded();
  }
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
    if (had) voiceAnswered('approval');
    return;
  }
  state.approvalAt = Date.now();
  if (!had) {
    showChat();
    voiceApproval(a);
  }
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
    if (had) voiceAnswered('question');
    return;
  }
  voiceQuestions(q);
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
  $('make-video').hidden = !empty;
  $('composer').classList.toggle('empty', empty);
}

/** The iOS app's artifact button sends the same words. */
const ARTIFACT_PROMPT = 'Can you generate an artifact for this?';

/** What the video button and `/video` send: the backend knows how from the artifact guide. */
const VIDEO_PROMPT = 'Can you make an animated video of this with Manim?';
const videoAbout = (what) => `Make an animated video with Manim of: ${what}`;

$('make-video').onclick = () => {
  if (send({ t: 'send', text: VIDEO_PROMPT })) scrollToBottom(true);
};

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
  { name: '/video', args: '[what]', desc: 'Animate something with Manim and play it here; alone, what you were just discussing', local: true },
  { name: '/voice', desc: 'Talk to tamale by voice through Gemini (needs a Gemini API key)', local: true },
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
    case '/video':
    case '/manim':
      if (send({ t: 'send', text: arg ? videoAbout(arg) : VIDEO_PROMPT })) scrollToBottom(true);
      return true;
    case '/voice':
    case '/gemini':
      startVoice();
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

// ---------------------------------------------------------------- Gemini
//
// Talking to the session through Gemini's Live API, as the iOS app does (apple/Tamale/Voice):
// the page streams the microphone to Gemini as 16 kHz PCM, plays its 24 kHz answers, and
// shows what both sides said. Gemini drives the session through functions that act on this
// page's socket, the way the composer and the cards do: send a prompt, read the conversation
// and the changes, interrupt, and answer approvals and questions. Turns take minutes, so a prompt returns at once, and the page tells Gemini when the
// turn ends or tamale asks something. On an artifact's bar, the same conversation starts
// with the artifact's text in Gemini's instructions.
//
// The key and the chosen model and voice are kept by tamale (see src/web/gemini.rs): each
// session's page is an origin of its own, so the browser couldn't keep them. Without a
// microphone (a page on a LAN address isn't a secure context) it still works typed.

const GEMINI_HOST = 'generativelanguage.googleapis.com';
const GEMINI_VOICES = ['Puck', 'Charon', 'Kore', 'Fenrir', 'Aoede', 'Leda', 'Orus', 'Zephyr'];
const GEMINI_KEY_URL = 'https://aistudio.google.com/apikey';

/** The models each key can hold a live conversation with, the best guess for voice first. */
const liveModelLists = new Map();
function liveModels(key) {
  if (!liveModelLists.has(key)) {
    const load = fetchLiveModels(key);
    load.catch(() => liveModelLists.delete(key));
    liveModelLists.set(key, load);
  }
  return liveModelLists.get(key);
}

async function fetchLiveModels(key) {
  const version = (name) => parseFloat(/\d+(\.\d+)?/.exec(name)?.[0] ?? '0');
  // Within a version, the plain conversational model first: thinking ones answer slower.
  const penalty = (name) => (name.includes('thinking') ? 2 : 0) + (/preview|exp/.test(name) ? 1 : 0);
  const names = [];
  let token = '';
  do {
    const url = `https://${GEMINI_HOST}/v1beta/models?pageSize=1000${token ? `&pageToken=${encodeURIComponent(token)}` : ''}`;
    const response = await fetch(url, { headers: { 'x-goog-api-key': key } });
    const body = await response.json().catch(() => ({}));
    if (!response.ok) {
      const why = body.error?.message || response.statusText;
      throw new Error(response.status === 400 || response.status === 403 ? `Gemini refused the key: ${why}` : `Gemini answered ${response.status}: ${why}`);
    }
    for (const m of body.models ?? []) {
      if (!m.supportedGenerationMethods?.includes('bidiGenerateContent')) continue;
      const name = m.name.replace(/^models\//, '');
      // Gemini's own conversational models: the list also has world models (genie-…) and
      // ones for transcribing, translating or robots, which can't hold a conversation.
      if (name.startsWith('gemini') && !['transcribe', 'translate', 'robotics'].some((w) => name.includes(w))) names.push(name);
    }
    token = body.nextPageToken || '';
  } while (token);
  return names.sort((a, b) => version(b) - version(a) || penalty(a) - penalty(b) || a.localeCompare(b));
}

/** 16-bit PCM at `to` Hz from float samples at `from`, averaging each output sample's span. */
function downsample(input, from, to) {
  const ratio = from / to;
  const out = new Int16Array(Math.floor(input.length / ratio));
  for (let i = 0; i < out.length; i++) {
    const start = Math.floor(i * ratio);
    const end = Math.max(start + 1, Math.min(input.length, Math.floor((i + 1) * ratio)));
    let sum = 0;
    for (let j = start; j < end; j++) sum += input[j];
    const v = Math.max(-1, Math.min(1, sum / (end - start)));
    out[i] = v < 0 ? v * 0x8000 : v * 0x7fff;
  }
  return out;
}

/** One spoken conversation with Gemini: the socket, the audio both ways and the transcript. */
class GeminiLive {
  constructor(onChange) {
    this.onChange = onChange;
    /** idle, connecting, listening, speaking or ended. */
    this.phase = 'idle';
    /** Why it ended, if not by the user. */
    this.reason = '';
    /** `{ who: 'you' | 'gemini' | 'action', text, closed }`. */
    this.lines = [];
    this.model = '';
    /** The microphone's loudness, 0…1. */
    this.level = 0;
    /** Set when Google says the connection will close soon. */
    this.notice = '';
    /** Why there's no microphone, when there isn't one: it's typed, then. */
    this.micOff = '';
    this.muted = false;
    /** Runs a function Gemini called, by name with its arguments, and returns its result. */
    this.onToolCall = null;
    this.socket = null;
    this.ready = false;
    /** Events waiting for Gemini to finish speaking. */
    this.pending = [];
    this.ctx = null;
    this.stream = null;
    this.proc = null;
    this.sources = new Set();
    this.playAt = 0;
  }

  get active() {
    return ['connecting', 'listening', 'speaking'].includes(this.phase);
  }

  changed() {
    this.onChange?.();
  }

  /** `ctx` was made in the click that started this: browsers only play sound after one. */
  async start({ ctx, key, model, voice, instruction, tools = [] }) {
    if (this.active) return;
    Object.assign(this, { phase: 'connecting', reason: '', lines: [], notice: '', micOff: '', muted: false, pending: [], ready: false, ctx });
    this.changed();
    try {
      await this.startMic();
      // Chosen automatically, a model Google turns away at setup gives way to the next.
      const models = model ? [model] : (await liveModels(key)).slice(0, 3);
      if (!models.length) throw new Error("This key can't use any Gemini Live model.");
      for (const [i, name] of models.entries()) {
        if (this.phase !== 'connecting') return;
        this.model = name;
        this.changed();
        try {
          return await this.connect(key, voice, instruction, tools);
        } catch (e) {
          if (i === models.length - 1) throw e;
        }
      }
    } catch (e) {
      this.end(e.message || String(e));
    }
  }

  stop() {
    this.end('');
  }

  async startMic() {
    if (!navigator.mediaDevices?.getUserMedia) {
      this.micOff = isSecureContext
        ? "This browser doesn't give pages a microphone. Type instead."
        : "Browsers only give the microphone to secure pages: open this one through the hub, or at this machine's own address (127.0.0.1). Type instead.";
      return;
    }
    try {
      this.stream = await navigator.mediaDevices.getUserMedia({
        audio: { channelCount: 1, echoCancellation: true, noiseSuppression: true, autoGainControl: true },
      });
    } catch (e) {
      this.micOff = e.name === 'NotAllowedError'
        ? "The microphone isn't allowed here; allow it in the browser's site settings, or type instead."
        : `No microphone (${e.message}). Type instead.`;
      return;
    }
    if (this.phase !== 'connecting') return this.stream.getTracks().forEach((t) => t.stop());
    const source = this.ctx.createMediaStreamSource(this.stream);
    // A script processor rather than a worklet: a worklet's module would be a file of its own,
    // which a hub doesn't pass through. 4096 frames is under 100 ms.
    const proc = this.ctx.createScriptProcessor(4096, 1, 1);
    const silent = this.ctx.createGain();
    silent.gain.value = 0;
    source.connect(proc);
    proc.connect(silent);
    silent.connect(this.ctx.destination);
    proc.onaudioprocess = (ev) => this.onMic(ev.inputBuffer.getChannelData(0));
    this.proc = proc;
  }

  onMic(samples) {
    let sum = 0;
    for (let i = 0; i < samples.length; i++) sum += samples[i] * samples[i];
    this.level = this.muted ? 0 : Math.min(1, Math.sqrt(sum / samples.length) * 8);
    if (!this.ready || this.muted) return;
    const pcm = downsample(samples, this.ctx.sampleRate, 16000);
    this.sendJSON({ realtimeInput: { audio: { mimeType: 'audio/pcm;rate=16000', data: toBase64(new Uint8Array(pcm.buffer)) } } });
  }

  setMuted(muted) {
    this.muted = muted;
    if (muted) this.sendJSON({ realtimeInput: { audioStreamEnd: true } });
    this.changed();
  }

  /** Resolves once Gemini has taken the setup; rejects if it closes before that. */
  connect(key, voice, instruction, tools) {
    let settled = null;
    const setUp = new Promise((resolve, reject) => (settled = { resolve, reject }));
    this.setUp = settled;
    const socket = new WebSocket(`wss://${GEMINI_HOST}/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent?key=${encodeURIComponent(key)}`);
    socket.binaryType = 'arraybuffer';
    this.socket = socket;
    socket.onopen = () => {
      const generationConfig = { responseModalities: ['AUDIO'] };
      if (voice) generationConfig.speechConfig = { voiceConfig: { prebuiltVoiceConfig: { voiceName: voice } } };
      const setup = {
        model: `models/${this.model}`,
        generationConfig,
        systemInstruction: { parts: [{ text: instruction }] },
        inputAudioTranscription: {},
        outputAudioTranscription: {},
        // Past the Live API's usual 15 minutes of audio, the oldest turns are dropped.
        contextWindowCompression: { slidingWindow: {} },
      };
      if (tools.length) setup.tools = [{ functionDeclarations: tools }];
      this.sendJSON({ setup });
    };
    socket.onmessage = (ev) => {
      if (socket !== this.socket) return;
      try {
        this.handle(JSON.parse(typeof ev.data === 'string' ? ev.data : new TextDecoder().decode(ev.data)));
      } catch (e) {
        console.error(e);
      }
    };
    socket.onclose = (ev) => {
      if (socket !== this.socket) return;
      if (!this.ready) {
        this.socket = null;
        settled.reject(new Error(ev.reason
          ? `Gemini refused ${this.model}: ${ev.reason}`
          : "Couldn't reach Gemini. Check the key, the model and the network."));
        return;
      }
      this.end(ev.reason ? `Gemini ended the conversation: ${ev.reason}` : 'Gemini ended the conversation.');
    };
    return setUp;
  }

  handle(m) {
    if (m.setupComplete) {
      this.ready = true;
      this.setUp?.resolve();
      this.phase = 'listening';
      this.changed();
      this.flush();
    }
    if (m.toolCall?.functionCalls) this.runTools(m.toolCall.functionCalls);
    if (m.goAway) {
      this.notice = `Gemini will end this conversation soon${m.goAway.timeLeft ? ` (${m.goAway.timeLeft})` : ''}.`;
      this.changed();
    }
    const content = m.serverContent;
    if (!content) return;
    if (content.inputTranscription?.text) this.append('you', content.inputTranscription.text);
    if (content.outputTranscription?.text) {
      this.close('you');
      this.append('gemini', content.outputTranscription.text);
    }
    if (content.interrupted) {
      this.interruptPlayback();
      this.close('gemini');
    }
    for (const part of content.modelTurn?.parts ?? []) {
      const data = part.inlineData;
      if (data?.data && data.mimeType?.startsWith('audio/')) {
        this.play(data.data, Number(/rate=(\d+)/.exec(data.mimeType)?.[1]) || 24000);
      }
    }
    if (content.turnComplete) {
      this.close('you');
      this.close('gemini');
      if (!this.sources.size) this.flush();
    }
    this.changed();
  }

  async runTools(calls) {
    const functionResponses = [];
    for (const call of calls) {
      let response;
      try {
        response = (await this.onToolCall?.(call.name, call.args ?? {})) ?? { error: 'No such function.' };
      } catch (e) {
        response = { error: e.message || String(e) };
      }
      functionResponses.push({ id: call.id, name: call.name, response });
    }
    this.sendJSON({ toolResponse: { functionResponses } });
    this.changed();
  }

  /**
   * Tells Gemini something happened that it should act on or tell the user about, once it
   * has finished what it's saying. `shown` goes in the transcript.
   */
  inform(text, shown) {
    if (!this.socket) return;
    this.note(shown);
    this.pending.push(text);
    this.flush();
  }

  flush() {
    if (this.phase !== 'listening' || !this.pending.length) return;
    const text = this.pending.splice(0).join('\n\n');
    this.sendJSON({ realtimeInput: { text: `[A message from the app, not the user]\n${text}` } });
  }

  /** Typed instead of spoken. */
  sendText(text) {
    text = text.trim();
    if (!text || !this.ready) return false;
    this.interruptPlayback();
    this.append('you', text);
    this.close('you');
    this.sendJSON({ realtimeInput: { text } });
    this.changed();
    return true;
  }

  /** A line in the transcript for something the page did. */
  note(text) {
    this.lines.push({ who: 'action', text, closed: true });
    this.changed();
  }

  append(who, text) {
    if (!text) return;
    const last = this.lines[this.lines.length - 1];
    if (last && last.who === who && !last.closed) last.text += text;
    else this.lines.push({ who, text: text.trimStart(), closed: false });
  }

  close(who) {
    for (let i = this.lines.length - 1; i >= 0; i--) {
      if (this.lines[i].who === who) {
        this.lines[i].closed = true;
        return;
      }
    }
  }

  play(b64, rate) {
    if (!this.ctx) return;
    const bytes = fromBase64(b64);
    const frames = bytes.length >> 1;
    if (!frames) return;
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    const buffer = this.ctx.createBuffer(1, frames, rate);
    const out = buffer.getChannelData(0);
    for (let i = 0; i < frames; i++) out[i] = view.getInt16(i * 2, true) / 32768;
    const source = this.ctx.createBufferSource();
    source.buffer = buffer;
    source.connect(this.ctx.destination);
    const at = Math.max(this.ctx.currentTime + 0.02, this.playAt);
    source.start(at);
    this.playAt = at + buffer.duration;
    this.sources.add(source);
    source.onended = () => {
      this.sources.delete(source);
      if (!this.sources.size && this.phase === 'speaking') {
        this.phase = 'listening';
        this.changed();
        this.flush();
      }
    };
    if (this.phase === 'listening') {
      this.phase = 'speaking';
      this.changed();
    }
  }

  /** Whatever Gemini was saying stops now: the user spoke over it. */
  interruptPlayback() {
    for (const source of this.sources) {
      source.onended = null;
      try {
        source.stop();
      } catch {}
    }
    this.sources.clear();
    this.playAt = 0;
    if (this.phase === 'speaking') this.phase = 'listening';
  }

  sendJSON(object) {
    if (this.socket?.readyState === WebSocket.OPEN) this.socket.send(JSON.stringify(object));
  }

  end(reason) {
    if (this.phase === 'ended') return;
    const socket = this.socket;
    this.socket = null;
    this.ready = false;
    this.setUp?.reject(new Error('Stopped.'));
    this.setUp = null;
    try {
      socket?.close();
    } catch {}
    this.interruptPlayback();
    this.stream?.getTracks().forEach((t) => t.stop());
    this.stream = null;
    if (this.proc) this.proc.onaudioprocess = null;
    this.proc?.disconnect();
    this.proc = null;
    this.ctx?.close().catch(() => {});
    this.ctx = null;
    this.level = 0;
    this.reason = reason;
    this.phase = 'ended';
    this.changed();
  }
}

// ---- the session, as Gemini is told it and acts on it

/** The voice card's state: the conversation, what it's about, and which view shows. */
const voice = {
  live: null,
  /** `{ title, url }` when it started on an artifact. */
  artifact: null,
  settings: null,
  /** 'talk' or 'settings'. */
  view: 'talk',
  /** The transcript folded away, to give the chat the room. */
  folded: false,
  /** What Gemini answered itself, so the page doesn't tell it the user did. */
  answering: { approval: false, question: false },
  note: '',
};

const VOICE_TOOLS = [
  {
    name: 'send_prompt',
    description: 'Sends a message to tamale, the coding agent, as if the user typed it. Returns at once; tamale works on it and the app reports when the turn ends. If tamale is busy, the message is queued.',
    parameters: { type: 'OBJECT', properties: { text: { type: 'STRING', description: 'The prompt, written as an instruction or question to the coding agent.' } }, required: ['text'] },
  },
  { name: 'get_status', description: "Whether tamale is working, for how long, its model, project and directory, queued prompts, and anything it's waiting on." },
  {
    name: 'read_conversation',
    description: "The last entries of the session's conversation: the user's prompts, tamale's replies, tool calls with their output, and errors.",
    parameters: { type: 'OBJECT', properties: { last: { type: 'INTEGER', description: 'How many entries, from the end. Default 8, at most 40.' } } },
  },
  { name: 'get_changes', description: "The uncommitted changes in tamale's working directory (git diff): files, lines added and removed, and the changed lines." },
  { name: 'interrupt', description: 'Stops what tamale is doing now.' },
  {
    name: 'answer_permission',
    description: "Answers a permission request tamale is waiting on, which the app will have told you about. Only with the user's explicit yes or no to that request. Not for asking tamale to do something: that's send_prompt.",
    parameters: {
      type: 'OBJECT',
      properties: {
        allow: { type: 'BOOLEAN', description: 'true if the user said yes.' },
        reason: { type: 'STRING', description: 'When refusing, what the user said to do instead, if anything.' },
      },
      required: ['allow'],
    },
  },
  {
    name: 'answer_questions',
    description: "Answers tamale's waiting questions, one answer per question in order.",
    parameters: {
      type: 'OBJECT',
      properties: {
        answers: {
          type: 'ARRAY',
          items: {
            type: 'OBJECT',
            properties: {
              chosen: { type: 'ARRAY', items: { type: 'STRING' }, description: 'Labels of the options the user picked.' },
              other: { type: 'STRING', description: 'Anything the user said instead of, or as well as, an option.' },
            },
          },
        },
      },
      required: ['answers'],
    },
  },
];

const cut = (s, n) => (s.length > n ? `${s.slice(0, n)} …(cut)` : s);

/** The text of a reply as Gemini reads it: artifacts and videos as one line each. */
function replyText(e) {
  let md = e.md;
  if (!md) md = h('template', { html: e.html || '' }).content.textContent;
  return md
    .replace(/<!-- artifact (\d+) -->/g, (_, n) => `[artifact: ${e.artifacts?.[n]?.title || 'page'}]`)
    .replace(/```video\n([\s\S]*?)```/g, (_, paths) => `[video: ${paths.trim().split(/[\\/]/).pop()}]`);
}

function entryText(e, room) {
  switch (e.kind) {
    case 'user':
      return `User: ${cut(e.text || '', room)}`;
    case 'assistant':
    case 'note':
      return `${e.kind === 'note' ? 'Note' : 'tamale'}: ${cut(replyText(e), room)}`;
    case 'tool': {
      const what = `${e.name || 'tool'} ${e.summary || e.description || ''}`;
      if (e.output == null) return `Tool ${what}: running`;
      return `Tool ${what}${e.error ? ' FAILED' : ''}: ${cut(e.output, Math.min(room, e.error ? 1500 : 500))}`;
    }
    case 'info':
      return `Info: ${cut(e.text || '', room)}`;
    case 'error':
      return `Error: ${cut(e.text || '', room)}`;
    default:
      return '';
  }
}

function conversationText(last) {
  const entries = state.entries.slice(-last);
  return entries.length ? entries.map((e) => entryText(e, 1500)).filter(Boolean).join('\n') : '(nothing yet)';
}

function connectionText() {
  if (locked) return 'unreachable';
  return isReady() ? 'connected' : 'reconnecting';
}

function statusLine() {
  const s = state.status;
  if (!s) return `Session ${connectionText()}; no status yet.`;
  let line = s.running ? 'tamale is working' : 'tamale is idle';
  const now = Math.floor(Date.now() / 1000);
  if (s.running && s.since) {
    line += ` (for ${fmtElapsed(now - s.since)})`;
    if (s.step) line += `; now: ${s.step}, for ${fmtElapsed(now - (s.step_since || s.since))}${s.steps ? `, ${s.steps} steps so far` : ''}`;
  }
  return `${line}. Project ${s.project}, model ${s.model}. Session ${connectionText()}.`;
}

function describeApproval(a) {
  let s = `${a.name}: ${a.summary}`;
  if (a.description && a.description !== a.summary) s += `\n${a.description.slice(0, 1500)}`;
  return s;
}

function describeQuestions(q) {
  return q.questions
    .map((item, i) => {
      const options = item.options.map((o) => (o.description ? `${o.label} (${o.description})` : o.label));
      return `${i + 1}. ${item.question}${item.multi_select ? ' (pick any)' : ''} Options: ${options.join('; ')}`;
    })
    .join('\n');
}

function voiceStatus() {
  const s = state.status;
  const out = { connection: connectionText(), summary: statusLine() };
  if (s) {
    Object.assign(out, {
      busy: s.running,
      compacting: s.compacting,
      project: s.project,
      directory: s.cwd,
      model: s.model,
      mode: s.mode,
      queued_prompts: s.queued,
    });
    if (s.goal) out.goal = s.goal;
    if (s.goal_state) out.goal_state = s.goal_state;
    if (s.window) out.context_used_percent = Math.round((s.context * 100) / s.window);
  }
  if (state.approval) out.waiting_for_permission = describeApproval(state.approval);
  if (state.question) out.waiting_for_answers = describeQuestions(state.question);
  const running = state.entries.findLast((e) => e.kind === 'tool' && e.output == null);
  if (running) out.running_now = `${running.name}: ${running.summary || running.description || ''}`;
  return out;
}

/** What happened since the last prompt: replies, errors, and how the tools went. */
function lastTurnText() {
  const entries = state.entries;
  let start = entries.findLastIndex((e) => e.kind === 'user');
  if (start < 0) start = Math.max(0, entries.length - 6);
  const turn = entries.slice(start);
  const tools = turn.filter((e) => e.kind === 'tool');
  const failed = tools.filter((e) => e.error).length;
  let out = `It ran ${tools.length} tool call${tools.length === 1 ? '' : 's'}${failed ? `, ${failed} failed` : ''}.\n`;
  for (const e of turn) if (e.kind !== 'tool' || e.error) out += `${entryText(e, 2500)}\n`;
  return out.slice(-6000);
}

async function changesText() {
  const diff = await requestReply('diff', {}, 20000);
  if (diff.error) return { error: diff.error };
  const files = diff.files ?? [];
  const added = files.reduce((n, f) => n + f.added, 0);
  const removed = files.reduce((n, f) => n + f.removed, 0);
  let out = `${diff.label}: ${files.length} file${files.length === 1 ? '' : 's'}, +${added} −${removed}\n`;
  for (const f of files) {
    out += `\n${f.status} ${f.path} +${f.added} −${f.removed}\n`;
    if (f.binary || f.omitted) continue;
    for (const line of (f.hunks ?? []).flat()) if (line.k === '+' || line.k === '-') out += `${line.k}${line.t}\n`;
    if (out.length > 12000) {
      out += '…(cut)\n';
      break;
    }
  }
  return { changes: out.slice(0, 12000) };
}

/** Runs one of `VOICE_TOOLS` for Gemini. */
async function voiceTool(name, args) {
  const live = voice.live;
  const busy = !!state.status?.running;
  const notConnected = { error: "The session isn't connected right now." };
  switch (name) {
    case 'send_prompt': {
      const text = String(args.text ?? '').trim();
      if (!text) return { error: 'Empty prompt.' };
      if (!send({ t: 'send', text }, true)) return notConnected;
      live?.note(`Sent to tamale: ${text}`);
      return {
        sent: true,
        queued: busy,
        next: busy
          ? 'tamale is busy; this runs after the current turn. Tell the user so in a few words.'
          : "tamale is working on it. Tell the user in a few words what you asked; you'll be told when it's done.",
      };
    }
    case 'get_status':
      return voiceStatus();
    case 'read_conversation': {
      const last = Math.min(Math.max(Number(args.last) || 8, 1), 40);
      live?.note('Read the conversation');
      return { conversation: conversationText(last) };
    }
    case 'get_changes':
      live?.note('Looked at the changes');
      try {
        return await changesText();
      } catch (e) {
        return { error: e.message };
      }
    case 'interrupt':
      if (!busy) return { error: "tamale isn't doing anything." };
      if (!send({ t: 'interrupt' }, true)) return notConnected;
      live?.note('Interrupted tamale');
      return { interrupted: true, next: 'Confirm it to the user in a few words.' };
    case 'answer_permission': {
      const approval = state.approval;
      if (!approval) return { error: "tamale isn't waiting for permission. To ask tamale to do something, use send_prompt." };
      const allow = args.allow === true || args.allow === 'true';
      const reason = allow ? undefined : String(args.reason ?? '').trim() || undefined;
      // Never "always": a standing rule is the user's to tap, not a voice model's to infer.
      if (!send({ t: 'approve', choice: allow ? 'yes' : 'no', reason }, true)) return notConnected;
      voice.answering.approval = true;
      live?.note(`${allow ? 'Allowed' : 'Refused'}: ${approval.summary}`);
      return { answered: allow ? 'yes' : 'no', next: 'Confirm it to the user in a few words.' };
    }
    case 'answer_questions': {
      const q = state.question;
      if (!q) return { error: "tamale isn't asking anything." };
      const answers = (Array.isArray(args.answers) ? args.answers : []).map((a) => ({
        chosen: Array.isArray(a?.chosen) ? a.chosen.map(String) : [],
        other: String(a?.other ?? '').trim() || undefined,
      }));
      if (answers.length !== q.questions.length) return { error: `tamale asked ${q.questions.length} questions; give one answer for each.` };
      if (answers.some((a) => !a.chosen.length && !a.other)) return { error: 'Every question needs an answer.' };
      if (!send({ t: 'answer', answers }, true)) return notConnected;
      voice.answering.question = true;
      live?.note("Answered tamale's questions");
      return { answered: true };
    }
    default:
      return { error: `No function called ${name}.` };
  }
}

function voiceInstruction(page) {
  const s = state.status;
  const place = s ? `${s.project} (${s.cwd})` : 'a project';
  let out = `You're a voice assistant for the user's coding agent, tamale, which is running on their computer in ${place}. The user has tamale's web page open and is talking to you; you control that tamale session through your functions, as if typing into it.

How to work:
- For anything about the code or the machine (does it build, run the tests, what's in a file, fix something), ask tamale with send_prompt. Write the prompt as a clear instruction, as the user would type it. Say briefly what you asked, then stop talking: tamale takes a while, and the app will tell you when it's done.
- To answer what tamale did or said, use read_conversation, get_status or get_changes first rather than guessing.
- When the app tells you a turn finished, tell the user the gist in a sentence or two. Mention errors. Don't read code, paths or long output aloud unless asked.
- When tamale asks permission, say what it wants to do in plain words and ask yes or no. Call answer_permission only after the user clearly answers that request. Never approve on your own.
- When tamale asks questions, read them and their options, and answer with answer_questions only with what the user chose.
- If the user wants tamale to stop, call interrupt.
Keep it spoken: short sentences, no markdown, no lists of symbols.

Right now: ${statusLine()}`;
  if (state.approval) out += `\ntamale is waiting for permission: ${describeApproval(state.approval)}`;
  if (state.question) out += `\ntamale is waiting for answers to: ${describeQuestions(state.question)}`;
  out += `\n\nThe end of the conversation so far:\n${conversationText(6)}`;
  if (page) {
    out += `\n\nThe user started this conversation from a page tamale made, titled "${page.title}", to talk about it. Answer questions about it from the page, the way you would in a spoken conversation; if the page doesn't say, tell them, then answer from what you know if you can. To change the page, ask tamale with send_prompt to revise the "${page.title}" artifact.\n\n=== Page text ===\n${page.text.slice(0, 150000)}`;
    if (page.html.length <= 120000) out += `\n\n=== Page HTML ===\n${page.html}`;
  }
  return out;
}

/** An artifact's document and its text, as Gemini reads them. */
async function artifactForVoice(artifact) {
  const html = await artifactHtml(artifact.url);
  const doc = new DOMParser().parseFromString(html, 'text/html');
  for (const node of doc.querySelectorAll('script, style, noscript, template')) node.remove();
  const text = (doc.body?.textContent ?? '').replace(/[ \t]+/g, ' ').replace(/\n\s*\n+/g, '\n').trim();
  // Without tamale's own scripts, which it adds to every artifact (see artifacts.rs).
  const document = html.replace(/\n<script>[\s\S]*$/, '');
  return { title: artifact.title, text, html: document };
}

// Events the page tells Gemini about.

function voiceTurnEnded() {
  if (!voice.live?.active) return;
  // The reply's last entries can come just after the status that says it's done.
  setTimeout(() => {
    if (!voice.live?.active || state.status?.running) return;
    const last = state.entries.findLast((e) => e.kind === 'assistant');
    const video = /```video\n/.test(last?.md ?? '') ? '\nIts reply has a video, which is playing in the chat now.' : '';
    voice.live.inform(`tamale finished its turn. Tell the user how it went.${video}\n${lastTurnText()}`, 'tamale finished');
  }, 600);
}

function voiceApproval(a) {
  voice.answering.approval = false;
  if (!voice.live?.active) return;
  voice.live.inform(`tamale is waiting for permission: ${describeApproval(a)}. Ask the user whether to allow it.`, `tamale asks permission: ${a.summary}`);
}

function voiceQuestions(q) {
  voice.answering.question = false;
  if (!voice.live?.active) return;
  voice.live.inform(`tamale is asking the user: ${describeQuestions(q)}. Read the questions and options to them.`, 'tamale has questions');
}

/** A card was answered: by the user on screen, unless Gemini just did. */
function voiceAnswered(kind) {
  const byGemini = voice.answering[kind];
  voice.answering[kind] = false;
  if (byGemini || !voice.live?.active) return;
  voice.live.inform(
    kind === 'approval'
      ? "The permission request is settled: the user answered it on screen (or tamale stopped waiting). Don't ask about it any more."
      : "tamale's questions are settled: the user answered them on screen (or tamale stopped waiting). Don't ask about them any more.",
    kind === 'approval' ? 'Permission answered on screen' : 'Questions answered on screen',
  );
}

// ---- the card

async function loadGeminiSettings() {
  const m = await requestReply('gemini');
  if (m.error) throw new Error(m.error);
  voice.settings = m;
  return m;
}

/**
 * Opens the card and starts a conversation, about `artifact` if given. Called from a click:
 * the audio has to start in it.
 */
async function startVoice({ artifact = null, ctx = audioContext() } = {}) {
  voice.live?.stop();
  voice.artifact = artifact;
  voice.view = 'talk';
  voice.folded = false;
  voice.note = '';
  const live = new GeminiLive(renderVoice);
  live.onToolCall = voiceTool;
  voice.live = live;
  showVoice();
  if (!ctx) return live.end("This browser can't play audio from a page.");
  try {
    const settings = await loadGeminiSettings();
    if (!settings.key) {
      ctx.close();
      voice.view = 'settings';
      voice.note = 'Add a Gemini API key to talk to tamale.';
      return renderVoice();
    }
    const page = artifact ? await artifactForVoice(artifact) : null;
    if (voice.live !== live) return ctx.close();
    await live.start({
      ctx,
      key: settings.key,
      model: settings.model,
      voice: settings.voice,
      instruction: voiceInstruction(page),
      tools: VOICE_TOOLS,
    });
  } catch (e) {
    ctx.close().catch(() => {});
    live.end(e.message || String(e));
  }
}

/** An audio context, made in a click: only then may a page play sound in every browser. */
function audioContext() {
  const AC = window.AudioContext || window.webkitAudioContext;
  if (!AC) return null;
  const ctx = new AC();
  ctx.resume?.().catch(() => {});
  return ctx;
}

function showVoice() {
  $('voice').hidden = false;
  showChat();
  renderVoice();
}

function closeVoice() {
  voice.live?.stop();
  voice.live = null;
  const node = $('voice');
  node.hidden = true;
  node.replaceChildren();
  node._talk = null;
  node._settings = false;
  $('toggle-voice').classList.remove('on');
}

$('toggle-voice').onclick = () => (voice.live?.active ? closeVoice() : startVoice());

const PHASE_TEXT = {
  idle: 'Starting…',
  connecting: 'Connecting…',
  listening: 'Listening',
  speaking: 'Gemini is speaking. Talk to interrupt.',
  ended: 'Ended',
};

/** Builds the card once, and after that only changes what changed (keeping the field's focus). */
function renderVoice() {
  const node = $('voice');
  if (node.hidden) return;
  const live = voice.live;
  $('toggle-voice').classList.toggle('on', !!live?.active);
  // The form is built once: the conversation going on behind it mustn't clear what's typed.
  if (voice.view === 'settings') return node._settings || renderVoiceSettings(node);
  node._settings = false;
  if (!node._talk) node.replaceChildren(buildVoiceTalk(node));
  const parts = node._talk;
  node.classList.toggle('folded', voice.folded);
  node.classList.toggle('speaking', live?.phase === 'speaking');
  node.classList.toggle('active', !!live?.active);
  parts.title.textContent = voice.artifact ? `Gemini · ${voice.artifact.title}` : 'Gemini';
  let status = PHASE_TEXT[live?.phase ?? 'idle'];
  if (live?.phase === 'listening') status = live.micOff ? 'Type to Gemini' : live.muted ? 'Muted' : 'Listening';
  parts.status.textContent = status;
  parts.model.textContent = live?.model ?? '';
  parts.mute.hidden = !live?.active || !!live?.micOff;
  parts.mute.innerHTML = live?.muted ? ICON.micOff : ICON.mic;
  parts.mute.classList.toggle('on', !!live?.muted);
  parts.mute.title = live?.muted ? 'Unmute' : 'Mute';
  parts.end.innerHTML = live?.active ? ICON.hangup : ICON.reload;
  parts.end.title = live?.active ? 'End the conversation' : 'Start again';
  parts.end.classList.toggle('danger', !!live?.active);
  parts.field.disabled = !live?.ready;
  parts.field.placeholder = live?.ready ? (live.micOff ? 'Type to Gemini…' : 'Or type to Gemini…') : '';
  // The transcript: lines are only added or grow, so only the last few need touching.
  const lines = live?.lines ?? [];
  const list = parts.lines;
  const stick = list.scrollHeight - list.scrollTop - list.clientHeight < 40;
  while (list.children.length > lines.length) list.lastChild.remove();
  lines.forEach((line, i) => {
    let row = list.children[i];
    if (!row) {
      row = line.who === 'action'
        ? h('div', { class: 'vline action' })
        : h('div', { class: `vline ${line.who}` }, h('span', { class: 'vwho', text: line.who === 'you' ? 'You' : 'Gemini' }), h('span', { class: 'vtext' }));
      list.append(row);
    }
    const target = line.who === 'action' ? row : row.lastChild;
    if (target.textContent !== line.text) target.textContent = line.text;
  });
  const messages = [];
  if (!lines.length && live?.phase === 'listening') {
    messages.push(['hint', voice.artifact ? `Ask anything about “${voice.artifact.title}”.` : 'Ask what tamale is doing, or tell it what to do.']);
  }
  if (live?.micOff) messages.push(['warn', live.micOff]);
  if (live?.notice) messages.push(['warn', live.notice]);
  if (live?.phase === 'ended' && live.reason) messages.push(['error', live.reason]);
  parts.notes.replaceChildren(...messages.map(([kind, text]) => h('div', { class: `vnote ${kind}`, text })));
  if (stick) list.scrollTop = list.scrollHeight;
}

function buildVoiceTalk(node) {
  const btn = (cls, title, icon, onclick) => h('button', { class: `vbtn ${cls}`, title, 'aria-label': title, html: icon, onclick });
  const field = h('input', {
    class: 'vfield',
    enterkeyhint: 'send',
    autocomplete: 'off',
    onkeydown: (ev) => {
      if (ev.key === 'Enter' && !ev.isComposing) {
        ev.preventDefault();
        sendTyped();
      }
      ev.stopPropagation();
    },
  });
  const sendTyped = () => {
    if (voice.live?.sendText(field.value)) field.value = '';
  };
  const parts = {
    orb: h('span', { class: 'vorb' }),
    title: h('span', { class: 'vtitle' }),
    status: h('span', { class: 'vstatus' }),
    model: h('span', { class: 'vmodel' }),
    lines: h('div', { class: 'vlines', 'aria-live': 'polite' }),
    notes: h('div', { class: 'vnotes' }),
    field,
    mute: btn('', 'Mute', ICON.mic, () => voice.live?.setMuted(!voice.live.muted)),
    end: btn('', 'End the conversation', ICON.hangup, () => (voice.live?.active ? voice.live.stop() : startVoice({ artifact: voice.artifact }))),
  };
  node._talk = parts;
  // The orb follows your voice, and glows while Gemini speaks.
  const pulse = () => {
    if (node._talk !== parts || node.hidden) return;
    const live = voice.live;
    const level = live?.phase === 'speaking' ? 0.5 : live?.level ?? 0;
    parts.orb.style.setProperty('--level', level.toFixed(2));
    requestAnimationFrame(pulse);
  };
  requestAnimationFrame(pulse);
  return h('div', { class: 'vcard' },
    h('div', { class: 'vhead', onclick: (ev) => {
      if (ev.target.closest('button')) return;
      voice.folded = !voice.folded;
      renderVoice();
    } },
      parts.orb,
      h('span', { class: 'vnames' }, parts.title, parts.status),
      parts.model,
      h('span', { class: 'spacer' }),
      btn('', 'Gemini settings', ICON.gear, async () => {
        await loadGeminiSettings().catch(() => {});
        voice.view = 'settings';
        renderVoice();
      }),
      btn('', 'Close', ICON.close, closeVoice)),
    h('div', { class: 'vbody' }, parts.lines, parts.notes),
    h('div', { class: 'vcontrols' },
      parts.field,
      btn('vsend', 'Send to Gemini', ICON.up, sendTyped),
      parts.mute,
      parts.end));
}

async function renderVoiceSettings(node) {
  node._talk = null;
  node._settings = true;
  node.classList.remove('folded', 'speaking');
  const s = voice.settings;
  const keyField = h('input', {
    class: 'vfield',
    type: 'password',
    autocomplete: 'off',
    spellcheck: 'false',
    placeholder: s?.key ? 'Paste a different key to replace it' : 'Paste a key: AIza…',
    onkeydown: (ev) => ev.stopPropagation(),
  });
  const modelSelect = h('select', { class: 'vselect' }, h('option', { value: '', text: 'The newest Live model (automatic)' }));
  const voiceSelect = h('select', { class: 'vselect' },
    h('option', { value: '', text: "Gemini's choice" }),
    GEMINI_VOICES.map((v) => h('option', { value: v, text: v })));
  voiceSelect.value = s?.voice ?? '';
  const note = h('div', { class: 'vnote hint' });
  const where = !s?.key
    ? `No key yet. Get one at AI Studio, or set ${s?.env || 'GEMINI_API_KEY'} where tamale runs.`
    : s.from === 'env'
      ? `Using ${s.env} from tamale's environment. A key pasted here is used instead.`
      : 'Saved on this machine, in ~/.config/tamale/gemini.json (only you can read it).';
  const save = async (extra = {}) => {
    const body = { model: modelSelect.value, voice: voiceSelect.value, ...extra };
    const key = keyField.value.trim();
    if (key && !('key' in extra)) body.key = key;
    note.textContent = 'Saving…';
    try {
      const m = await requestReply('gemini_save', body);
      if (m.error) throw new Error(m.error);
      voice.settings = m;
      return true;
    } catch (e) {
      note.textContent = e.message;
      return false;
    }
  };
  const back = () => {
    voice.view = 'talk';
    voice.note = '';
    node.replaceChildren();
    renderVoice();
  };
  node.replaceChildren(
    h('div', { class: 'vcard settings' },
      h('div', { class: 'vhead' },
        h('span', { class: 'vnames' }, h('span', { class: 'vtitle', text: 'Gemini settings' })),
        h('span', { class: 'spacer' }),
        h('button', { class: 'vbtn', title: 'Close', 'aria-label': 'Close', html: ICON.close, onclick: closeVoice })),
      voice.note ? h('div', { class: 'vnote warn', text: voice.note }) : null,
      h('label', { class: 'vlabel' }, h('span', { text: 'API key' }), keyField),
      h('div', { class: 'vnote hint' },
        h('span', { text: `${where} ` }),
        h('a', { href: GEMINI_KEY_URL, target: '_blank', rel: 'noopener noreferrer', text: 'Get a key' })),
      h('label', { class: 'vlabel' }, h('span', { text: 'Model' }), modelSelect),
      h('label', { class: 'vlabel' }, h('span', { text: 'Voice' }), voiceSelect),
      note,
      h('div', { class: 'ap-actions vactions' },
        s?.from === 'saved'
          ? h('button', { class: 'btn danger', onclick: async () => {
              if (await save({ key: '' })) renderVoiceSettings(node);
            } }, h('span', { text: 'Forget the saved key' }))
          : null,
        h('span', { class: 'spacer' }),
        voice.live?.active ? h('button', { class: 'btn', onclick: back }, h('span', { text: 'Back' })) : null,
        h('button', { class: 'btn primary', onclick: async () => {
          const ctx = audioContext();
          if (!(await save()) || !voice.settings?.key) {
            if (voice.settings && !voice.settings.key) note.textContent = 'Paste a key first.';
            ctx?.close();
            return;
          }
          // The choices take effect in a new conversation.
          node._settings = false;
          startVoice({ artifact: voice.artifact, ctx });
        } }, h('span', { text: voice.live?.active ? 'Save and restart' : 'Save and start' })))),
  );
  // The models this key can use, once Google says.
  if (s?.key) {
    try {
      const models = await liveModels(s.key);
      for (const m of models) modelSelect.append(h('option', { value: m, text: m }));
      if (s.model && !models.includes(s.model)) modelSelect.append(h('option', { value: s.model, text: `${s.model} (not listed)` }));
      modelSelect.value = s.model ?? '';
    } catch (e) {
      note.textContent = e.message;
    }
  }
}

// ---- videos a reply shows (```video blocks; see `render::push_videos`)

/** Each video's blob URL, once asked for, by path. */
const videos = new Map();

function videoData(path) {
  let got = videos.get(path);
  if (!got) {
    got = requestReply('video', { path }, 180000).then(
      (m) => {
        if (m.error) throw new Error(m.error);
        return { url: URL.createObjectURL(new Blob([fromBase64(m.data)], { type: m.mime })), mime: m.mime };
      },
    );
    got.catch(() => videos.delete(path));
    videos.set(path, got);
  }
  return got;
}

/** Turns `.video[data-path]` placeholders into players, loading each as it comes into view. */
function mountVideos(scope) {
  const found = scope.matches?.('.video[data-path]') ? [scope] : scope.querySelectorAll('.video[data-path]');
  for (const box of found) {
    if (box._mounted) continue;
    box._mounted = true;
    const { path, title } = box.dataset;
    const stage = h('div', { class: 'video-stage' });
    const download = h('a', { class: 'artifact-btn', title: 'Download', 'aria-label': 'Download', html: ICON.download, hidden: true });
    box.append(
      h('div', { class: 'artifact-bar' },
        h('span', { class: 'artifact-icon video-icon', html: ICON.play }),
        h('span', { class: 'artifact-title', text: title, title: path }),
        h('span', { class: 'spacer' }),
        download),
      stage,
    );
    const load = async () => {
      stage.replaceChildren(h('div', { class: 'video-wait', text: 'Loading the video…' }));
      try {
        const { url, mime } = await videoData(path);
        stage.replaceChildren(mime.startsWith('image/')
          ? h('img', { src: url, alt: title })
          : h('video', { src: url, controls: true, playsinline: true, loop: true, preload: 'metadata' }));
        download.href = url;
        download.download = title;
        download.hidden = false;
      } catch (e) {
        stage.replaceChildren(h('div', { class: 'video-wait error' },
          h('span', { text: e.message }),
          h('button', { class: 'btn', onclick: load }, h('span', { text: 'Try again' }))));
      }
    };
    const seen = new IntersectionObserver((items) => {
      if (!items.some((i) => i.isIntersecting)) return;
      seen.disconnect();
      load();
    }, { rootMargin: '600px 0px' });
    seen.observe(box);
  }
}

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
