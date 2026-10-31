// tamale's web UI: the conversation first, with a code browser beside it (/code).
//
// One WebSocket carries everything (see src/web.rs). tamale sends the session as JSON:
// `entries` (from an index on, replacing what was there), `status` and `approval`.
// The page sends prompts, interrupts and approvals, and asks for the file tree, files
// and searches, whose replies carry the request's `seq` so stale ones are dropped.

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
};

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
function toast(text, ms = 5000) {
  const node = $('toast');
  node.innerHTML = inline(text);
  node.hidden = false;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (node.hidden = true), ms);
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
  /** From the server's `tree`: [{ name, path, files, total }]. */
  roots: [],
  fileSets: [],
  treeLoaded: false,
};

// ---------------------------------------------------------------- connection

const token = new URLSearchParams(location.search).get('t') ?? '';
let ws;
let seq = 0;
let retry = 0;
const latest = {};

function connect() {
  ws = new WebSocket(`ws://${location.host}/ws?t=${encodeURIComponent(token)}`);
  ws.onopen = () => {
    retry = 0;
    setConn('live', 'Connected to tamale');
    request('tree');
  };
  ws.onmessage = (ev) => {
    try {
      onMessage(JSON.parse(ev.data));
    } catch (e) {
      console.error(e);
    }
  };
  ws.onclose = (ev) => {
    if (ev.code === 4000) {
      setConn('stopped', ev.reason);
      toast(ev.reason || 'tamale stopped serving this page.', 1e9);
      return;
    }
    setConn('down', 'Reconnecting…');
    setTimeout(connect, Math.min(5000, 250 * 2 ** retry++));
  };
}

function setConn(kind, title) {
  const node = $('conn');
  node.className = `conn ${kind}`;
  node.title = title;
}

function send(message) {
  if (ws?.readyState === WebSocket.OPEN) {
    ws.send(JSON.stringify(message));
    return true;
  }
  toast('Not connected to tamale. Is it still running?');
  return false;
}

/** Sends a request whose reply is only used if no newer one of its kind was made. */
function request(t, body = {}, kind = t) {
  const s = ++seq;
  latest[kind] = s;
  send({ t, seq: s, ...body });
  return s;
}

