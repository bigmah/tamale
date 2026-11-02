// The hub's own page.
//   /            a landing page for visitors; the session list and token pairs for
//                browsers that hold view tokens
//   /open/<sid>  trades the view tokens for a ticket into that session and goes there
// Session labels come from agents and pair names from whoever made them, so everything
// here is built with textContent.

const $ = (id) => document.getElementById(id);

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

// A view link carries its token in the fragment, which never reaches the server or its
// logs; it goes to /api/tokens, which keeps it in an HttpOnly cookie.
async function takeFragment() {
  const token = decodeURIComponent(location.hash.slice(1));
  if (!token.startsWith('thv_') && !token.startsWith('tha_')) return;
  history.replaceState(null, '', location.pathname + location.search);
  const { ok, data } = await api('/api/tokens', { add: token });
  if (!ok) showError('error', data.error || 'That token was not accepted.');
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

function sessionItem(s, showPair) {
  const link = el('a', 'session' + (s.online ? '' : ' offline'));
  link.href = s.open;
  const top = el('div', 'row');
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
  if (showPair) {
    meta.append(document.createTextNode(' '));
    meta.append(el('span', 'tag', s.pair));
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

function pairItem(pair, sessions) {
  const item = el('li', 'pair');
  const name = el('span', 'pair-name', pair.name);
  const count = sessions.filter((s) => s.pair === pair.name && s.online).length;
  name.append(el('span', 'pair-meta', `${made(pair.created)} · ${count} connected`));
  const button = (label, className, onClick) => {
    const b = el('button', `small ${className}`, label);
    b.type = 'button';
    b.addEventListener('click', onClick);
    return b;
  };
  item.append(
    name,
    button('New agent token', '', () => rotate(pair.name, 'agent')),
    button('New view link', '', () => rotate(pair.name, 'view')),
    button('Delete', 'danger', () => remove(pair.name)),
  );
  return item;
}

let busy = false;

async function refresh() {
  if (busy) return;
  const { ok, data } = await api('/api/sessions');
  if (!ok) return;
  const has = data.tokens > 0;
  const showingTokens = !$('made').hidden;
  $('landing').hidden = has || showingTokens;
  $('list').hidden = !has;
  $('manage').hidden = !has;
  $('forget').hidden = !has;
  $('create').hidden = !data.signup;
  $('create-title').textContent = has ? 'Make another token pair' : 'Make a token pair';
  $('add-title').textContent = has ? 'Add a view link' : 'Have a view link?';
  $('pairs').textContent = has ? data.pairs.map((p) => p.name).join(' · ') : '';
  $('sessions').replaceChildren(...data.sessions.map((s) => sessionItem(s, data.pairs.length > 1)));
  $('empty').hidden = data.sessions.length > 0;
  $('pair-list').replaceChildren(...data.pairs.map((p) => pairItem(p, data.sessions)));
}

// Shows tokens that were just made; they aren't shown again.
function showTokens(data, title) {
  $('made-title').textContent = title;
  $('made-agent').hidden = !data.agent;
  $('made-view').hidden = !data.view_link;
  $('made-view-title').textContent = data.agent ? '2. On your phone' : 'On your phone';
  if (data.env) $('made-env').textContent = data.env;
  if (data.view_link) {
    $('made-link').textContent = data.view_link;
    $('made-qr').src = 'data:image/svg+xml,' + encodeURIComponent(data.qr);
  }
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
  const { ok, data } = await api('/api/pairs', { label: $('label').value.trim() || null });
  busy = false;
  button.disabled = false;
  showError('create-error', ok ? '' : data.error || "Couldn't make a token pair.");
  if (!ok) return;
  $('label').value = '';
  showTokens(data, `Your token pair: ${data.name}`);
  refresh();
});

async function rotate(name, half) {
  const what = half === 'agent'
    ? `Make a new agent token for ${name}? The old one stops working, so every machine using it needs the new one.`
    : `Make a new view link for ${name}? Every other browser and phone using the old one loses access until you scan the new one.`;
  if (!confirm(what)) return;
  const { ok, data } = await api('/api/pairs/rotate', { name, half });
  if (!ok) {
    alert(data.error || "Couldn't rotate that.");
    return;
  }
  showTokens(data, half === 'agent' ? `New agent token for ${name}` : `New view link for ${name}`);
  refresh();
}

async function remove(name) {
  if (!confirm(`Delete the pair ${name}? Its sessions disconnect, and both tokens stop working everywhere.`)) return;
  const { ok, data } = await api('/api/pairs/delete', { name });
  if (!ok) alert(data.error || "Couldn't delete that.");
  refresh();
}

async function open(sid) {
  $('opening').hidden = false;
  for (const id of ['add', 'trust', 'create']) $(id).hidden = true;
  const { ok, data } = await api('/api/ticket', { sid });
  if (ok && data.url) {
    location.replace(data.url);
    return;
  }
  $('opening-text').textContent = data.error || "Couldn't open that session.";
  $('opening-back').hidden = false;
  if (data.error && data.error.includes('no view token')) $('add').hidden = false;
}

$('add-form').addEventListener('submit', async (ev) => {
  ev.preventDefault();
  const token = $('token').value.trim();
  if (!token) return;
  const { ok, data } = await api('/api/tokens', { add: token });
  showError('error', ok ? '' : data.error || 'That token was not accepted.');
  if (ok) $('token').value = '';
  if (ok && location.pathname.startsWith('/open/')) location.reload();
  else refresh();
});

$('forget').addEventListener('click', async () => {
  if (!confirm('Forget the view tokens on this browser? You will need the view link again.')) return;
  await api('/api/tokens', { forget: true });
  refresh();
});

await takeFragment();
const opening = location.pathname.match(/^\/open\/([0-9a-f]{32})$/);
if (opening) {
  open(opening[1]);
} else {
  refresh();
  setInterval(() => { if (!document.hidden) refresh(); }, 5000);
  document.addEventListener('visibilitychange', () => { if (!document.hidden) refresh(); });
}
