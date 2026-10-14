'use strict';
// kuggis web: your open PRs with the Thinker's word on each, and your sessions.
// Text from a PR, the Thinker or a VM goes into the page as text, never HTML,
// and invisible or direction-changing characters show as U+XXXX.

const store = {
  get(k) { try { return localStorage.getItem(k); } catch { return null; } },
  set(k, v) { try { v == null ? localStorage.removeItem(k) : localStorage.setItem(k, v); } catch { /* private window */ } },
};

// BigBrain's themes (theme.css; ids and names from its lib/theme.ts). No
// data-theme is Ink following the system; a pair follows it too.
const THEMES = [
  ['', 'Ink · follows the system'],
  ['pair:kind-of-blue', 'Kind of Blue · follows the system'],
  ['pair:somethings-gotta-give', "Something's Gotta Give · follows the system"],
  ['default', 'Ink, light'], ['dusk', 'Ink, dark'],
  ['kind-of-blue-light', 'Kind of Blue, light'], ['kind-of-blue-dark', 'Kind of Blue, dark'],
  ['somethings-gotta-give', "Something's Gotta Give, light"], ['hamptons-nights', "Hampton's Nights"],
  ['web', 'OG Web Blue'], ['moegiiro', 'Spring'], ['adzukiiro', 'Fall'], ['asagiiro', 'Winter'],
  ['jodie-foster-in-contact', 'Jodie Foster in Contact'], ['a-love-supreme', 'A Love Supreme'],
];
const PAIRS = { 'kind-of-blue': ['kind-of-blue-light', 'kind-of-blue-dark'], 'somethings-gotta-give': ['somethings-gotta-give', 'hamptons-nights'] };
const darkMode = matchMedia('(prefers-color-scheme: dark)');
let themeChoice = store.get('kuggis.theme') || '';
function applyTheme() {
  const pair = themeChoice.startsWith('pair:') && PAIRS[themeChoice.slice(5)];
  const id = pair ? pair[darkMode.matches ? 1 : 0] : themeChoice;
  if (id) document.documentElement.dataset.theme = id;
  else delete document.documentElement.dataset.theme;
}
applyTheme(); // before first paint: this script loads in <head>
darkMode.addEventListener('change', applyTheme);

// The token arrives once in the #fragment (kuggis web opens that link) and is
// kept here, out of the address bar.
const token = (() => {
  const h = location.hash.slice(1);
  if (/^[0-9a-f]{32,}$/.test(h)) {
    store.set('kuggis.token', h);
    history.replaceState(null, '', '/');
    return h;
  }
  return store.get('kuggis.token') || '';
})();

const PLAIN = /^[A-Za-z0-9][A-Za-z0-9_-]*$/; // what a session or project name may be
const HIDDEN = /[­​-‏‪-‮⁠-⁤﻿]/g;
const STATE_EVERY = 15000, LOG_EVERY = 3000;

let state = null;        // {projects, prs, sessions}
let rows = [];           // what the list shows, in order
let selKey = store.get('kuggis.sel');
let detail = null;       // {key, data, error}
let logTimer = null, stateTimer = null, detailTimer = null;
let busy = false;        // an action is running

// ── small helpers ────────────────────────────────────────────────────────────
function el(tag, cls, ...kids) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  for (const k of kids) if (k != null && k !== false) e.append(k);
  return e;
}

// Text, with invisible and direction-changing characters made visible.
function visible(s) {
  const f = document.createDocumentFragment();
  let last = 0;
  for (const m of String(s).matchAll(HIDDEN)) {
    f.append(s.slice(last, m.index));
    f.append(el('span', 'hidden-char', 'U+' + m[0].codePointAt(0).toString(16).toUpperCase().padStart(4, '0')));
    last = m.index + m[0].length;
  }
  f.append(String(s).slice(last));
  return f;
}

function inline(node, s) { // `code` and **bold**; the rest is plain text
  const re = /(`[^`]+`|\*\*[^*]+\*\*)/g;
  let last = 0, m;
  while ((m = re.exec(s))) {
    node.append(visible(s.slice(last, m.index)));
    const code = m[0][0] === '`';
    node.append(el(code ? 'code' : 'strong', null, visible(code ? m[0].slice(1, -1) : m[0].slice(2, -2))));
    last = re.lastIndex;
  }
  node.append(visible(s.slice(last)));
  return node;
}

