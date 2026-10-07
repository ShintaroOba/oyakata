/* OYAKATA sidebar: session list (repositories that have sessions), project explorer for the
   active repository, and a Git view with commit / push / pull. Views can be switched or
   stacked. */
(() => {
  'use strict';
  const { $, $$, esc, api, state, bus, LS, toast, ago, sessionTitle, basename, norm, joinPath, modal, confirmDialog, showOutput } = OY;

  const sb = {
    view: LS.get('sb.view', 'sessions'),
    mode: LS.get('sb.mode', 'single'),
    collapsed: new Set(LS.get('sb.collapsed', [])),
    groupsCollapsed: new Set(LS.get('collapsed', [])),
    filter: '',
    treeFilter: '',
    expanded: new Set(),
    tree: { root: null, files: null, status: null, isGit: false, dirs: new Map() },
    git: { root: null, status: null, log: [], branches: [], selected: new Set(), open: { changes: true, log: true, branches: false } },
    activeFile: null,
    refreshTimer: null,
  };

  // ------------------------------------------------------------ views
  function applyMode() {
    const views = $('#sb-views');
    const stack = sb.mode === 'stack';
    views.classList.toggle('stack', stack);
    $('#sb-mode').classList.toggle('on', stack);
    for (const v of $$('.sb-view', views)) {
      const name = v.dataset.view;
      const visible = stack || name === sb.view;
      v.classList.toggle('visible', visible);
      $('.sb-view-title', v).hidden = !stack;
      v.classList.toggle('collapsed', stack && sb.collapsed.has(name));
    }
    for (const b of $$('#sb-switch .sw[data-view]')) b.classList.toggle('active', !stack && b.dataset.view === sb.view);
  }
  function show(view) {
    sb.view = view;
    LS.set('sb.view', view);
    if (sb.mode === 'stack') { sb.collapsed.delete(view); LS.set('sb.collapsed', [...sb.collapsed]); }
    applyMode();
    document.body.classList.add('sb-open');
  }

  // --------------------------------------------------------- sessions
  function isActive(s) { return s.status !== 'ended'; }
  function matches(s, q) {
    if (!q) return true;
    const hay = [s.title, s.first_prompt, s.last_prompt, s.repo?.name, s.cwd, s.git_branch, s.live?.name, s.id].join(' ').toLowerCase();
    return q.split(/\s+/).every((w) => hay.includes(w));
  }
  function rowHtml(s) {
    const cls = ['row'];
    if (state.openChats.has(s.id) && OY.wb.has('chat:' + s.id)) cls.push('active');
    if (s.status !== 'ended') cls.push(s.status);
    const unread = state.unread.get(s.id) || 0;
    const name = s.live?.name ? `<span class="chip tiny">${esc(s.live.name)}</span>` : '';
    const owner = s.owner === 'oyakata' ? '<span class="chip tiny owner">親方</span>' : '';
    const sub = s.repo?.subdir ? ` <span class="row-sub">/${esc(s.repo.subdir)}</span>` : '';
    const branch = s.git_branch && s.git_branch !== 'HEAD' ? ` · ${esc(s.git_branch)}` : '';
    return `<div class="row ${cls.join(' ')}" draggable="true" data-id="${esc(s.id)}" title="${esc(sessionTitle(s))}">
      <span class="dot"></span>
      <span class="row-title">${esc(sessionTitle(s))}</span>
      <span class="row-meta">${esc(ago(s.last_at))} · ${s.user_turns}往復${branch}${sub}${name}${owner}</span>
      ${unread ? `<span class="badge">${unread}</span>` : ''}
    </div>`;
  }
  function groupHtml(key, name, sessions, { root = null, title = '', repo = false } = {}) {
    const open = !sb.groupsCollapsed.has(key);
    const isActiveRepo = root && norm(root) === norm(state.activeRepo);
    const btns = root ? `<button type="button" class="g-btn g-tree" data-root="${esc(root)}" title="ツリーと Git を開く">⊞</button><button type="button" class="g-btn g-add" data-root="${esc(root)}" title="このフォルダで新しいセッション">＋</button>` : '';
    return `<details class="group${isActiveRepo ? ' active-repo' : ''}" data-key="${esc(key)}"${root ? ` data-root="${esc(root)}"` : ''}${open ? ' open' : ''}>
      <summary><span class="caret">▶</span><span class="g-name" title="${esc(title || name)}">${esc(name)}</span>${btns}<span class="g-count">${sessions.length}</span></summary>
      ${sessions.map(rowHtml).join('')}
    </details>`;
  }
  function renderSessions() {
    const q = sb.filter.trim().toLowerCase();
    const vis = state.sessions.filter((s) => (state.showEmpty || s.user_turns > 0 || isActive(s)) && matches(s, q));
    const live = vis.filter(isActive);
    const repoGroups = new Map();
    const otherGroups = new Map();
    for (const s of vis) {
      const map = s.repo?.root ? repoGroups : otherGroups;
      const k = s.repo?.key || s.project_dir;
      let g = map.get(k);
      if (!g) { g = { key: k, repo: s.repo, sessions: [], last: '' }; map.set(k, g); }
      g.sessions.push(s);
      if ((s.last_at || '') > g.last) g.last = s.last_at || '';
    }
    const byLast = (a, b) => (b.last > a.last ? 1 : b.last < a.last ? -1 : 0);
    let html = '';
    if (live.length) html += `<section class="sb-section live"><h3>稼働中 <span>${live.length}</span></h3>${live.map(rowHtml).join('')}</section>`;
    html += '<section class="sb-section"><h3>リポジトリ</h3>';
    const rg = [...repoGroups.values()].sort(byLast);
    for (const g of rg) html += groupHtml(g.key, g.repo.name, g.sessions, { root: g.repo.root, title: g.repo.root, repo: true });
    if (!rg.length) html += '<div class="sb-empty">リポジトリに紐づくセッションはありません</div>';
    html += '</section>';
    if (otherGroups.size) {
      html += '<section class="sb-section"><h3>その他</h3>';
      for (const g of [...otherGroups.values()].sort(byLast)) {
        const first = g.sessions[0];
        html += groupHtml(g.key, first.cwd || g.repo?.name || g.key, g.sessions, { root: first.cwd, title: first.cwd });
      }
      html += '</section>';
    }
    $('#session-list').innerHTML = html;
    $('#counts').textContent = `${state.sessions.length} セッション · ${state.sessions.filter(isActive).length} 稼働`;
  }

  // --------------------------------------------------------- explorer
  function repoChoices() {
    const map = new Map();
    for (const s of state.sessions) {
      if (s.repo?.root && (s.user_turns > 0 || isActive(s))) map.set(norm(s.repo.root), { root: s.repo.root, name: s.repo.name });
      else if (!s.repo?.root && s.cwd && (s.user_turns > 0 || isActive(s))) map.set(norm(s.cwd), { root: s.cwd, name: s.cwd });
    }
    if (state.activeRepo && !map.has(norm(state.activeRepo))) map.set(norm(state.activeRepo), { root: state.activeRepo, name: basename(state.activeRepo) });
    return [...map.values()].sort((a, b) => a.name.localeCompare(b.name));
  }
  function renderRepoSelect() {
    const sel = $('#explorer-repo');
    const choices = repoChoices();
    sel.innerHTML = choices.map((c) => `<option value="${esc(c.root)}">${esc(c.name)}</option>`).join('') || '<option value="">（リポジトリなし）</option>';
    if (state.activeRepo) sel.value = state.activeRepo;
    if (!sel.value && choices.length) { sel.value = choices[0].root; OY.setActiveRepo(choices[0].root); }
    $('#explorer-follow').classList.toggle('on', state.followRepo);
  }
  async function loadTree(root, { force = false } = {}) {
    if (!root) { $('#repo-tree').innerHTML = '<div class="sb-empty">リポジトリを選んでください</div>'; return; }
    const t = sb.tree;
    if (!force && norm(t.root) === norm(root) && t.files) { renderTree(); return; }
    t.root = root;
    t.files = null;
    t.dirs = new Map();
    $('#repo-tree').innerHTML = '<div class="loading">読み込み中…</div>';
    try {
      const st = await api.get(`/api/git/status?root=${encodeURIComponent(root)}`).catch(() => null);
      t.isGit = !!st;
      t.status = st;
      if (st) {
        const r = await api.get(`/api/git/tree?root=${encodeURIComponent(root)}`);
        t.files = r.files || [];
      } else {
        t.files = [];
      }
      if (norm(sb.git.root) !== norm(root) || !sb.git.status) { sb.git.root = root; sb.git.status = st; }
      renderTree();
      renderGit();
    } catch (e) {
      $('#repo-tree').innerHTML = `<div class="loading err">${esc(e.message)}</div>`;
    }
  }
  function statusMap() {
    const m = new Map();
    for (const e of sb.tree.status?.entries || []) m.set(e.path, e);
    return m;
  }
  function badgeFor(e) {
    if (!e) return '';
    if (e.untracked) return 'Q';
    if (e.conflict) return 'U';
    const c = e.index !== ' ' && e.index !== '?' ? e.index : e.worktree;
    return /^[MADR]$/.test(c) ? c : (c.trim() || 'M');
  }
  function badge(e) {
    const b = badgeFor(e);
    return b ? `<span class="tstat ${esc(b)}">${esc(b === 'Q' ? '?' : b)}</span>` : '';
  }
  function iconFor(name) {
    const ext = (name.split('.').pop() || '').toLowerCase();
    if (/^(md|markdown|txt)$/.test(ext)) return '📝';
    if (/^(png|jpe?g|gif|svg|webp|ico)$/.test(ext)) return '🖼';
    if (/^(json|yml|yaml|toml|ini|env|lock)$/.test(ext)) return '⚙';
    if (/^(html|css|scss)$/.test(ext)) return '🌐';
    return '📄';
  }
  function buildTree(files) {
    const rootNode = { name: '', dirs: new Map(), files: [] };
    for (const f of files) {
      const parts = f.split('/');
      let node = rootNode;
      for (let i = 0; i < parts.length - 1; i++) {
        let d = node.dirs.get(parts[i]);
        if (!d) { d = { name: parts[i], dirs: new Map(), files: [], path: parts.slice(0, i + 1).join('/') }; node.dirs.set(parts[i], d); }
        node = d;
      }
      node.files.push({ name: parts[parts.length - 1], path: f });
    }
    return rootNode;
  }
  function fileNode(path, name, stMap) {
    return `<div class="tnode file${sb.activeFile === path ? ' active' : ''}" draggable="true" data-file="${esc(path)}" title="${esc(path)}"><span class="tcaret"></span><span class="ticon">${iconFor(name)}</span><span class="tname">${esc(name)}</span>${badge(stMap.get(path))}</div>`;
  }
  function renderTree() {
    const t = sb.tree;
    const el = $('#repo-tree');
    if (!t.root) { el.innerHTML = '<div class="sb-empty">リポジトリを選んでください</div>'; return; }
    if (!t.isGit) { renderPlainTree(); return; }
    const stMap = statusMap();
    const dirty = new Set();
    for (const p of stMap.keys()) {
      const parts = p.split('/');
      for (let i = 1; i < parts.length; i++) dirty.add(parts.slice(0, i).join('/'));
    }
    const q = sb.treeFilter.trim().toLowerCase();
    if (q) {
      const hits = (t.files || []).filter((f) => f.toLowerCase().includes(q)).slice(0, 500);
      el.innerHTML = hits.map((f) => fileNode(f, f, stMap)).join('') || '<div class="empty-note">該当なし</div>';
      return;
    }
    const tree = buildTree(t.files || []);
    const render = (node) => {
      let h = '';
      for (const d of [...node.dirs.values()].sort((a, b) => a.name.localeCompare(b.name))) {
        const open = sb.expanded.has(d.path);
        h += `<div class="tnode dir${dirty.has(d.path) ? ' dirty' : ''}" data-dir="${esc(d.path)}" title="${esc(d.path)}"><span class="tcaret">${open ? '▾' : '▸'}</span><span class="ticon">📁</span><span class="tname">${esc(d.name)}</span></div>`;
        if (open) h += `<div class="tchildren">${render(d)}</div>`;
      }
      for (const f of node.files.sort((a, b) => a.name.localeCompare(b.name))) h += fileNode(f.path, f.name, stMap);
      return h;
    };
    el.innerHTML = render(tree) || '<div class="empty-note">ファイルがありません</div>';
  }
  async function renderPlainTree() {
    // Non-git folder: lazy directory listing.
    const t = sb.tree;
    const el = $('#repo-tree');
    const render = async (relDir) => {
      const abs = relDir ? joinPath(t.root, relDir) : t.root;
      if (!t.dirs.has(relDir)) {
        try { t.dirs.set(relDir, (await api.get(`/api/fs/list?path=${encodeURIComponent(abs)}`)).entries || []); }
        catch { t.dirs.set(relDir, []); }
      }
      let h = '';
      for (const e of t.dirs.get(relDir)) {
        const p = relDir ? relDir + '/' + e.name : e.name;
        if (e.dir) {
          const open = sb.expanded.has(p);
          h += `<div class="tnode dir" data-dir="${esc(p)}"><span class="tcaret">${open ? '▾' : '▸'}</span><span class="ticon">📁</span><span class="tname">${esc(e.name)}</span></div>`;
          if (open) h += `<div class="tchildren">${await render(p)}</div>`;
        } else h += fileNode(p, e.name, new Map());
      }
      return h;
    };
    el.innerHTML = (await render('')) || '<div class="empty-note">ファイルがありません</div>';
  }

  // -------------------------------------------------------------- git
  async function loadGit(root, { force = false } = {}) {
    const g = sb.git;
    if (!root) { $('#git-body').innerHTML = '<div class="sb-empty">リポジトリを選んでください</div>'; return; }
    if (norm(g.root) !== norm(root)) { g.root = root; g.status = null; g.log = []; g.branches = []; g.selected = new Set(); }
    try {
      const [st, lg, br] = await Promise.all([
        api.get(`/api/git/status?root=${encodeURIComponent(root)}`),
        api.get(`/api/git/log?root=${encodeURIComponent(root)}&n=40`).catch(() => ({ commits: [] })),
        api.get(`/api/git/branches?root=${encodeURIComponent(root)}`).catch(() => ({ branches: [] })),
      ]);
      if (norm(g.root) !== norm(root)) return;
      g.status = st; g.log = lg.commits || []; g.branches = br.branches || [];
      if (norm(sb.tree.root) === norm(root)) { sb.tree.status = st; if (force) await loadTree(root, { force: true }); else renderTree(); }
      renderGit();
    } catch (e) {
      $('#git-body').innerHTML = `<div class="loading err">${esc(e.message)}${/not a git/.test(e.message) ? '<div class="muted" style="margin-top:6px">このフォルダは Git リポジトリではありません</div>' : ''}</div>`;
    }
  }
  function renderGit() {
    const g = sb.git;
    const st = g.status;
    const body = $('#git-body');
    if (!g.root || !st) { body.innerHTML = g.root ? '<div class="loading">読み込み中…</div>' : '<div class="sb-empty">リポジトリを選んでください</div>'; return; }
    const entries = st.entries || [];
    const all = entries.length && entries.every((e) => g.selected.has(e.path));
    const sync = [st.ahead ? `↑${st.ahead}` : '', st.behind ? `↓${st.behind}` : ''].filter(Boolean).join(' ');
    const name = basename(g.root);
    body.innerHTML = `
      <div class="git-head"><span class="branch" title="${esc(g.root)}">${esc(name)}</span><span class="chip">⎇ ${esc(st.detached ? 'HEAD' : st.branch || '?')}${st.upstream ? ` → ${esc(st.upstream)}` : ''}</span>${sync ? `<span class="chip" title="push 待ち ${st.ahead || 0} / pull 待ち ${st.behind || 0}">${sync}</span>` : ''}</div>
      <div class="git-actions"><button type="button" class="btn small" data-git="refresh">更新</button><button type="button" class="btn small" data-git="pull">Pull</button><button type="button" class="btn small" data-git="push"${!st.ahead && st.upstream ? ' disabled' : ''}>Push</button><button type="button" class="btn small primary" data-git="commit"${entries.length ? '' : ' disabled'}>Commit…</button><button type="button" class="btn small" data-git="new">新しいセッション</button></div>
      <div class="git-section"><h3 data-sec="changes"><span>変更 ${entries.length}</span><span class="caret">${g.open.changes ? '▾' : '▸'}</span></h3>${g.open.changes ? (entries.length ? `
        <div class="list-head"><label><input type="checkbox" id="chg-all"${all ? ' checked' : ''}> すべて</label><span class="sel-count">${g.selected.size} 件選択</span><span class="spacer"></span><button type="button" class="link-btn" data-git="diff-all">すべての差分</button></div>
        ${entries.map((e) => `<div class="lrow chg" data-path="${esc(e.path)}" draggable="true" title="${esc(e.path)}${e.staged ? '（ステージ済み）' : ''}${e.unstaged ? '（未ステージ）' : ''}"><input type="checkbox" class="chg-sel"${g.selected.has(e.path) ? ' checked' : ''}><span class="st">${esc(badgeFor(e) === 'Q' ? '?' : badgeFor(e))}</span><span class="lpath"><span>${esc(e.path)}</span></span></div>`).join('')}`
        : `<div class="empty-note">作業ツリーに変更はありません${st.head ? `<br><span class="muted">HEAD ${esc(st.head)} ${esc(st.head_subject || '')}</span>` : ''}</div>`) : ''}</div>
      <div class="git-section"><h3 data-sec="log"><span>ログ</span><span class="caret">${g.open.log ? '▾' : '▸'}</span></h3>${g.open.log ? (g.log.length ? g.log.map((c) => `<div class="lrow commit" data-hash="${esc(c.hash)}" data-subject="${esc(c.subject)}" title="${esc(c.author)} · ${esc(c.date)}"><span class="lhash">${esc(c.short)}</span><span class="lsub">${esc(c.subject)}</span><span class="lmeta">${esc(ago(c.date))}</span></div>`).join('') : '<div class="empty-note">コミットがありません</div>') : ''}</div>
      <div class="git-section"><h3 data-sec="branches"><span>ブランチ</span><span class="caret">${g.open.branches ? '▾' : '▸'}</span></h3>${g.open.branches ? (g.branches.map((b) => `<div class="lrow"><span class="lhash">${b.current ? '●' : ' '}</span><span class="lsub">${esc(b.name)}</span><span class="lmeta">${b.upstream ? '→ ' + esc(b.upstream) : ''}</span></div>`).join('') || '<div class="empty-note">ブランチがありません</div>') : ''}</div>`;
  }
  function commitDialog() {
    const g = sb.git;
    const entries = g.status?.entries || [];
    const sel = [...g.selected].filter((p) => entries.some((e) => e.path === p));
    modal({
      title: 'コミット',
      body: `
        <label class="field"><span>コミットメッセージ</span><textarea id="cm-msg" placeholder="変更の要約"></textarea></label>
        <div class="field"><span>対象</span>
          <label><input type="radio" name="cm-scope" value="selected"${sel.length ? ' checked' : ' disabled'}> 選択した ${sel.length} 件</label>
          <label><input type="radio" name="cm-scope" value="all"${sel.length ? '' : ' checked'}> すべての変更（${entries.length} 件、git add -A）</label>
        </div>
        <p class="note">${esc(g.status?.branch || '')} に直接コミットします。push は別途行います。</p>`,
      actions: [
        { label: 'キャンセル' },
        {
          label: 'コミット', primary: true,
          onClick: async (card) => {
            const message = $('#cm-msg', card).value.trim();
            if (!message) throw new Error('コミットメッセージを入力してください');
            const scope = $('input[name="cm-scope"]:checked', card)?.value || 'all';
            const r = await api.post('/api/git/commit', { root: g.root, message, paths: scope === 'selected' ? sel : [] });
            toast('コミットしました');
            g.selected = new Set();
            await loadGit(g.root, { force: true });
            showOutput('コミット結果', r.output);
          },
        },
      ],
      onOpen: (card) => $('#cm-msg', card).focus(),
    });
  }
  async function pushFlow() {
    const st = sb.git.status || {};
    if (!(await confirmDialog('Push', `<code>${esc(st.branch || 'HEAD')}</code> を ${st.upstream ? `<code>${esc(st.upstream)}</code>` : '新しい上流（origin）'} へ push します。${st.ahead ? `未送信コミット ${st.ahead} 件。` : ''}`, { label: 'Push' }))) return;
    toast('push 中…');
    try { const r = await api.post('/api/git/push', { root: sb.git.root }); await loadGit(sb.git.root, { force: true }); showOutput('Push 結果', r.output); }
    catch (e) { showOutput('Push に失敗', e.message); }
  }
  async function pullFlow() {
    const st = sb.git.status || {};
    if (!(await confirmDialog('Pull', `<code>${esc(st.upstream || '上流')}</code> から fast-forward で取り込みます（git pull --ff-only）。`, { label: 'Pull' }))) return;
    toast('pull 中…');
    try { const r = await api.post('/api/git/pull', { root: sb.git.root }); await loadGit(sb.git.root, { force: true }); showOutput('Pull 結果', r.output); bus.emit('files-changed', { root: sb.git.root }); }
    catch (e) { showOutput('Pull に失敗', e.message); }
  }
  function scheduleRefresh(root) {
    if (root && norm(root) !== norm(state.activeRepo)) return;
    clearTimeout(sb.refreshTimer);
    sb.refreshTimer = setTimeout(() => loadGit(state.activeRepo, { force: true }), 1500);
  }

  // ------------------------------------------------------------- bind
  function bind() {
    $('#sb-switch').addEventListener('click', (e) => {
      const b = e.target.closest('.sw');
      if (!b) return;
      if (b.id === 'sb-mode') { sb.mode = sb.mode === 'stack' ? 'single' : 'stack'; LS.set('sb.mode', sb.mode); applyMode(); return; }
      show(b.dataset.view);
    });
    $('#sb-views').addEventListener('click', (e) => {
      const t = e.target.closest('.sb-view-title');
      if (t) {
        const v = t.closest('.sb-view').dataset.view;
        if (sb.collapsed.has(v)) sb.collapsed.delete(v); else sb.collapsed.add(v);
        LS.set('sb.collapsed', [...sb.collapsed]);
        applyMode();
      }
    });
    $('#search').addEventListener('input', (e) => { sb.filter = e.target.value; renderSessions(); });
    const list = $('#session-list');
    list.addEventListener('toggle', (e) => {
      const d = e.target;
      if (!d.classList?.contains('group')) return;
      if (d.open) sb.groupsCollapsed.delete(d.dataset.key); else sb.groupsCollapsed.add(d.dataset.key);
      LS.set('collapsed', [...sb.groupsCollapsed]);
    }, true);
    list.addEventListener('click', (e) => {
      const add = e.target.closest('.g-add');
      if (add) { e.preventDefault(); e.stopPropagation(); OY.newSessionDialog(add.dataset.root); return; }
      const tree = e.target.closest('.g-tree');
      if (tree) { e.preventDefault(); e.stopPropagation(); OY.setActiveRepo(tree.dataset.root, { explicit: true }); show('explorer'); return; }
      const row = e.target.closest('.row');
      if (row) { OY.chat.open(row.dataset.id); state.unread.delete(row.dataset.id); renderSessions(); }
    });
    list.addEventListener('dragstart', (e) => {
      const row = e.target.closest('.row');
      if (!row) return;
      e.dataTransfer.setData('text/oy-open', JSON.stringify(OY.chat.desc(row.dataset.id)));
      e.dataTransfer.effectAllowed = 'copyMove';
    });
    $('#explorer-repo').addEventListener('change', (e) => OY.setActiveRepo(e.target.value, { explicit: true }));
    $('#explorer-follow').addEventListener('click', () => {
      state.followRepo = !state.followRepo;
      LS.set('followRepo', state.followRepo);
      $('#explorer-follow').classList.toggle('on', state.followRepo);
      if (state.followRepo && state.lastChat) { const s = state.byId.get(state.lastChat); if (s?.repo?.root) OY.setActiveRepo(s.repo.root); }
      toast(state.followRepo ? '開いているチャットのリポジトリに追従します' : '追従を止めました');
    });
    $('#explorer-refresh').addEventListener('click', () => { loadTree(state.activeRepo, { force: true }); loadGit(state.activeRepo, { force: true }); });
    $('#tree-filter').addEventListener('input', (e) => { sb.treeFilter = e.target.value; renderTree(); });
    const tree = $('#repo-tree');
    tree.addEventListener('click', (e) => {
      const d = e.target.closest('.tnode.dir');
      if (d) { const p = d.dataset.dir; if (sb.expanded.has(p)) sb.expanded.delete(p); else sb.expanded.add(p); renderTree(); return; }
      const f = e.target.closest('.tnode.file');
      if (f) { sb.activeFile = f.dataset.file; renderTree(); OY.editors.openFile(joinPath(sb.tree.root, f.dataset.file), sb.tree.isGit ? sb.tree.root : null); }
    });
    tree.addEventListener('dragstart', (e) => {
      const f = e.target.closest('.tnode.file');
      if (!f) return;
      e.dataTransfer.setData('text/oy-open', JSON.stringify(OY.editors.fileDesc(joinPath(sb.tree.root, f.dataset.file), sb.tree.isGit ? sb.tree.root : null)));
    });
    const git = $('#git-body');
    git.addEventListener('click', (e) => {
      const h = e.target.closest('h3[data-sec]');
      if (h) { const s = h.dataset.sec; sb.git.open[s] = !sb.git.open[s]; renderGit(); return; }
      const act = e.target.closest('[data-git]');
      if (act) {
        const a = act.dataset.git;
        if (a === 'refresh') loadGit(sb.git.root, { force: true });
        else if (a === 'pull') pullFlow();
        else if (a === 'push') pushFlow();
        else if (a === 'commit') commitDialog();
        else if (a === 'new') OY.newSessionDialog(sb.git.root);
        else if (a === 'diff-all') OY.editors.openDiff(sb.git.root, null);
        return;
      }
      const all = e.target.closest('#chg-all');
      if (all) { sb.git.selected = all.checked ? new Set((sb.git.status?.entries || []).map((x) => x.path)) : new Set(); renderGit(); return; }
      const sel = e.target.closest('.chg-sel');
      if (sel) { const p = sel.closest('.chg').dataset.path; if (sel.checked) sb.git.selected.add(p); else sb.git.selected.delete(p); const c = $('.sel-count', git); if (c) c.textContent = `${sb.git.selected.size} 件選択`; return; }
      const chg = e.target.closest('.chg');
      if (chg) { OY.editors.openDiff(sb.git.root, chg.dataset.path); return; }
      const c = e.target.closest('.commit');
      if (c) OY.editors.openCommit(sb.git.root, c.dataset.hash, c.dataset.subject);
    });
    git.addEventListener('dragstart', (e) => {
      const chg = e.target.closest('.chg');
      if (!chg) return;
      e.dataTransfer.setData('text/oy-open', JSON.stringify({ kind: 'diff', key: `diff:${norm(sb.git.root)}:${chg.dataset.path}:0`, title: basename(chg.dataset.path) + ' 差分', icon: '±', data: { root: sb.git.root, rel: chg.dataset.path, staged: false } }));
    });

    bus.on('sessions', () => { renderSessions(); renderRepoSelect(); if (!sb.tree.root && state.activeRepo) { loadTree(state.activeRepo); loadGit(state.activeRepo); } });
    bus.on('repos', () => renderRepoSelect());
    bus.on('active-repo', (root) => { renderRepoSelect(); renderSessions(); loadTree(root); loadGit(root); });
    bus.on('files-changed', (d) => scheduleRefresh(d.root || d.cwd));
    bus.on('layout', () => renderSessions());
    bus.on('tick', () => { renderSessions(); if (state.activeRepo) loadGit(state.activeRepo); });
  }

  function init() {
    applyMode();
    bind();
    if (state.activeRepo) { loadTree(state.activeRepo); loadGit(state.activeRepo); }
  }

  OY.sidebar = { init, show, renderSessions, loadTree, loadGit };
})();
