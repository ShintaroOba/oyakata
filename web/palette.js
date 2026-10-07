/* OYAKATA palette, modelled on VS Code's quick open: Ctrl+P finds files in the current
   repository, ">" runs commands (Ctrl+Shift+P / F1), ":" goes to a line in the open editor
   (Ctrl+G), "@" opens a session. Also used as a picker (definition candidates…). */
(() => {
  'use strict';
  const { $, $$, esc, state, toast, basename, norm, joinPath, sessionTitle, ago } = OY;

  const commands = [];
  let el = null;
  let input = null;
  let list = null;
  let items = [];
  let sel = 0;
  let fixed = null;          // { title, items } when used as a picker
  let returnFocus = null;
  let seq = 0;
  let inflight = null;

  /// Fuzzy subsequence match. Returns { score, hits } or null; rewards consecutive runs and
  /// matches at word starts.
  function fuzzy(q, s) {
    if (!q) return { score: 0, hits: [] };
    const ql = q.toLowerCase();
    const sl = s.toLowerCase();
    const hits = [];
    let score = 0;
    let j = 0;
    let prev = -2;
    for (let i = 0; i < sl.length && j < ql.length; i++) {
      if (sl[i] !== ql[j]) continue;
      const start = i === 0 || /[\\/_\-.\s]/.test(s[i - 1]) || (s[i] !== sl[i] && s[i - 1] === sl[i - 1]);
      score += 1 + (i === prev + 1 ? 5 : 0) + (start ? 8 : 0);
      hits.push(i);
      prev = i;
      j++;
    }
    if (j < ql.length) return null;
    score -= Math.min(20, s.length / 10);
    return { score, hits };
  }
  function hl(s, hits) {
    if (!hits?.length) return esc(s);
    const set = new Set(hits);
    let out = '';
    for (let i = 0; i < s.length; i++) out += set.has(i) ? `<b>${esc(s[i])}</b>` : esc(s[i]);
    return out;
  }

  function ensure() {
    if (el) return;
    el = document.createElement('div');
    el.id = 'palette';
    el.hidden = true;
    el.innerHTML = '<div class="pal-card"><div class="pal-title" hidden></div><input class="pal-input" spellcheck="false" autocomplete="off"><div class="pal-list"></div><div class="pal-foot"></div></div>';
    document.body.appendChild(el);
    input = $('.pal-input', el);
    list = $('.pal-list', el);
    el.addEventListener('mousedown', (e) => { if (e.target === el) close(); });
    input.addEventListener('input', () => refresh());
    input.addEventListener('keydown', (e) => {
      if (e.key === 'ArrowDown') { e.preventDefault(); move(1); }
      else if (e.key === 'ArrowUp') { e.preventDefault(); move(-1); }
      else if (e.key === 'PageDown') { e.preventDefault(); move(10); }
      else if (e.key === 'PageUp') { e.preventDefault(); move(-10); }
      // The list may still be loading for the latest keystroke; act on the finished one.
      else if (e.key === 'Enter') { e.preventDefault(); Promise.resolve(inflight).then(() => choose(sel)); }
      else if (e.key === 'Escape') { e.preventDefault(); e.stopPropagation(); close(); }
    });
    list.addEventListener('mousemove', (e) => { const r = e.target.closest('.pal-item'); if (r && +r.dataset.i !== sel) { sel = +r.dataset.i; paintSel(); } });
    list.addEventListener('click', (e) => { const r = e.target.closest('.pal-item'); if (r) choose(+r.dataset.i); });
  }
  function move(d) { if (!items.length) return; sel = Math.max(0, Math.min(items.length - 1, sel + d)); paintSel(); }
  function paintSel() {
    $$('.pal-item', list).forEach((r) => r.classList.toggle('sel', +r.dataset.i === sel));
    $(`.pal-item[data-i="${sel}"]`, list)?.scrollIntoView({ block: 'nearest' });
  }
  function render(foot = '') {
    list.innerHTML = items.length
      ? items.map((it, i) => `<div class="pal-item${i === sel ? ' sel' : ''}" data-i="${i}"><span class="pi-icon">${it.icon || ''}</span><span class="pi-main"><span class="pi-label">${it.labelHtml || esc(it.label)}</span>${it.detail ? `<span class="pi-detail">${it.detailHtml || esc(it.detail)}</span>` : ''}${it.hint ? `<span class="pi-hint">${esc(it.hint)}</span>` : ''}</span>${it.right ? `<span class="pi-right">${esc(it.right)}</span>` : ''}</div>`).join('')
      : `<div class="pal-empty">${esc(foot || '該当なし')}</div>`;
    $('.pal-foot', el).textContent = items.length ? foot : '';
  }
  function close() {
    if (!el || el.hidden) return;
    el.hidden = true;
    fixed = null;
    try { returnFocus?.focus?.(); } catch { /* gone */ }
  }
  function choose(i) {
    const it = items[i];
    if (!it) return;
    if (it.keep) { it.run?.(); return; }
    close();
    setTimeout(() => it.run?.(), 0);
  }
  function show(value, title) {
    ensure();
    if (el.hidden) returnFocus = document.activeElement;
    el.hidden = false;
    $('.pal-title', el).hidden = !title;
    $('.pal-title', el).textContent = title || '';
    input.value = value;
    input.focus();
    input.setSelectionRange(value.length, value.length);
    sel = 0;
    refresh();
  }
  /// Open in a mode by prefix: '' files, '>' commands, ':' line, '@' sessions.
  function open(prefix = '') { fixed = null; show(prefix, ''); input.placeholder = 'ファイル名で検索（> コマンド  : 行へ移動  @ セッション）'; }
  /// Use the palette as a picker over fixed items.
  function pick({ title, items: list2, placeholder = '絞り込む' }) { fixed = { items: list2 }; show('', title); input.placeholder = placeholder; }

  function refresh() { inflight = doRefresh(); return inflight; }
  async function doRefresh() {
    const v = input.value;
    const my = ++seq;
    sel = 0;
    if (fixed) {
      items = rankBy(fixed.items, v, (it) => `${it.label} ${it.detail || ''}`, 200);
      render();
      return;
    }
    if (v.startsWith('>')) { items = commandItems(v.slice(1).trim()); render('コマンドが見つかりません'); return; }
    if (v.startsWith(':')) { items = lineItems(v.slice(1).trim()); render('ファイルを開いていると、行番号で移動できます'); return; }
    if (v.startsWith('@')) { items = sessionItems(v.slice(1).trim()); render('セッションが見つかりません'); return; }
    const root = state.activeRepo;
    if (!root) { items = []; render('リポジトリを選んでください'); return; }
    let files;
    try { files = await OY.code.files(root); } catch (e) { if (my === seq) { items = []; render(e.message.includes('not a git') ? 'Git リポジトリではないため、ファイル検索は使えません' : e.message); } return; }
    if (my !== seq) return;
    items = fileItems(files, v.trim(), root);
    render(`${OY.state.repos.find((r) => norm(r.root) === norm(root))?.name || basename(root)} · ${files.length.toLocaleString()} ファイル`);
  }
  function rankBy(arr, q, text, limit) {
    if (!q) return arr.slice(0, limit);
    const out = [];
    for (const it of arr) {
      const m = fuzzy(q, text(it));
      if (m) out.push({ ...it, _s: m.score, labelHtml: hl(it.label, fuzzy(q, it.label)?.hits) });
    }
    return out.sort((a, b) => b._s - a._s).slice(0, limit);
  }

  // ---------------------------------------------------------------- files
  const MRU_MAX = 30;
  function mru(root) { return OY.LS.get('mru', {})[norm(root)] || []; }
  function touchMru(root, rel) {
    const all = OY.LS.get('mru', {});
    const k = norm(root);
    all[k] = [rel, ...(all[k] || []).filter((x) => x !== rel)].slice(0, MRU_MAX);
    OY.LS.set('mru', all);
  }
  function fileItems(files, raw, root) {
    const m = /^(.*?)(?::(\d+))?$/.exec(raw);
    const q = (m[1] || '').replace(/\\/g, '/');
    const line = m[2] ? +m[2] : null;
    const mk = (f, labelHits, detailHits) => {
      const dir = f.includes('/') ? f.slice(0, f.lastIndexOf('/')) : '';
      const base = f.slice(dir ? dir.length + 1 : 0);
      return { icon: '📄', label: base, labelHtml: hl(base, labelHits), detail: dir, detailHtml: hl(dir, detailHits), run: () => { touchMru(root, f); OY.code.jump({ path: joinPath(root, f), root, line }); } };
    };
    if (!q) {
      const recent = mru(root).filter((f) => files.includes(f));
      const shown = recent.length ? recent : files.slice(0, 40);
      return shown.map((f) => ({ ...mk(f), right: recent.length ? '最近' : '' }));
    }
    const scored = [];
    const hasSlash = q.includes('/');
    for (const f of files) {
      const base = f.slice(f.lastIndexOf('/') + 1);
      const b = hasSlash ? null : fuzzy(q, base);
      const p = b ? null : fuzzy(q, f);
      if (!b && !p) continue;
      const score = b ? 1000 + b.score - base.length / 50 : p.score;
      scored.push({ f, score, b, p });
    }
    scored.sort((a, c) => c.score - a.score);
    return scored.slice(0, 60).map(({ f, b, p }) => {
      const dir = f.includes('/') ? f.slice(0, f.lastIndexOf('/')) : '';
      // Map full-path hits to the dir / base parts for highlighting.
      const off = dir ? dir.length + 1 : 0;
      const dh = p ? p.hits.filter((i) => i < dir.length) : [];
      const bh = b ? b.hits : p ? p.hits.filter((i) => i >= off).map((i) => i - off) : [];
      return mk(f, bh, dh);
    });
  }

  // ------------------------------------------------------------- commands
  function register(cmd) { commands.push(cmd); }
  function commandItems(q) {
    const avail = commands.filter((c) => !c.when || c.when());
    return rankBy(avail.map((c) => ({ icon: c.icon || '›', label: c.label, right: c.keys || '', run: c.run })), q, (it) => it.label, 100);
  }
  function lineItems(q) {
    const t = OY.wb.activeTab();
    if (t?.desc?.kind !== 'file' || !t.inst?.reveal) return [];
    const n = parseInt(q, 10);
    const total = t.inst.lineCount?.() || 0;
    if (!n) return [{ icon: '↧', label: `行番号を入力（1〜${total}）`, run: () => {}, keep: true }];
    return [{ icon: '↧', label: `${n} 行目へ移動`, detail: t.desc.title, run: () => { const loc = OY.code.here(); if (loc) OY.code.jump({ ...loc, line: Math.min(n, total || n), col: 1 }); } }];
  }
  function sessionItems(q) {
    const list2 = state.sessions.filter((s) => s.user_turns > 0 || s.status !== 'ended').map((s) => ({
      icon: s.status === 'busy' ? '🟠' : s.status === 'waiting' ? '🟣' : s.status === 'idle' ? '🟢' : '💬',
      label: sessionTitle(s), detail: `${s.repo?.name || s.cwd || ''} · ${ago(s.last_at)}`, run: () => OY.chat.open(s.id),
    }));
    return rankBy(list2, q, (it) => `${it.label} ${it.detail}`, 80);
  }

  OY.palette = { open, list: pick, close, register, fuzzy, touchMru, isOpen: () => !!el && !el.hidden };
})();