// The review's markdown, enough of it to read well: headings, lists, code,
// tables as preformatted text. Links stay text.
function markdown(text) {
  const out = document.createDocumentFragment();
  const lines = text.split('\n');
  let para = [], list = null;
  const flush = () => { if (para.length) out.append(inline(el('p'), para.join(' '))); para = []; };
  for (let i = 0; i < lines.length; i++) {
    const l = lines[i];
    let m;
    if (/^\s*```/.test(l)) {
      flush(); list = null;
      const buf = [];
      while (++i < lines.length && !/^\s*```/.test(lines[i])) buf.push(lines[i]);
      out.append(el('pre', null, visible(buf.join('\n'))));
    } else if (/^\s*\|/.test(l)) {
      flush(); list = null;
      const buf = [l];
      while (i + 1 < lines.length && /^\s*\|/.test(lines[i + 1])) buf.push(lines[++i]);
      out.append(el('pre', 'table', visible(buf.join('\n'))));
    } else if ((m = /^(#{1,6})\s+(.*)/.exec(l))) {
      flush(); list = null;
      out.append(inline(el('h' + Math.min(6, m[1].length + 2)), m[2]));
    } else if ((m = /^\s*([-*+]|\d+[.)])\s+(.*)/.exec(l))) {
      flush();
      if (!list) out.append(list = el(/\d/.test(m[1]) ? 'ol' : 'ul'));
      list.append(inline(el('li'), m[2]));
    } else if (!l.trim()) {
      flush(); list = null;
    } else if (list && /^\s+\S/.test(l)) {
      inline(list.lastChild, ' ' + l.trim());
    } else {
      list = null;
      para.push(l.trim());
    }
  }
  flush();
  return out;
}

function ago(t) {
  const m = Math.max(0, Math.floor((Date.now() / 1000 - t) / 60));
  return m < 60 ? `${m}m` : `${Math.floor(m / 60)}h${String(m % 60).padStart(2, '0')}`;
}
const clock = t => new Date(t * 1000).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
const prKey = pr => `pr:${pr.repo}#${pr.n}`;

// xterm-256 colour → CSS, for the project colours the tmux bars use.
function xterm(i) {
  const v = c => (c ? 55 + 40 * c : 0);
  i -= 16;
  return `rgb(${v(Math.floor(i / 36))}, ${v(Math.floor(i / 6) % 6)}, ${v(i % 6)})`;
}

async function api(path, body) {
  const headers = { 'X-Kuggis-Token': token };
  const r = await fetch(path, body
    ? { method: 'POST', headers: { ...headers, 'Content-Type': 'application/json' }, body: JSON.stringify(body) }
    : { headers });
  const j = await r.json().catch(() => ({ error: `HTTP ${r.status}` }));
  if (!r.ok) throw new Error(j.error || `HTTP ${r.status}`);
  return j;
}

let toastTimer;
function toast(msg, bad) {
  const t = document.getElementById('toast');
  t.replaceChildren(visible(msg));
  t.className = 'toast show' + (bad ? ' bad' : '');
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => (t.className = 'toast'), bad ? 9000 : 5000);
}

// ── the list ─────────────────────────────────────────────────────────────────
function badge(pr) {
  if (pr.reviewing_since) return el('span', 'badge reviewing', el('span', 'spin'), ` reviewing ${ago(pr.reviewing_since)}`);
  if (pr.empty) return el('span', 'badge bad', '✗ no review');
  if (!pr.verdict) return el('span', 'badge none', '· not reviewed');
  const [icon, cls] = { merge: ['✓', 'ok'], changes: ['✎', 'warn'], hold: ['‖', 'hold'] }[pr.verdict] || ['?', 'none'];
  return el('span', `badge ${cls}${pr.stale ? ' stale' : ''}`, visible(`${icon} ${pr.verdict}${pr.stale ? ' (old)' : ''}`));
}

const checksPill = c => el('span', `pill ci-${c}`, `ci ${c}`);

// The open PR this one is stacked on: it merges first.
const parentOf = pr => state.prs.find(p => p.repo === pr.repo && p.head === pr.base);

