// The hub's own page. On / it lists the sessions under the view tokens this browser
// holds; on /open/<sid> it trades them for a ticket into that session and goes there.
// Session labels come from agents, so everything here is built with textContent.

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

function showError(message) {
  $('error').textContent = message || '';
  $('error').hidden = !message;
}

// A view link carries its token in the fragment, which never reaches the server or its
// logs; it goes to /api/tokens, which keeps it in an HttpOnly cookie.
async function takeFragment() {
  const token = decodeURIComponent(location.hash.slice(1));
  if (!token.startsWith('thv_') && !token.startsWith('tha_')) return;
  history.replaceState(null, '', location.pathname + location.search);
  const { ok, data } = await api('/api/tokens', { add: token });
  if (!ok) showError(data.error || 'That token was not accepted.');
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

async function refresh() {
  const { ok, data } = await api('/api/sessions');
  if (!ok) return;
  const has = data.tokens > 0;
  $('list').hidden = !has;
  $('forget').hidden = !has;
  $('pairs').textContent = has ? data.pairs.join(' · ') : '';
  $('add-lead').textContent = has
    ? 'Add another view token:'
    : 'Open your view link, or paste its view token (thv_…) here.';
  const list = $('sessions');
  list.replaceChildren(...data.sessions.map((s) => sessionItem(s, data.pairs.length > 1)));
  $('empty').hidden = data.sessions.length > 0;
}

async function open(sid) {
  $('opening').hidden = false;
  $('add').hidden = true;
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
  showError(ok ? '' : data.error || 'That token was not accepted.');
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
