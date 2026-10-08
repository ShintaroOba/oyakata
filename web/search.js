/* OYAKATA search view (Ctrl+Shift+F): full-text search over the files of the current
   repository (git grep, with case / whole word / regex switches and a path filter), or over
   everything said in every conversation. Results open the file at the line, or the session at
   the message. */
(() => {
  'use strict';
  const { $, $$, esc, api, state, bus, LS, toast, norm, basename, joinPath, sessionTitle, ago } = OY;

  const sv = {
    cs: LS.get('sv.case', false),
    word: LS.get('sv.word', false),
    regex: LS.get('sv.regex', false),
    scope: LS.get('sv.scope', 'files'),
    seq: 0,
    timer: null,
    collapsed: new Set(),
    lastRoot: null,
  };

  function paintOpts() {
    for (const b of $$('#sb-search .sv-opt')) b.classList.toggle('on', !!sv[b.dataset.opt]);
    $('#sv-scope').value = sv.scope;
    const files = sv.scope === 'files';
    $('#sv-glob').hidden = !files;
    $$('#sb-search .sv-opt').forEach((b) => { b.disabled = !files && b.dataset.opt !== 'cs'; });
  }
  /// The RegExp used to highlight hits in result lines (mirrors the search options).
  function marker(q) {
    try {
      const src = sv.scope === 'files' && sv.regex ? q : q.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
      const body = sv.scope === 'files' && sv.word ? `\\b(?:${src})\\b` : src;
      return new RegExp(body, sv.cs && sv.scope === 'files' ? 'g' : 'gi');
    } catch { return null; }
  }
  function highlight(text, re) {
    if (!re) return esc(text);
    let out = '';
    let last = 0;
    re.lastIndex = 0;
    let m;
    let n = 0;
    while ((m = re.exec(text)) && n++ < 50) {
      if (!m[0].length) { re.lastIndex++; continue; }
      out += esc(text.slice(last, m.index)) + `<mark>${esc(m[0])}</mark>`;
      last = m.index + m[0].length;
    }
    return out + esc(text.slice(last));
  }
  /// Column (1-based, in characters) of the first hit, for placing the cursor.
  function firstCol(text, re) {
    if (!re) return 1;
    re.lastIndex = 0;
    const m = re.exec(text);
    return m ? m.index + 1 : 1;
  }

  async function exec() {
    const q = $('#sv-q').value;
    const out = $('#sv-results');
    const sum = $('#sv-summary');
    clearTimeout(sv.timer);
    if (!q.trim()) { out.innerHTML = ''; sum.textContent = ''; return; }
    const my = ++sv.seq;
    sum.innerHTML = t('<span class="spinner"></span> 検索中…');
    if (sv.scope === 'sessions') return execSessions(q, my);
    const root = state.activeRepo;
    if (!root) { sum.textContent = t('リポジトリを選んでください'); out.innerHTML = ''; return; }
    sv.lastRoot = root;
    let r;
    try {
      r = await OY.code.grep(root, q, { regex: sv.regex, word: sv.word, cs: sv.cs, glob: $('#sv-glob').value.trim(), max: 3000 });
    } catch (e) {
      if (my !== sv.seq) return;
      sum.textContent = '';
      out.innerHTML = `<div class="loading err">${esc(/not a git/.test(e.message) ? t('Git リポジトリではないため、ファイルの全文検索は使えません') : e.message)}</div>`;
      return;
    }
    if (my !== sv.seq) return;
    const groups = new Map();
    for (const m of r.matches || []) {
      if (!groups.has(m.path)) groups.set(m.path, []);
      groups.get(m.path).push(m);
    }
    const re = marker(q);
    const repoName = state.repos.find((x) => norm(x.root) === norm(root))?.name || basename(root);
    sum.innerHTML = `${(r.matches || []).length.toLocaleString()} ${t("件 ·")} ${groups.size} ${t("ファイル")} <span class="muted">（${esc(repoName)}）</span>${r.truncated ? t(' <span class="sv-warn">上限で打ち切り</span>') : ''}`;
    out.innerHTML = [...groups.entries()].map(([path, ms]) => {
      const dir = path.includes('/') ? path.slice(0, path.lastIndexOf('/')) : '';
      const open = !sv.collapsed.has(path);
      return `<div class="sv-file${open ? '' : ' closed'}" data-path="${esc(path)}"><div class="sv-fhead" draggable="true"><span class="caret">${open ? '▾' : '▸'}</span><span class="sv-fname">${esc(basename(path))}</span><span class="sv-fdir">${esc(dir)}</span><span class="sv-n">${ms.length}</span></div>
        <div class="sv-hits">${ms.slice(0, 200).map((m) => `<div class="sv-hit" data-line="${m.line}" data-col="${firstCol(m.text, re)}"><span class="ln">${m.line}</span><span class="tx">${highlight(m.text.trim(), re)}</span></div>`).join('')}</div></div>`;
    }).join('') || t('<div class="empty-note">見つかりませんでした</div>');
  }
  async function execSessions(q, my) {
    const out = $('#sv-results');
    const sum = $('#sv-summary');
    if (q.trim().length < 2) { sum.textContent = t('2 文字以上で検索してください'); out.innerHTML = ''; return; }
    let r;
    try { r = await api.get(`/api/search/sessions?q=${encodeURIComponent(q.trim())}`); }
    catch (e) { if (my === sv.seq) { sum.textContent = ''; out.innerHTML = `<div class="loading err">${esc(e.message)}</div>`; } return; }
    if (my !== sv.seq) return;
    const groups = new Map();
    for (const h of r.hits || []) {
      if (!groups.has(h.session)) groups.set(h.session, []);
      groups.get(h.session).push(h);
    }
    const re = marker(q.trim());
    sum.innerHTML = `${(r.hits || []).length} ${t("件 ·")} ${groups.size} ${t("セッション")} <span class="muted">${t("（すべての会話）")}</span>`;
    out.innerHTML = [...groups.entries()].map(([id, hs]) => {
      const s = state.byId.get(id);
      return `<div class="sv-file" data-session="${esc(id)}"><div class="sv-fhead"><span class="caret">▾</span><span class="sv-fname">${esc(s ? sessionTitle(s) : id)}</span><span class="sv-fdir">${esc(s?.repo?.name || '')}${s?.last_at ? ` · ${esc(ago(s.last_at))}` : ''}</span><span class="sv-n">${hs.length}</span></div>
        <div class="sv-hits">${hs.map((h) => `<div class="sv-hit" data-ts="${esc(h.ts || '')}"><span class="ln ${h.role}">${h.role === 'user' ? t('あなた') : 'AI'}</span><span class="tx">${highlight(h.snippet, re)}</span></div>`).join('')}</div></div>`;
    }).join('') || t('<div class="empty-note">見つかりませんでした</div>');
  }

  /// Search from elsewhere (references, Ctrl+Shift+F with a selection).
  function run({ q, word, cs, regex, scope } = {}) {
    if (word !== undefined) sv.word = word;
    if (cs !== undefined) sv.cs = cs;
    if (regex !== undefined) sv.regex = regex;
    if (scope) sv.scope = scope;
    paintOpts();
    OY.sidebar.show('search');
    const box = $('#sv-q');
    if (q != null) box.value = q;
    box.focus();
    box.select();
    if (box.value.trim()) exec();
  }

  function bind() {
    const box = $('#sv-q');
    box.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); exec(); } });
    box.addEventListener('input', () => { clearTimeout(sv.timer); if (box.value.trim().length >= 3) sv.timer = setTimeout(exec, 450); });
    $('#sv-glob').addEventListener('keydown', (e) => { if (e.key === 'Enter') exec(); });
    $('#sb-search').addEventListener('click', (e) => {
      const opt = e.target.closest('.sv-opt');
      if (opt) { sv[opt.dataset.opt] = !sv[opt.dataset.opt]; LS.set('sv.' + (opt.dataset.opt === 'cs' ? 'case' : opt.dataset.opt), sv[opt.dataset.opt]); paintOpts(); exec(); return; }
      const head = e.target.closest('.sv-fhead');
      if (head) {
        const f = head.closest('.sv-file');
        const key = f.dataset.path || f.dataset.session;
        f.classList.toggle('closed');
        if (f.classList.contains('closed')) sv.collapsed.add(key); else sv.collapsed.delete(key);
        $('.caret', head).textContent = f.classList.contains('closed') ? '▸' : '▾';
        return;
      }
      const hit = e.target.closest('.sv-hit');
      if (!hit) return;
      $$('#sv-results .sv-hit.cur').forEach((x) => x.classList.remove('cur'));
      hit.classList.add('cur');
      const f = hit.closest('.sv-file');
      if (f.dataset.session) { OY.chat.open(f.dataset.session, { ts: hit.dataset.ts || null }); return; }
      const root = sv.lastRoot || state.activeRepo;
      OY.code.jump({ path: joinPath(root, f.dataset.path), root, line: +hit.dataset.line, col: +hit.dataset.col, select: $('#sv-q').value });
    });
    $('#sv-results').addEventListener('dragstart', (e) => {
      const f = e.target.closest('.sv-file');
      if (!f?.dataset.path) return;
      e.dataTransfer.setData('text/oy-open', JSON.stringify(OY.editors.fileDesc(joinPath(sv.lastRoot || state.activeRepo, f.dataset.path), sv.lastRoot || state.activeRepo)));
    });
    $('#sv-scope').addEventListener('change', (e) => { sv.scope = e.target.value; LS.set('sv.scope', sv.scope); paintOpts(); exec(); });
    // A different repository: results of the old one would be misleading.
    bus.on('active-repo', (root) => {
      if (sv.scope === 'files' && sv.lastRoot && norm(root) !== norm(sv.lastRoot) && $('#sv-q').value.trim()) exec();
    });
  }

  function init() { paintOpts(); bind(); }

  OY.search = { init, run, exec };
})();