// A PR's worker is the session named for its branch. While it or the Thinker
// is busy, or a ready PR waits for the one it's stacked on, you're waiting;
// otherwise it's your move.
const workerOf = pr => state.sessions.find(s => s.project === pr.project && s.name === pr.head);
const atWork = s => /^(starting|working|running)/.test(s?.state || '');
const yourTurn = pr => !pr.reviewing_since && !atWork(workerOf(pr)) &&
  !(parentOf(pr) && pr.verdict === 'merge' && !pr.stale);

function workerChip(pr) {
  const w = workerOf(pr);
  if (!w) return el('span', 'badge worker none');
  const waiting = w.state.startsWith('waiting');
  const e = el('span', `badge worker ${atWork(w) ? 'busy' : waiting ? 'waiting' : 'none'}`,
    atWork(w) ? el('span', 'spin') : waiting ? '⚑' : '·', ' ', visible(w.name));
  e.title = `${w.name}: ${w.state}`;
  return e;
}

const SESSION_RANK = s => (s.state.startsWith('waiting') ? 0 : s.state.startsWith('working') ? 1 : 2);

function buildRows() {
  rows = [];
  for (const p of state.projects) {
    const prs = state.prs.filter(x => x.project === p.name);
    const ss = state.sessions.filter(x => x.project === p.name);
    rows.push({ kind: 'project', key: `project:${p.name}`, project: p });
    for (const pr of prs) rows.push({ kind: 'pr', key: prKey(pr), pr });
    if (ss.some(s => s.state === 'unreachable')) rows.push({ kind: 'vm', key: `vm:${p.name}`, project: p.name });
    for (const s of ss.filter(s => s.name !== '-').sort((a, b) => SESSION_RANK(a) - SESSION_RANK(b)))
      rows.push({ kind: 'session', key: `session:${s.project}/${s.name}`, s });
  }
}

const selectable = () => rows.filter(r => r.kind !== 'project');
const selected = () => rows.find(r => r.key === selKey);

function renderList() {
  const list = document.getElementById('list');
  const frag = document.createDocumentFragment();
  for (const r of rows) {
    let e;
    if (r.kind === 'project') {
      e = el('div', 'group', el('span', 'swatch'), visible(r.project.name));
      e.firstChild.style.background = xterm(r.project.colour);
    } else if (r.kind === 'pr') {
      const pr = r.pr;
      e = el('div', `row pr ${yourTurn(pr) ? 'yours' : 'waiting'}`, el('span', 'num', `#${pr.n}`), badge(pr), workerChip(pr), checksPill(pr.checks),
        el('span', 'title', visible(pr.title)));
    } else if (r.kind === 'session') {
      const s = r.s, waiting = s.state.startsWith('waiting');
      e = el('div', 'row session', el('span', 'num', '●'), el('span', 'name', visible(s.name)),
        el('span', `badge ${waiting ? 'waiting' : s.state.startsWith('working') ? 'working' : 'none'}`,
          visible((waiting ? '⚑ ' : '') + s.state)),
        el('span', 'age', visible(s.age)));
    } else {
      e = el('div', 'row vm', el('span', 'badge bad', "✗ the VM didn't answer"));
    }
    if (r.kind !== 'project') {
      e.dataset.key = r.key;
      if (r.key === selKey) e.classList.add('selected');
      e.addEventListener('click', () => { select(r.key); if (r.kind === 'session') enter(); });
    }
    frag.append(e);
  }
  if (!rows.length) frag.append(el('p', 'empty', 'No projects: kuggis new <project> <owner/repo>'));
  list.replaceChildren(frag);
}

function select(key, scroll) {
  if (key === selKey) return;
  selKey = key;
  store.set('kuggis.sel', key);
  for (const e of document.querySelectorAll('#list .row')) e.classList.toggle('selected', e.dataset.key === key);
  if (scroll) document.querySelector(`#list .row.selected`)?.scrollIntoView({ block: 'nearest' });
  clearTimeout(detailTimer);
  detailTimer = setTimeout(loadDetail, 150);
  renderDetail();
}

function move(d) {
  const s = selectable();
  if (!s.length) return;
  const i = s.findIndex(r => r.key === selKey);
  select(s[Math.max(0, Math.min(s.length - 1, i < 0 ? 0 : i + d))].key, true);
}