function onMessage(m) {
  switch (m.t) {
    case 'entries':
      return applyEntries(m.from, m.items);
    case 'status':
      return applyStatus(m.status);
    case 'approval':
      return applyApproval(m.approval);
    case 'roots':
      return request('tree');
    case 'tree':
      if (m.seq === latest.tree) applyTree(m.roots);
      return;
    case 'file':
      return onFile(m);
    case 'grep':
      if (m.seq === latest.grep) applyGrep(m);
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
      node.innerHTML = e.html;
      decorate(node);
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

const FILE_TOOLS = new Set(['read_file', 'write_file', 'edit_file']);

function patchTool(node, e) {
  const pending = e.output == null;
  node._entry = e;
  node.classList.toggle('pending', pending);
  node.classList.toggle('error', !pending && e.error);
  node.classList.toggle('done', !pending && !e.error);
  const summary = e.summary.split('\n')[0];
  // The summary is usually the description's subject ("Read src/main.rs"); skip it then.
  const showSummary = summary && !e.description.includes(summary);
  const canOpen = FILE_TOOLS.has(e.name) && summary;
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
    (canOpen ? `<span class="tool-open" data-path="${esc(summary)}">${ICON.open}open</span>` : '') +
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
      ? e.output
          .split('\n')
          .map((l) => {
            const cls = l.startsWith('@@') ? 'hunk' : /^\+(?!\+\+)/.test(l) ? 'add' : /^-(?!--)/.test(l) ? 'del' : '';
            return cls ? `<span class="${cls}">${esc(l)}</span>` : esc(l);
          })
          .join('\n')
      : esc(e.output || '(no output)');
    parts.push(`<pre>${body}${e.cut ? '<span class="cut">… output cut here</span>' : ''}</pre>`);
  }
  out.innerHTML = parts.join('');
}

/** Copy buttons on code blocks, and file paths in `code` linked to the code browser. */
function decorate(node) {
  for (const block of node.querySelectorAll('.codeblock')) {
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
  linkPaths(node);
}

function linkPaths(node) {
  if (!state.treeLoaded) return;
  for (const code of node.querySelectorAll('code:not(pre code)')) {
    const target = resolvePath(code.textContent);
    code.classList.toggle('pathlink', !!target);
    if (target) code.title = `Open ${target.path}${target.line ? `:${target.line}` : ''}`;
  }
}

thread.addEventListener('click', (ev) => {
  const link = ev.target.closest('.pathlink, .tool-open');
  if (!link) return;
  ev.stopPropagation();
  const text = link.dataset.path ?? link.textContent;
  const target = resolvePath(text);
  if (target) openFile(target.root, target.path, target.line);
  else toast(`\`${text}\` isn't a file in the workspace.`);
});

/**
 * A file the text names: a workspace-relative or absolute path, optionally with
 * `:line`, `:line-line` or `#Lline`. Null if no such file is known.
 */
function resolvePath(raw) {
  let text = raw.trim().replace(/^['"`]|['"`]$/g, '');
  let line = null;
  const m = text.match(/^(.*?)(?::(\d+)(?:[-–:]\d+)?|#L(\d+)(?:-L?\d+)?)$/);
  if (m) {
    text = m[1];
    line = Number(m[2] ?? m[3]);
  }
  if (!text || /\s/.test(text) || text.length > 400) return null;
  for (const [root, r] of state.roots.entries()) {
    let rel = text;
    if (rel.startsWith(r.path + '/')) rel = rel.slice(r.path.length + 1);
    rel = rel.replace(/^\.\//, '');
    if (state.fileSets[root].has(rel)) return { root, path: rel, line };
  }
  // A path the agent shortened, if exactly one file ends with it.
  if (text.includes('/')) {
    const found = [];
    for (const [root, r] of state.roots.entries())
      for (const f of r.files) if (f.endsWith('/' + text)) found.push({ root, path: f, line });
    if (found.length === 1) return found[0];
  }
  return null;
}

// ---------------------------------------------------------------- activity

function activityLabel() {
  const s = state.status;
  if (state.approval) return 'Waiting for your approval';
  if (s.compacting) return 'Compacting the conversation';
  const last = state.entries[state.entries.length - 1];
  if (last?.kind === 'tool' && last.output == null) return last.description;
  if (last?.kind === 'assistant') return 'Writing';
  return 'Thinking';
}

function updateActivity() {
  const s = state.status;
  const node = $('activity');
  const running = !!s?.running;
  const stick = nearBottom();
  node.hidden = !running;
  if (!running) return;
  node.classList.toggle('waiting', !!state.approval);
  $('act-label').textContent = activityLabel();
  $('act-time').textContent = s.since ? fmtElapsed(Math.max(0, Math.floor(Date.now() / 1000) - s.since)) : '';
  if (stick) scrollToBottom();
}
setInterval(() => state.status?.running && updateActivity(), 1000);

// ---------------------------------------------------------------- status

const MODES = ['ask', 'accept-edits', 'auto'];

function applyStatus(s) {
  const wasRunning = state.status?.running;
  state.status = s;
  $('project-name').textContent = s.project;
  $('project-dirs').textContent = s.dirs.length ? `+${s.dirs.length}` : '';
  $('project').title = [s.cwd, ...s.dirs].join('\n');
  $('hero-sub').textContent = s.cwd;
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
  $('hint-left').textContent = s.goal ? `goal: ${s.goal}` : `via ${s.backend}`;
  $('queued').replaceChildren(...s.queued.map((q) => h('div', { class: 'queued-item', text: q })));
  document.title = `${s.running ? '● ' : ''}${s.project} · tamale`;
  updateSend();
  updateActivity();
  // The agent may have changed files while it ran.
  if (wasRunning && !s.running) afterToolResult();
}

$('mode').onclick = () => {
  const next = MODES[(MODES.indexOf(state.status?.mode) + 1) % MODES.length];
  send({ t: 'send', text: `/permissions ${next}` });
};
$('model').onclick = () => setInput('/model ');

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
}

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
    scrollToBottom(true);
  }
}

/** Commands, in the order the menu offers them; `local` ones run in the page. */
const COMMANDS = [
  { name: '/code', args: '[path]', desc: 'Browse the code beside the chat', local: true },
  { name: '/files', desc: 'Go to a file by name', local: true },
  { name: '/search', args: '<text>', desc: 'Search the code for text', local: true },
  { name: '/help', desc: 'Commands and shortcuts', local: true },
  { name: '/model', args: '<id>', desc: 'Switch models, e.g. claude-opus-5-5' },
  { name: '/effort', args: '<level>', desc: 'low, medium, high, xhigh, max or default' },
  { name: '/thinking', args: '[on|off]', desc: 'Toggle adaptive thinking' },
  { name: '/permissions', args: '[mode]', desc: 'Show the rules, or set ask, accept-edits, auto' },
  { name: '/compact', args: '[focus]', desc: 'Summarize the conversation to free up context' },
  { name: '/goal', args: '<condition>', desc: 'Keep working until the condition holds' },
  { name: '/undo', desc: 'Restore files the last turn changed and drop it' },
  { name: '/add-dir', args: '[path]', desc: "Add a directory to the agent's scope" },
  { name: '/resume', args: '<n|id>', desc: 'Continue a saved session' },
  { name: '/copy', args: '[all]', desc: 'Copy the latest reply to the clipboard' },
  { name: '/clear', desc: 'Clear the conversation and start fresh' },
  { name: '/web off', desc: 'Stop serving this page' },
];

/** Commands that open a picker or screen in the terminal when given no arguments. */
const TERMINAL_ONLY = {
  '/model': 'Pick a model with `/model <id>` here, or `/model` in the terminal for the list.',
  '/models': 'Pick a model with `/model <id>` here, or `/model` in the terminal for the list.',
  '/effort': 'Set it with `/effort <level>`: low, medium, high, xhigh, max or default.',
  '/resume': 'Resume with `/resume <n|id>` here, or `/resume` in the terminal for the list.',
  '/continue': 'Resume with `/resume <n|id>` here, or `/resume` in the terminal for the list.',
  '/backend': 'Use `/backend api|claude-code|codex` here, or `/backend` in the terminal for the list.',
  '/rewind': '`/rewind` picks a turn in the terminal; `/undo` works here.',
  '/diff': '`/diff` opens in the terminal; `/code` browses files here.',
  '/login': 'Sign-in runs in the terminal tamale is running in.',
  '/paste': 'Paste with ⌘V here.',
  '/mouse': '`/mouse` only affects the terminal.',
  '/browser': '`/browser` mirrors the terminal; you are already in the web UI.',
};

function runLocal(text) {
  const [name, ...rest] = text.split(/\s+/);
  const arg = text.slice(name.length).trim();
  switch (name) {
    case '/code':
    case '/browse':
      if (arg) {
        const target = resolvePath(arg);
        if (target) openFile(target.root, target.path, target.line);
        else if (state.treeLoaded) {
          openCode();
          setFind(arg);
        } else toast('The file list is still loading.');
      } else toggleCode();
      return true;
    case '/files':
      openPicker(arg);
      return true;
    case '/search':
    case '/grep':
      openCode();
      setMode('search');
      setFind(arg, true);
      return true;
    case '/help':
      openHelp();
      return true;
  }
  if (TERMINAL_ONLY[name] && (!rest.length || ['/rewind', '/diff', '/login', '/paste', '/browser'].includes(name))) {
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
  if (ev.key === 'Enter' && !ev.shiftKey && !ev.altKey) {
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

for (const chip of document.querySelectorAll('.chip')) {
  chip.onclick = () => {
    if (chip.dataset.command) runLocal(chip.dataset.command);
    else setInput(chip.dataset.prompt);
  };
}

/** Adds `text` to the prompt on a paragraph of its own, ready for a question. */
function appendToInput(text) {
  let value = input.value;
  if (value.trim() && !value.endsWith('\n\n')) value += value.endsWith('\n') ? '\n' : '\n\n';
  setInput(value + text);
  input.scrollTop = input.scrollHeight;
}

// ---------------------------------------------------------------- code browser

const code = {
  open: false,
  mode: 'files',
  /** Open folders, as `root:dir/path`. */
  expanded: new Set(store.get('expanded', [])),
  /** Open files: [{ root, path }], and which one shows. */
  tabs: store.get('tabs', []),
  active: store.get('active', -1),
  /** Loaded files by `root:path`: { lines, syntax, error }. */
  files: new Map(),
  /** Selected lines of the file showing: { a, b } (1-based, unordered). */
  sel: null,
  matches: [],
};
const fileKey = (root, path) => `${root}:${path}`;
const activeTab = () => code.tabs[code.active];

function saveCode() {
  store.set('codeOpen', code.open);
  store.set('tabs', code.tabs);
  store.set('active', code.active);
  store.set('expanded', [...code.expanded].slice(-400));
}

function openCode(focusFind = false) {
  if (!code.open) {
    code.open = true;
    $('main').classList.add('with-code');
    $('code').hidden = false;
    $('splitter').hidden = false;
    $('toggle-code').classList.add('on');
    renderSide();
    renderTabs();
    renderView();
    saveCode();
  }
  if (focusFind) $('find').focus();
}

function closeCode() {
  if (!code.open) return;
  code.open = false;
  $('main').classList.remove('with-code');
  $('code').hidden = true;
  $('splitter').hidden = true;
  $('toggle-code').classList.remove('on');
  saveCode();
  input.focus();
}

const toggleCode = () => (code.open ? closeCode() : openCode(!activeTab()));
$('toggle-code').onclick = toggleCode;
$('close-code').onclick = closeCode;
$('code-refresh').onclick = () => {
  request('tree');
  const tab = activeTab();
  if (tab) loadFile(tab.root, tab.path);
};

// The splitter between chat and code, its position kept as a share of the width.
{
  const splitter = $('splitter');
  const main = $('main');
  const saved = store.get('chatWidth', null);
  if (saved) main.style.setProperty('--chat-w', `${saved}%`);
  splitter.addEventListener('pointerdown', (ev) => {
    ev.preventDefault();
    splitter.setPointerCapture(ev.pointerId);
    splitter.classList.add('drag');
    document.body.classList.add('resizing');
    const box = main.getBoundingClientRect();
    const move = (e) => {
      const px = Math.min(Math.max(e.clientX - box.left, 340), box.width - 420);
      const pct = (px / box.width) * 100;
      main.style.setProperty('--chat-w', `${pct}%`);
      store.set('chatWidth', pct);
    };
    const up = () => {
      splitter.classList.remove('drag');
      document.body.classList.remove('resizing');
      splitter.removeEventListener('pointermove', move);
      splitter.removeEventListener('pointerup', up);
    };
    splitter.addEventListener('pointermove', move);
    splitter.addEventListener('pointerup', up);
  });
}

function applyTree(roots) {
  state.roots = roots;
  state.fileSets = roots.map((r) => new Set(r.files));
  state.treeLoaded = true;
  // Tabs of files that are gone are closed.
  const before = code.tabs.length;
  const active = activeTab();
  code.tabs = code.tabs.filter((t) => state.fileSets[t.root]?.has(t.path));
  if (code.tabs.length !== before) code.active = active ? Math.max(0, code.tabs.indexOf(active)) : code.tabs.length - 1;
  if (code.active >= code.tabs.length) code.active = code.tabs.length - 1;
  for (const node of state.nodes) if (node?.classList.contains('prose')) linkPaths(node);
  if (code.open) {
    renderSide();
    renderTabs();
    if (code.tabs.length !== before) renderView();
  }
  if (!$('picker').hidden) renderPicker();
}

/** After the agent ran a tool: files may have changed. */
let refreshTimer;
function afterToolResult() {
  clearTimeout(refreshTimer);
  refreshTimer = setTimeout(() => {
    request('tree');
    const tab = activeTab();
    if (code.open && tab) loadFile(tab.root, tab.path, { quiet: true });
  }, 400);
}

// ---- sidebar: tree, filtered list, search results

function setMode(mode) {
  code.mode = mode;
  for (const b of $('seg').children) b.classList.toggle('on', b.dataset.mode === mode);
  const find = $('find');
  find.placeholder = mode === 'files' ? 'Filter files…' : 'Search the code…  ⏎';
  find.value = mode === 'files' ? '' : code.query ?? '';
  renderSide();
  find.focus();
}
for (const b of $('seg').children) b.onclick = () => setMode(b.dataset.mode);

function setFind(text, run = false) {
  const find = $('find');
  find.value = text;
  find.focus();
  if (code.mode === 'search' && run && text) runSearch(text);
  else renderSide();
}

let findTimer;
$('find').addEventListener('input', () => {
  if (code.mode === 'files') return renderSide();
  clearTimeout(findTimer);
  const q = $('find').value;
  if (q.length >= 3) findTimer = setTimeout(() => runSearch(q), 350);
});
$('find').addEventListener('keydown', (ev) => {
  if (ev.key === 'Enter') {
    if (code.mode === 'search') runSearch($('find').value);
    else $('tree').querySelector('.row:not(.dir)')?.click();
  }
  if (ev.key === 'Escape' && $('find').value) {
    ev.stopPropagation();
    $('find').value = '';
    renderSide();
  }
});

function runSearch(query) {
  if (!query.trim()) return;
  code.query = query;
  $('tree').innerHTML = '<div class="tree-note">Searching…</div>';
  request('grep', { query });
}

function applyGrep(m) {
  code.matches = m.matches;
  code.truncated = m.truncated;
  if (code.mode === 'search') renderSide();
}

function renderSide() {
  if (!code.open) return;
  const tree = $('tree');
  if (!state.treeLoaded) {
    tree.innerHTML = '<div class="tree-note">Loading files…</div>';
    return;
  }
  if (code.mode === 'search') return renderMatches(tree);
  const q = $('find').value.trim();
  if (q) return renderFiltered(tree, q);
  const rows = [];
  const multi = state.roots.length > 1;
  state.roots.forEach((r, ri) => {
    const node = buildTree(r);
    if (multi) {
      // Roots start open; `${ri}:!` marks one closed.
      const open = !code.expanded.has(`${ri}:!`);
      rows.push(dirRow(r.name, `${ri}:!`, 0, open, 'rootrow'));
      if (open) treeRows(rows, node, ri, '', 1);
    } else treeRows(rows, node, ri, '', 0);
    if (r.total > r.files.length) rows.push(`<div class="tree-note">${r.total - r.files.length} more files not listed.</div>`);
  });
  tree.innerHTML = rows.join('') || '<div class="tree-note">No files.</div>';
  tree.querySelector('.row.on')?.scrollIntoView({ block: 'nearest' });
}

const treeCache = new WeakMap();
function buildTree(root) {
  if (treeCache.has(root)) return treeCache.get(root);
  const top = { dirs: new Map(), files: [] };
  for (const path of root.files) {
    const parts = path.split('/');
    let node = top;
    for (const part of parts.slice(0, -1)) {
      if (!node.dirs.has(part)) node.dirs.set(part, { dirs: new Map(), files: [] });
      node = node.dirs.get(part);
    }
    node.files.push(parts[parts.length - 1]);
  }
  treeCache.set(root, top);
  return top;
}

function dirRow(name, key, depth, open, extra = '') {
  return (
    `<button class="row dir ${extra}${open ? ' open' : ''}" data-toggle="${esc(key)}" data-depth="${depth}">` +
    `<span class="arrow">${ICON.chev}</span><span class="rname">${esc(name)}</span></button>`
  );
}

function treeRows(rows, node, root, prefix, depth) {
  const tab = activeTab();
  const dirs = [...node.dirs.keys()].sort((a, b) => a.localeCompare(b));
  for (const name of dirs) {
    const path = prefix + name;
    // Folders with a single folder inside read as one row, like `src/app`.
    let label = name;
    let child = node.dirs.get(name);
    let full = path;
    while (child.files.length === 0 && child.dirs.size === 1) {
      const [only] = child.dirs.keys();
      label += '/' + only;
      full += '/' + only;
      child = child.dirs.get(only);
    }
    const fullKey = `${root}:${full}`;
    const open = code.expanded.has(fullKey);
    rows.push(dirRow(label, fullKey, depth, open));
    if (open) treeRows(rows, child, root, full + '/', depth + 1);
  }
  for (const name of [...node.files].sort((a, b) => a.localeCompare(b))) {
    const path = prefix + name;
    const on = tab && tab.root === root && tab.path === path;
    rows.push(
      `<button class="row${on ? ' on' : ''}" data-root="${root}" data-path="${esc(path)}" data-depth="${depth}">` +
        `<span class="arrow"></span><span class="rname">${esc(name)}</span></button>`,
    );
  }
}

$('tree').addEventListener('click', (ev) => {
  const row = ev.target.closest('[data-toggle], [data-path]');
  if (!row) return;
  if (row.dataset.toggle) {
    const key = row.dataset.toggle;
    if (code.expanded.has(key)) code.expanded.delete(key);
    else code.expanded.add(key);
    saveCode();
    renderSide();
  } else openFile(Number(row.dataset.root), row.dataset.path, Number(row.dataset.line) || null);
});
// Rows are indented with a style, which the page's CSP doesn't allow inline.
new MutationObserver(() => {
  for (const row of $('tree').querySelectorAll('[data-depth]')) row.style.paddingLeft = `${6 + row.dataset.depth * 14}px`;
}).observe($('tree'), { childList: true });

/** A subsequence match of `q` in `path`, preferring the file name, word starts and runs. */
function fuzzy(q, path) {
  const lower = path.toLowerCase();
  const base = path.lastIndexOf('/') + 1;
  const attempt = (from) => {
    const hits = [];
    let score = 0;
    let qi = 0;
    let last = -2;
    for (let i = from; i < lower.length && qi < q.length; i++) {
      if (lower[i] !== q[qi]) continue;
      let s = 1;
      if (i === last + 1) s += 5;
      if (i === base || '/_-. '.includes(path[i - 1] ?? '/')) s += 3;
      if (i >= base) s += 2;
      score += s;
      hits.push(i);
      last = i;
      qi++;
    }
    return qi === q.length ? { score, hits } : null;
  };
  const inName = attempt(base);
  const found = inName ? { score: inName.score + 10, hits: inName.hits } : attempt(0);
  if (found) found.score -= path.length * 0.03;
  return found;
}

function marked(text, hits, offset = 0) {
  const set = new Set(hits.map((i) => i - offset));
  let out = '';
  for (let i = 0; i < text.length; i++) out += set.has(i) ? `<b>${esc(text[i])}</b>` : esc(text[i]);
  return out;
}

function searchFiles(query, limit) {
  const q = query.toLowerCase().replace(/\s+/g, '');
  const found = [];
  state.roots.forEach((r, root) => {
    for (const path of r.files) {
      const m = fuzzy(q, path);
      if (m) found.push({ root, path, ...m });
    }
  });
  return found.sort((a, b) => b.score - a.score).slice(0, limit);
}

function fileLabel(f) {
  const base = f.path.lastIndexOf('/') + 1;
  const dir = f.path.slice(0, base);
  const multi = state.roots.length > 1;
  return {
    name: marked(f.path.slice(base), f.hits ?? [], base),
    dir: esc((multi ? `${state.roots[f.root].name}/` : '') + dir),
  };
}

function renderFiltered(tree, q) {
  const found = searchFiles(q, 200);
  tree.innerHTML = found.length
    ? found
        .map((f) => {
          const { name, dir } = fileLabel(f);
          return (
            `<button class="row" data-root="${f.root}" data-path="${esc(f.path)}" data-depth="0">` +
            `<span class="rname">${name}</span><span class="rdir">${dir}</span></button>`
          );
        })
        .join('')
    : '<div class="tree-note">No files match.</div>';
}

function renderMatches(tree) {
  if (!code.query) {
    tree.innerHTML = '<div class="tree-note">Type to search every file in the workspace. Smart case: a capital letter makes it case-sensitive.</div>';
    return;
  }
  if (!code.matches.length) {
    tree.innerHTML = `<div class="tree-note">Nothing matches “${esc(code.query)}”.</div>`;
    return;
  }
  const rows = [];
  let last = '';
  const qlen = code.query.length;
  for (const m of code.matches) {
    const key = fileKey(m.root, m.path);
    if (key !== last) {
      last = key;
      const slash = m.path.lastIndexOf('/') + 1;
      rows.push(`<div class="hitfile" title="${esc(m.path)}">${esc(m.path.slice(slash))}<span>${esc(m.path.slice(0, slash))}</span></div>`);
    }
    const chars = [...m.text];
    const text =
      esc(chars.slice(0, m.col).join('')) +
      `<mark>${esc(chars.slice(m.col, m.col + qlen).join(''))}</mark>` +
      esc(chars.slice(m.col + qlen).join(''));
    rows.push(
      `<button class="hit" data-root="${m.root}" data-path="${esc(m.path)}" data-line="${m.line}">` +
        `<span class="hn">${m.line}</span><span class="ht">${text}</span></button>`,
    );
  }
  if (code.truncated) rows.push('<div class="tree-note">Showing the first matches; narrow the search for more.</div>');
  tree.innerHTML = rows.join('');
}

// ---- tabs and the viewer

function openFile(root, path, line = null) {
  openCode();
  let i = code.tabs.findIndex((t) => t.root === root && t.path === path);
  if (i < 0) {
    code.tabs.push({ root, path });
    if (code.tabs.length > 12) code.tabs.splice(code.tabs.findIndex((_, j) => j !== code.tabs.length - 1), 1);
    i = code.tabs.findIndex((t) => t.root === root && t.path === path);
  }
  // Reveal it in the tree.
  const parts = path.split('/');
  for (let d = 1; d < parts.length; d++) code.expanded.add(`${root}:${parts.slice(0, d).join('/')}`);
  code.expanded.delete(`${root}:!`);
  const changed = code.active !== i;
  code.active = i;
  if (changed) code.sel = line ? { a: line, b: line } : null;
  else if (line) code.sel = { a: line, b: line };
  code.pendingLine = line;
  saveCode();
  renderTabs();
  renderView();
  if (code.mode === 'files' && !$('find').value) renderSide();
  loadFile(root, path, { quiet: code.files.has(fileKey(root, path)) });
}

function closeTab(i) {
  code.tabs.splice(i, 1);
  if (code.active >= i) code.active = Math.max(code.tabs.length ? 0 : -1, code.active - 1);
  code.sel = null;
  saveCode();
  renderTabs();
  renderView();
  renderSide();
}

function loadFile(root, path, { quiet = false } = {}) {
  request('open', { root, path }, `file:${fileKey(root, path)}`);
  if (!quiet && !code.files.has(fileKey(root, path))) renderView();
}

function onFile(m) {
  const key = fileKey(m.root, m.path);
  if (m.seq !== latest[`file:${key}`]) return;
  const prev = code.files.get(key);
  code.files.set(key, m);
  const tab = activeTab();
  if (!tab || fileKey(tab.root, tab.path) !== key) return;
  // An unchanged reload leaves the view (and its scroll position) alone.
  if (prev && !code.pendingLine && JSON.stringify(prev.lines) === JSON.stringify(m.lines) && prev.error === m.error) return;
  renderView({ keepScroll: !!prev });
}

function renderTabs() {
  const strip = $('tabs');
  const counts = {};
  for (const t of code.tabs) {
    const name = t.path.split('/').pop();
    counts[name] = (counts[name] ?? 0) + 1;
  }
  strip.replaceChildren(
    ...code.tabs.map((t, i) => {
      const parts = t.path.split('/');
      const name = parts.pop();
      // Same-named files say which folder they are in.
      const label = counts[name] > 1 && parts.length ? `${name} · ${parts.pop()}` : name;
      return h(
        'button',
        {
          class: `tab${i === code.active ? ' on' : ''}`,
          title: t.path,
          onclick: () => openFile(t.root, t.path),
          onauxclick: (ev) => ev.button === 1 && closeTab(i),
        },
        h('span', { text: label }),
        h('span', {
          class: 'tabx',
          html: ICON.close,
          onclick: (ev) => {
            ev.stopPropagation();
            closeTab(i);
          },
        }),
      );
    }),
  );
  strip.querySelector('.tab.on')?.scrollIntoView({ inline: 'nearest', block: 'nearest' });
}

function renderView({ keepScroll = false } = {}) {
  const wrap = $('codewrap');
  const tab = activeTab();
  $('copy-path').hidden = !tab;
  if (!tab) {
    $('crumbs').textContent = '';
    $('vmeta').textContent = '';
    $('ask').hidden = true;
    wrap.innerHTML =
      '<div class="code-empty"><div class="big">Pick a file to read</div>' +
      '<div><kbd>⌘P</kbd> go to file · <code>/search</code> text · click a path in the chat</div></div>';
    return;
  }
  const slash = tab.path.lastIndexOf('/') + 1;
  const rootName = state.roots.length > 1 ? `${state.roots[tab.root]?.name ?? ''}/` : '';
  // The crumbs are right-to-left so long paths lose their start; the bidi mark keeps
  // the slashes where they belong.
  $('crumbs').innerHTML = `\u200e${esc(rootName + tab.path.slice(0, slash))}<b>${esc(tab.path.slice(slash))}</b>\u200e`;
  const file = code.files.get(fileKey(tab.root, tab.path));
  if (!file) {
    $('vmeta').textContent = '';
    wrap.innerHTML = '<div class="code-empty">Opening…</div>';
    return;
  }
  if (file.error) {
    $('vmeta').textContent = '';
    wrap.innerHTML = `<div class="code-empty"><div class="big">${esc(file.error)}</div></div>`;
    return;
  }
  $('vmeta').textContent = `${file.syntax ?? 'Plain text'} · ${file.lines.length} lines`;
  const top = wrap.scrollTop;
  const rows = file.lines.map((l, i) => `<div class="cl"><span class="ln">${i + 1}</span><span class="lc">${l || ' '}</span></div>`);
  wrap.innerHTML = `<div class="codeview">${rows.join('')}</div>`;
  paintSelection();
  if (code.pendingLine) {
    const row = wrap.firstChild.children[code.pendingLine - 1];
    if (row) {
      row.scrollIntoView({ block: 'center' });
      row.classList.add('flash');
    }
    code.pendingLine = null;
  } else if (keepScroll) wrap.scrollTop = top;
  else wrap.scrollTop = 0;
}

function paintSelection() {
  const view = $('codewrap').querySelector('.codeview');
  if (!view) return;
  for (const row of view.querySelectorAll('.cl.sel')) row.classList.remove('sel');
  const ask = $('ask');
  if (!code.sel) {
    ask.hidden = true;
    return;
  }
  const [a, b] = [Math.min(code.sel.a, code.sel.b), Math.max(code.sel.a, code.sel.b)];
  for (let n = a; n <= b; n++) view.children[n - 1]?.classList.add('sel');
  ask.hidden = false;
  ask.innerHTML = `Ask about ${a === b ? `line ${a}` : `lines ${a}–${b}`} <kbd>⌘⏎</kbd>`;
}

// Line numbers select: click a line, shift-click or drag for a range.
{
  const wrap = $('codewrap');
  let dragging = false;
  const lineOf = (target) => {
    const row = target.closest?.('.cl');
    return row ? [...row.parentNode.children].indexOf(row) + 1 : 0;
  };
  wrap.addEventListener('mousedown', (ev) => {
    if (!ev.target.classList.contains('ln')) return;
    ev.preventDefault();
    const n = lineOf(ev.target);
    if (ev.shiftKey && code.sel) code.sel.b = n;
    else if (code.sel && code.sel.a === n && code.sel.b === n) code.sel = null;
    else code.sel = { a: n, b: n };
    dragging = !!code.sel;
    paintSelection();
  });
  wrap.addEventListener('mouseover', (ev) => {
    if (!dragging) return;
    const n = lineOf(ev.target);
    if (n && n !== code.sel.b) {
      code.sel.b = n;
      paintSelection();
    }
  });
  addEventListener('mouseup', () => (dragging = false));
}

/** Quotes the selected lines into the prompt, the way the terminal's diff viewer does. */
function askAboutSelection() {
  const tab = activeTab();
  const view = $('codewrap').querySelector('.codeview');
  if (!tab || !code.sel || !view) return;
  const [a, b] = [Math.min(code.sel.a, code.sel.b), Math.max(code.sel.a, code.sel.b)];
  const lines = [];
  for (let n = a; n <= b; n++) lines.push(view.children[n - 1]?.querySelector('.lc').textContent ?? '');
  const ext = tab.path.includes('.') ? tab.path.split('.').pop() : '';
  const root = state.roots[tab.root];
  const path = tab.root === 0 ? tab.path : `${root.path}/${tab.path}`;
  const range = a === b ? `line ${a}` : `lines ${a}–${b}`;
  appendToInput(`In \`${path}\` (${range}):\n\`\`\`${ext}\n${lines.join('\n').replace(/\u00a0/g, ' ')}\n\`\`\`\n\n`);
}
$('ask').onclick = askAboutSelection;

$('copy-path').onclick = () => {
  const tab = activeTab();
  if (tab) copyText(tab.path, `Copied \`${tab.path}\`.`);
};

// ---------------------------------------------------------------- go to file

let pickItems = [];
let pickAt = 0;

function openPicker(query = '') {
  $('picker').hidden = false;
  const box = $('pick-input');
  box.value = query;
  box.focus();
  pickAt = 0;
  renderPicker();
}
const closePicker = () => {
  $('picker').hidden = true;
};

function renderPicker() {
  const q = $('pick-input').value.trim();
  if (!state.treeLoaded) {
    $('pick-list').innerHTML = '<div class="pickempty">Loading files…</div>';
    return;
  }
  // Without a query: the open files, most recent first.
  pickItems = q ? searchFiles(q, 60) : [...code.tabs].reverse().map((t) => ({ ...t, hits: [] }));
  pickAt = Math.min(pickAt, Math.max(0, pickItems.length - 1));
  $('pick-list').innerHTML = pickItems.length
    ? pickItems
        .map((f, i) => {
          const { name, dir } = fileLabel(f);
          return `<button class="pickrow${i === pickAt ? ' on' : ''}" data-i="${i}"><span>${name}</span><span class="pickdetail">${dir}</span></button>`;
        })
        .join('')
    : `<div class="pickempty">${q ? 'No files match.' : 'Type to find a file.'}</div>`;
  $('pick-list').children[pickAt]?.scrollIntoView({ block: 'nearest' });
}

function pick(i) {
  const f = pickItems[i];
  if (!f) return;
  closePicker();
  openFile(f.root, f.path);
}

$('pick-input').addEventListener('input', () => {
  pickAt = 0;
  renderPicker();
});
$('pick-input').addEventListener('keydown', (ev) => {
  if (ev.key === 'ArrowDown' || ev.key === 'ArrowUp' || (ev.ctrlKey && (ev.key === 'n' || ev.key === 'p'))) {
    ev.preventDefault();
    const down = ev.key === 'ArrowDown' || ev.key === 'n';
    pickAt = (pickAt + (down ? 1 : -1) + pickItems.length) % Math.max(1, pickItems.length);
    renderPicker();
  } else if (ev.key === 'Enter') {
    ev.preventDefault();
    pick(pickAt);
  }
});
$('pick-list').addEventListener('click', (ev) => {
  const row = ev.target.closest('.pickrow');
  if (row) pick(Number(row.dataset.i));
});
$('pick-list').addEventListener('mousemove', (ev) => {
  const row = ev.target.closest('.pickrow');
  if (row && Number(row.dataset.i) !== pickAt) {
    pickAt = Number(row.dataset.i);
    for (const r of $('pick-list').children) r.classList.toggle('on', r === row);
  }
});
for (const mask of document.querySelectorAll('.pickmask')) {
  mask.addEventListener('mousedown', (ev) => {
    if (ev.target === mask) mask.hidden = true;
  });
}

// ---------------------------------------------------------------- help

const KEYS = [
  ['⏎ / ⇧⏎', 'Send / new line (while the agent runs, ⏎ queues for its next step)'],
  ['esc', 'Interrupt the agent; close a menu, the picker or the code browser'],
  ['↑ / ↓', 'Earlier prompts, from the first or last line'],
  ['⌘P / Ctrl+P', 'Go to file'],
  ['⌘B / Ctrl+B', 'Show or hide the code browser'],
  ['⌘⏎ / Ctrl+⏎', 'Ask about the selected lines (click line numbers; shift-click or drag for a range)'],
  ['y / a / n', 'Answer a tool approval (when not typing)'],
  ['/', 'Start a command from anywhere'],
];

function openHelp() {
  const row = (k, v) => `<div class="helprow"><code>${esc(k)}</code><span>${esc(v)}</span></div>`;
  $('help-body').innerHTML =
    '<h2>tamale on the web</h2><p>The same session as the terminal, chat first. Paths in replies and tool calls open in the code browser.</p>' +
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
  if (mod && ev.key.toLowerCase() === 'p' && !ev.shiftKey) {
    ev.preventDefault();
    return openPicker();
  }
  if (mod && ev.key.toLowerCase() === 'b') {
    ev.preventDefault();
    return toggleCode();
  }
  if (mod && ev.key === 'Enter' && code.open && code.sel) {
    ev.preventDefault();
    return askAboutSelection();
  }
  if (ev.key === 'Escape') {
    if (!$('help').hidden) return ($('help').hidden = true);
    if (!$('picker').hidden) return closePicker();
    if (state.status?.running && !ev.target.closest?.('.code')) {
      ev.preventDefault();
      return send({ t: 'interrupt' });
    }
    if (code.open && ev.target.closest?.('.code')) {
      if (code.sel) {
        code.sel = null;
        return paintSelection();
      }
      return closeCode();
    }
    return;
  }
  if (approvalKey(ev)) return;
  // Typing anywhere that isn't a field goes to the prompt.
  const t = ev.target;
  if (t.matches?.('input, textarea') || mod || ev.altKey || ev.key.length !== 1) return;
  if (!$('picker').hidden || !$('help').hidden) return;
  input.focus();
});

// ---------------------------------------------------------------- start

if (store.get('codeOpen', false)) openCode();
renderView();
autosize();
updateSend();
connect();