// ── the detail pane ──────────────────────────────────────────────────────────
async function loadDetail() {
  const r = selected();
  clearTimeout(logTimer);
  if (!r || r.kind !== 'pr') return renderDetail();
  const key = r.key;
  try {
    const data = await api(`/api/pr?repo=${encodeURIComponent(r.pr.repo)}&n=${r.pr.n}`);
    if (selKey !== key) return;
    detail = { key, data };
  } catch (e) {
    if (selKey !== key) return;
    detail = { key, error: e.message };
  }
  renderDetail();
  if (r.pr.reviewing_since) pollLog(r.pr);
}

// While the Thinker works on the PR you're looking at, follow its log.
function pollLog(pr) {
  clearTimeout(logTimer);
  logTimer = setTimeout(async () => {
    if (selKey !== prKey(pr)) return;
    try {
      const l = await api(`/api/log?repo=${encodeURIComponent(pr.repo)}&n=${pr.n}`);
      if (selKey !== prKey(pr) || !detail?.data) return;
      detail.data.log = l.log;
      if (l.reviewing_since) {
        renderDetail();
        return pollLog(pr);
      }
      await refresh(); // it finished: its review (or its absence) is news
    } catch { pollLog(pr); }
  }, LOG_EVERY);
}

function button(label, key, on, why) {
  const b = el('button', null, el('kbd', null, key), ' ', label);
  if (why) { b.disabled = true; b.title = why; } else b.addEventListener('click', on);
  return b;
}

// The PR's details (review, files, log), once they've loaded.
const loaded = pr => (detail?.key === prKey(pr) ? detail.data : null);

// What blocks an action on this PR, or null.
function blockers(pr) {
  const d = loaded(pr), up = parentOf(pr), loading = 'still loading this PR';
  return {
    ask: pr.reviewing_since ? 'the Thinker is reviewing it now' : null,
    merge: up ? `merge #${up.n} first` : !d ? loading : pr.base !== d.default_branch ? `merge ${pr.base} first` : null,
    send: !d ? loading : !d.review ? 'no review to send yet' : null,
  };
}

function renderDetail() {
  const pane = document.getElementById('detail');
  const r = selected();
  if (!r) return pane.replaceChildren(el('p', 'empty', 'Pick a PR or a session.'));
  if (r.kind === 'session') return pane.replaceChildren(sessionDetail(r.s));
  if (r.kind === 'vm') return pane.replaceChildren(el('div', 'banner bad', `${r.project}'s VM didn't answer ssh. It may be asleep or rebooting.`));
  const pr = r.pr, d = loaded(pr), b = blockers(pr), up = parentOf(pr);
  const frag = document.createDocumentFragment();
  frag.append(
    el('div', 'label', visible(`${pr.repo} · #${pr.n}`)),
    el('h1', null, visible(pr.title)),
    el('div', 'meta', visible(`by ${pr.author} · ${pr.head} → ${pr.base} · head ${pr.sha.slice(0, 12)} · `), checksPill(pr.checks), workerOf(pr) && workerChip(pr)),
    el('div', 'actions',
      button(pr.verdict || pr.empty ? 'Review again' : 'Ask the Thinker', 't', () => ask(pr), b.ask),
      button(up ? `Merge after #${up.n}` : 'Merge', 'm', () => mergeDialog(pr), b.merge),
      button('Send review to its session', 's', () => sendDialog(pr), b.send),
      button('GitHub', 'o', () => openPr(pr))));
  if (pr.reviewing_since) {
    frag.append(el('div', 'banner reviewing', el('span', 'spin'),
      ` The Thinker is reviewing this · since ${clock(pr.reviewing_since)} · ${ago(pr.reviewing_since)}`));
    frag.append(el('pre', 'log follow', visible(d?.log || 'starting…')));
  } else if (pr.empty) {
    frag.append(el('div', 'banner bad', "The Thinker's last run ended without a review of this commit"));
    if (d?.log) frag.append(el('pre', 'log follow', visible(d.log)));
  }
  if (detail?.key === r.key && detail.error) frag.append(el('div', 'banner bad', visible(detail.error)));
  else if (!d) frag.append(el('p', 'muted', 'loading…'));
  else {
    if (d.review) {
      frag.append(el('div', 'section-head',
        el('span', 'label', 'The Thinker'), badge(pr),
        el('span', 'muted', visible(`reviewed ${d.reviewed_head || 'an unknown commit'}${d.reviewed_at ? ' at ' + clock(d.reviewed_at) : ''}`))));
      if (pr.stale) frag.append(el('div', 'banner warn', visible(`This review is of an older commit: ${pr.stale}.`)));
      if (d.flags) frag.append(el('div', 'flags', el('div', 'label', 'Look at, before you send it on'), el('pre', null, visible(d.flags))));
      frag.append(el('article', 'review bb-prose', markdown(d.review)));
    } else if (!pr.reviewing_since && !pr.empty) {
      frag.append(el('p', 'muted', 'No review from the Thinker yet. t asks it for one; it takes about two minutes.'));
    }
    const p = d.pr;
    if (p) {
      const files = el('details', 'files', el('summary', null, el('span', 'label', 'Files'), ` ${p.files.length} · +${p.additions} −${p.deletions}`));
      for (const f of p.files) files.append(el('div', 'file', el('span', 'add', `+${f.additions}`), el('span', 'del', `−${f.deletions}`), visible(f.path)));
      frag.append(files);
    }
    if (d.log && !pr.reviewing_since && !pr.empty)
      frag.append(el('details', 'files', el('summary', null, el('span', 'label', 'Last run')), el('pre', 'log', visible(d.log))));
  }
  const top = pane.scrollTop;
  pane.replaceChildren(frag);
  pane.scrollTop = top;
  for (const p of pane.querySelectorAll('pre.follow')) p.scrollTop = p.scrollHeight; // newest last
}

const attachCommand = s => `kuggis ${s.project} ${s.name}`;

function sessionDetail(s) {
  const plain = PLAIN.test(s.project) && PLAIN.test(s.name);
  return el('div', null,
    el('div', 'label', visible(s.project)),
    el('h1', null, visible(s.name)),
    el('div', 'meta', visible(`${s.state} · ${s.age}`)),
    plain ? el('pre', 'command', attachCommand(s)) : el('div', 'banner bad', "This session's name isn't plain letters, digits, - and _, so kuggis won't put it on your clipboard."),
    el('p', 'muted', plain ? 'Enter copies it. Paste it into a terminal to attach.' : ''));
}

// ── actions ──────────────────────────────────────────────────────────────────
async function copy(text) {
  try { await navigator.clipboard.writeText(text); return true; } catch { /* fall through */ }
  const t = el('textarea');
  t.value = text;
  document.body.append(t);
  t.select();
  const ok = document.execCommand('copy');
  t.remove();
  return ok;
}

async function enter() {
  const r = selected();
  if (r?.kind !== 'session') return;
  if (!PLAIN.test(r.s.project) || !PLAIN.test(r.s.name)) return toast("not copied: the session's name isn't plain", true);
  const c = attachCommand(r.s);
  const ok = await copy(c);
  toast(ok ? `copied: ${c}` : `couldn't reach the clipboard: ${c}`, !ok);
}

async function act(path, body, pending) {
  if (busy) return;
  busy = true;
  toast(pending);
  try {
    const j = await api(path, body);
    toast(j.message || 'done');
  } catch (e) {
    toast(e.message, true);
  } finally {
    busy = false;
    await refresh();
  }
}

const ask = pr => act('/api/ask', { repo: pr.repo, n: pr.n }, `asking the Thinker to review #${pr.n}…`);
const openPr = pr => window.open(`https://github.com/${pr.repo}/pull/${pr.n}`, '_blank', 'noopener');

function confirmDialog(title, body, ok, run) {
  const dlg = document.getElementById('confirm');
  const yes = run && el('button', 'primary', el('kbd', null, 'y'), ' ', ok);
  const no = el('button', null, el('kbd', null, 'esc'), ' ', run ? 'Cancel' : 'Close');
  dlg.replaceChildren(el('h2', null, visible(title)), body, el('div', 'actions', no, yes));
  no.addEventListener('click', () => dlg.close());
  if (yes) yes.addEventListener('click', () => { dlg.close(); run(); });
  dlg.onkeydown = e => {
    if (e.key === 'y' && run) { e.preventDefault(); dlg.close(); run(); }
    if (e.key === 'n') { e.preventDefault(); dlg.close(); }
  };
  dlg.showModal();
  (yes || no).focus();
}

function mergeDialog(pr) {
  const b = blockers(pr);
  if (b.merge) return toast(b.merge);
  const warn = [];
  if (pr.checks === 'failing') warn.push('Checks are failing.');
  if (pr.checks === 'pending') warn.push('Checks are still running.');
  if (pr.reviewing_since) warn.push('The Thinker is still reviewing it.');
  else if (!pr.verdict) warn.push("The Thinker hasn't reviewed it.");
  else if (pr.stale) warn.push("The Thinker's review is of an older commit.");
  else if (pr.verdict !== 'merge') warn.push(`The Thinker says ${pr.verdict}.`);
  const body = el('div', null,
    el('p', null, visible(`${pr.repo} #${pr.n} at ${pr.sha.slice(0, 12)} into ${pr.base}. Only that commit merges: if the branch moved, GitHub refuses.`)),
    warn.length ? el('ul', 'warn', ...warn.map(w => el('li', null, w))) : null);
  confirmDialog(`Merge #${pr.n}?`, body, 'Merge', () =>
    act('/api/merge', { repo: pr.repo, n: pr.n, sha: pr.sha }, `merging #${pr.n}…`));
}

function sendDialog(pr) {
  const d = loaded(pr);
  if (!d?.review) return;
  const body = el('div', null,
    el('p', null, visible(`It goes to the session ${pr.project} / ${pr.head} as its next message, exactly as you see it here, asking it to address the review and push.`)),
    pr.stale ? el('p', 'warn', 'This review is of an older commit.') : null,
    el('div', 'label', 'Look at (anything that could carry data out)'),
    el('pre', null, visible(d.flags || 'nothing unusual')));
  confirmDialog(`Send the review of #${pr.n} to ${pr.head}?`, body, 'Send', () =>
    act('/api/send', { repo: pr.repo, n: pr.n, hash: d.hash }, `sending to ${pr.project} / ${pr.head}…`));
}

// ── refresh ──────────────────────────────────────────────────────────────────
async function refresh() {
  clearTimeout(stateTimer);
  const status = document.getElementById('status');
  status.textContent = 'refreshing…';
  try {
    state = await api('/api/state');
    buildRows();
    if (!selected()) selKey = selectable()[0]?.key || null;
    renderList();
    await loadDetail();
    status.textContent = `updated ${new Date().toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' })}`;
  } catch (e) {
    status.textContent = e.message;
  }
  stateTimer = setTimeout(refresh, STATE_EVERY);
}

function setupThemes() {
  const sel = document.getElementById('theme');
  for (const [id, name] of THEMES) {
    const o = el('option', null, name);
    o.value = id;
    sel.append(o);
  }
  sel.value = themeChoice;
  sel.addEventListener('change', () => {
    themeChoice = sel.value;
    store.set('kuggis.theme', themeChoice || null);
    applyTheme();
    sel.blur();
  });
}

document.addEventListener('keydown', e => {
  if (e.metaKey || e.ctrlKey || e.altKey || document.getElementById('confirm').open) return;
  if (e.target.closest?.('select, input, textarea')) return;
  const r = selected(), pr = r?.kind === 'pr' ? r.pr : null, b = pr && blockers(pr);
  const on = {
    j: () => move(1), ArrowDown: () => move(1), k: () => move(-1), ArrowUp: () => move(-1),
    Enter: enter, r: refresh,
    t: () => pr && (b.ask ? toast(b.ask) : ask(pr)),
    m: () => pr && mergeDialog(pr),
    s: () => pr && (b.send ? toast(b.send) : sendDialog(pr)),
    o: () => pr && openPr(pr),
    ' ': () => document.getElementById('detail').scrollBy({ top: (e.shiftKey ? -0.8 : 0.8) * innerHeight }),
  }[e.key];
  if (on) { e.preventDefault(); on(); }
});

document.addEventListener('DOMContentLoaded', () => {
  setupThemes();
  if (!token) {
    document.getElementById('detail').replaceChildren(el('div', 'banner bad', 'No token: open this page with the link kuggis web prints.'));
    return;
  }
  refresh();
});
document.addEventListener('visibilitychange', () => { if (!document.hidden) refresh(); });
