/* OYAKATA editors: a saveable CodeMirror file editor, diff / commit viewers, URL viewer
   (docked popup for sites that refuse framing) and subagent transcript viewer. */
(() => {
  'use strict';
  const { $, $$, esc, api, state, bus, md, toast, copyText, basename, relTo, joinPath, buildRange, observeMermaid, bindTranscript, highlight, langFromPath, repoOfCwd } = OY;

  const NO_FRAME = ['claude.ai', 'github.com', 'google.com', 'notion.so', 'atlassian.net', 'microsoft.com', 'office.com', 'slack.com', 'figma.com', 'youtube.com', 'x.com', 'twitter.com', 'linear.app'];

  const div = (cls, html) => { const d = document.createElement('div'); d.className = cls; if (html) d.innerHTML = html; return d; };
  const bar = (parts) => `<div class="ev-bar">${parts.filter(Boolean).join('')}</div>`;
  const sizeStr = (n) => (n >= 1e6 ? (n / 1e6).toFixed(1) + ' MB' : n >= 1e3 ? (n / 1e3).toFixed(1) + ' KB' : n + ' B');

  // ------------------------------------------------------------- file editor
  function cmTheme() { return OY.isDark() ? 'dracula' : 'default'; }
  function cmMode(path) {
    const ext = (path.split('.').pop() || '').toLowerCase();
    const candidates = [CodeMirror.findModeByFileName?.(basename(path)), CodeMirror.findModeByExtension?.(ext)].filter(Boolean);
    for (const info of candidates) {
      let mode = info.mode;
      if (mode === 'gfm') mode = 'markdown';
      if (mode && CodeMirror.modes[mode]) return mode === 'markdown' ? 'markdown' : (info.mime || mode);
    }
    const byExt = { rs: 'rust', toml: 'toml', ps1: 'powershell', md: 'markdown', yml: 'yaml', yaml: 'yaml', json: { name: 'javascript', json: true }, jsonl: { name: 'javascript', json: true }, sh: 'shell', bash: 'shell', py: 'python', go: 'go', rb: 'ruby', php: 'php', sql: 'sql', css: 'css', html: 'htmlmixed', htm: 'htmlmixed', xml: 'xml', svg: 'xml', js: 'javascript', mjs: 'javascript', cjs: 'javascript', ts: 'text/typescript', tsx: 'text/typescript-jsx', jsx: 'text/jsx', c: 'text/x-csrc', h: 'text/x-csrc', cpp: 'text/x-c++src', java: 'text/x-java', cs: 'text/x-csharp', kt: 'text/x-kotlin', diff: 'diff', patch: 'diff', ini: 'properties', env: 'properties', dockerfile: 'dockerfile' };
    return byExt[ext] || null;
  }
  function fileDesc(absPath, root) {
    const r = root || repoOfCwd(absPath)?.root || null;
    return { kind: 'file', key: 'file:' + OY.norm(absPath), title: basename(absPath), icon: '📄', data: { path: absPath, root: r } };
  }
  function fileEditor(desc, tab) {
    const { path } = desc.data;
    const root = desc.data.root || repoOfCwd(path)?.root || null;
    const rel = relTo(root, path);
    const el = div('ev');
    const ext = (path.split('.').pop() || '').toLowerCase();
    const isMd = /^(md|markdown)$/.test(ext);
    const isDoc = /^(html?|svg|pdf)$/.test(ext);
    const rawUrl = `/api/fs/raw?path=${encodeURIComponent(path)}`;
    let file = null;
    let cm = null;
    let mode = isDoc ? 'preview' : 'edit';
    let lastSaved = '';
    const unsubs = [];

    const setDirty = () => OY.wb.setDirty(desc.key, !!cm && cm.getValue() !== lastSaved);
    const render = () => {
      if (!file) return;
      const btns = [];
      if (!file.binary) {
        if (mode !== 'edit') btns.push('<button type="button" class="btn small act" data-act="edit">編集</button>');
        if (isMd && mode !== 'render') btns.push('<button type="button" class="btn small act" data-act="render">描画</button>');
        if (isDoc && mode !== 'preview') btns.push('<button type="button" class="btn small act" data-act="preview">プレビュー</button>');
        if (mode === 'edit') btns.push('<button type="button" class="btn small primary act" data-act="save" title="Ctrl+S">保存</button>');
      }
      if (rel != null && root) btns.push('<button type="button" class="btn small act" data-act="diff">差分</button>');
      btns.push('<button type="button" class="btn small act" data-act="reload">再読込</button>');
      btns.push('<button type="button" class="btn small act" data-act="copy-path">パス</button>');
      let body;
      const img = file.mime.startsWith('image/');
      if (img) body = `<div class="ev-content" style="padding:16px;text-align:center"><img src="${rawUrl}" style="max-width:100%" alt=""></div>`;
      else if (file.binary) body = `<div class="url-card"><div class="big">バイナリファイルです</div><a class="btn" href="${rawUrl}" target="_blank" rel="noopener">ブラウザで開く</a></div>`;
      else if (mode === 'preview') body = `<iframe sandbox="allow-scripts allow-popups allow-forms" src="${rawUrl}" title="${esc(path)}"></iframe>`;
      else if (mode === 'render') body = `<div class="ev-content md">${md(cm ? cm.getValue() : file.content || '')}</div>`;
      else body = '<div class="cm-wrap"></div>';
      const title = `<span class="path" title="${esc(path)}">${esc(rel != null && root ? rel : path)}</span><span>${sizeStr(file.size)}${file.truncated ? ' · 先頭 2MB のみ（保存不可）' : ''}</span>`;
      const keepCm = cm && mode === 'edit' && $('.cm-wrap', el);
      if (!keepCm) {
        const value = cm ? cm.getValue() : null;
        el.innerHTML = bar([title, ...btns]) + body;
        if (mode === 'edit' && !file.binary) {
          cm = CodeMirror($('.cm-wrap', el), {
            value: value ?? file.content ?? '',
            mode: cmMode(path),
            theme: cmTheme(),
            lineNumbers: true,
            lineWrapping: false,
            styleActiveLine: true,
            matchBrackets: true,
            autoCloseBrackets: false,
            indentUnit: 2,
            tabSize: 4,
            readOnly: file.truncated ? 'nocursor' : false,
            extraKeys: { 'Ctrl-S': save, 'Cmd-S': save, Tab: (c) => c.execCommand('insertSoftTab') },
          });
          cm.on('change', setDirty);
          setTimeout(() => cm.refresh(), 0);
        }
        observeMermaid(el);
      } else {
        $('.ev-bar', el).outerHTML = bar([title, ...btns]);
      }
    };
    const load = async () => {
      el.innerHTML = '<div class="loading">読み込み中…</div>';
      try {
        file = await api.get(`/api/fs/file?path=${encodeURIComponent(path)}`);
        lastSaved = file.content ?? '';
        if (cm) cm = null;
        render();
        setDirty();
      } catch (e) {
        el.innerHTML = `<div class="loading err">${esc(e.message)}</div>`;
      }
    };
    async function save() {
      if (!cm || !file || file.truncated) return;
      const content = cm.getValue();
      try {
        const r = await api.post('/api/fs/write', { path, content, eol: file.eol, base_mtime_ms: file.mtime_ms });
        file.mtime_ms = r.mtime_ms;
        file.size = r.size;
        lastSaved = content;
        setDirty();
        toast(`保存しました: ${basename(path)}`);
        $$('.conflict', el).forEach((c) => c.remove());
        bus.emit('files-changed', { path, root });
        const barEl = $('.ev-bar', el);
        if (barEl) $('.ev-bar span:nth-of-type(2)', el).textContent = sizeStr(r.size);
      } catch (e) {
        if (e.status === 409) {
          const c = div('conflict', `<span>ディスク上のファイルが変更されています。</span><button type="button" class="btn small act" data-act="force-save">上書き保存</button><button type="button" class="btn small act" data-act="reload">読み込み直す</button>`);
          $('.ev-bar', el)?.after(c);
        } else toast(e.message);
      }
    }
    async function forceSave() {
      if (!cm) return;
      const content = cm.getValue();
      try {
        const r = await api.post('/api/fs/write', { path, content, eol: file.eol });
        file.mtime_ms = r.mtime_ms;
        lastSaved = content;
        setDirty();
        $$('.conflict', el).forEach((c) => c.remove());
        toast('上書き保存しました');
        bus.emit('files-changed', { path, root });
      } catch (e) { toast(e.message); }
    }
    el.addEventListener('click', (e) => {
      const b = e.target.closest('.act');
      if (!b) return;
      const act = b.dataset.act;
      if (act === 'edit') { mode = 'edit'; render(); }
      else if (act === 'render') { mode = 'render'; render(); }
      else if (act === 'preview') { mode = 'preview'; render(); }
      else if (act === 'save') save();
      else if (act === 'force-save') forceSave();
      else if (act === 'reload') load();
      else if (act === 'diff') openDiff(root, rel);
      else if (act === 'copy-path') copyText(path);
    });
    unsubs.push(bus.on('theme', () => cm?.setOption('theme', cmTheme())));
    unsubs.push(bus.on('files-changed', (d) => {
      // Reload when Claude edits the file we are viewing and we have no local changes.
      if (d.path && OY.norm(d.path) === OY.norm(path) && cm && cm.getValue() === lastSaved) load();
    }));
    load();
    return {
      el,
      onShow: () => { setTimeout(() => cm?.refresh(), 0); },
      dispose: () => { for (const u of unsubs) u(); },
      focus: () => cm?.focus(),
      isDirty: () => !!cm && cm.getValue() !== lastSaved,
    };
  }

  // ------------------------------------------------------------------ diffs
  function diffHtml(text) {
    if (!text.trim()) return '<div class="empty-note">差分はありません</div>';
    const lines = text.replace(/\r\n/g, '\n').split('\n');
    let out = '<div class="diff-view">';
    for (const l of lines) {
      let cls = 'ctx';
      if (l.startsWith('diff --git') || l.startsWith('commit ')) cls = 'file';
      else if (l.startsWith('+++') || l.startsWith('---') || l.startsWith('index ') || l.startsWith('Author:') || l.startsWith('Date:') || l.startsWith('new file') || l.startsWith('deleted file') || l.startsWith('similarity') || l.startsWith('rename ')) cls = 'meta';
      else if (l.startsWith('@@')) cls = 'hunk';
      else if (l.startsWith('+')) cls = 'add';
      else if (l.startsWith('-')) cls = 'del';
      out += `<div class="dl ${cls}">${esc(l) || ' '}</div>`;
    }
    return out + '</div>';
  }
  function diffViewer(desc) {
    const { root, rel, staged } = desc.data;
    const el = div('ev');
    const load = async () => {
      el.innerHTML = '<div class="loading">読み込み中…</div>';
      try {
        const q = `/api/git/diff?root=${encodeURIComponent(root)}${rel ? `&path=${encodeURIComponent(rel)}` : ''}${staged ? '&staged=1' : ''}`;
        const r = await api.get(q);
        el.innerHTML = bar([`<span class="path">${esc(rel || 'すべての変更')}</span><span>${staged ? 'ステージ済み' : '作業ツリー'}</span>`, '<button type="button" class="btn small act" data-act="reload">更新</button>', rel ? '<button type="button" class="btn small act" data-act="open">ファイルを開く</button>' : '']) + `<div class="ev-content">${diffHtml(r.diff || '')}</div>`;
      } catch (e) { el.innerHTML = `<div class="loading err">${esc(e.message)}</div>`; }
    };
    el.addEventListener('click', (e) => {
      const b = e.target.closest('.act');
      if (!b) return;
      if (b.dataset.act === 'reload') load();
      if (b.dataset.act === 'open') openFile(joinPath(root, rel), root);
    });
    const unsub = bus.on('files-changed', (d) => { if (!d.root || OY.norm(d.root) === OY.norm(root)) load(); });
    load();
    return { el, dispose: unsub };
  }
  function commitViewer(desc) {
    const { root, hash } = desc.data;
    const el = div('ev', '<div class="loading">読み込み中…</div>');
    (async () => {
      try {
        const r = await api.get(`/api/git/show?root=${encodeURIComponent(root)}&hash=${encodeURIComponent(hash)}`);
        el.innerHTML = bar([`<span class="path">${esc(hash)}</span>`]) + `<div class="ev-content">${diffHtml(r.text || '')}</div>`;
      } catch (e) { el.innerHTML = `<div class="loading err">${esc(e.message)}</div>`; }
    })();
    return { el };
  }

  // ------------------------------------------------------------------- urls
  function hostOf(url) { try { return new URL(url).hostname.toLowerCase(); } catch { return ''; } }
  function framable(url) {
    const h = hostOf(url);
    if (!h) return false;
    if (h === location.hostname || h === 'localhost' || h === '127.0.0.1') return true;
    return !NO_FRAME.some((d) => h === d || h.endsWith('.' + d));
  }
  function dockedWindow(url) {
    const w = Math.max(480, Math.round(screen.availWidth * 0.42));
    const h = screen.availHeight;
    const left = Math.max(0, screen.availWidth - w);
    const win = window.open(url, 'oyakata-side', `popup=yes,width=${w},height=${h},left=${left},top=0`);
    if (!win) { toast('ポップアップがブロックされました。許可してから再度開いてください。'); return false; }
    try { win.focus(); } catch { /* ignore */ }
    return true;
  }
  function urlViewer(desc) {
    const { url } = desc.data;
    const el = div('ev');
    const canFrame = framable(url);
    const render = (frame) => {
      el.innerHTML = bar([`<span class="path" title="${esc(url)}">${esc(url)}</span>`, '<button type="button" class="btn small act" data-act="dock">別ウィンドウ</button>', `<a class="btn small" href="${esc(url)}" target="_blank" rel="noopener noreferrer">新しいタブ</a>`, '<button type="button" class="btn small act" data-act="copy">コピー</button>'])
        + (frame ? `<iframe src="${esc(url)}" referrerpolicy="no-referrer" title="${esc(url)}"></iframe>` : `<div class="url-card"><div class="big">${esc(url)}</div><div>このサイトは埋め込み表示を許可していないため、右横の別ウィンドウとして開きます。</div><div class="acts"><button type="button" class="btn primary act" data-act="dock">右横の別ウィンドウで開く</button><a class="btn" href="${esc(url)}" target="_blank" rel="noopener noreferrer">新しいタブで開く</a><button type="button" class="btn act" data-act="frame">この中で試す</button></div></div>`);
    };
    render(canFrame);
    el.addEventListener('click', (e) => {
      const b = e.target.closest('.act');
      if (!b) return;
      if (b.dataset.act === 'dock') dockedWindow(url);
      else if (b.dataset.act === 'frame') render(true);
      else if (b.dataset.act === 'copy') copyText(url);
    });
    return { el };
  }

  // ----------------------------------------------------------------- agents
  function agentViewer(desc) {
    const { session, agent } = desc.data;
    const el = div('ev', '<div class="loading">読み込み中…</div>');
    let items = [];
    bindTranscript(el, { items: () => items, cwd: () => state.byId.get(session)?.cwd, sessionId: () => session });
    (async () => {
      try {
        const data = await api.get(`/api/sessions/${encodeURIComponent(session)}/agents/${encodeURIComponent(agent)}`);
        items = data.items || [];
        const title = `${data.agent.agent_type || 'agent'}: ${data.agent.description || agent}`;
        OY.wb.setTitle(desc.key, title);
        el.innerHTML = bar([`<span class="path">${esc(title)}</span>`]) + '<div class="ev-content"><div class="agent-view"></div></div>';
        const { frag } = buildRange(items, 0, items.length, state.byId.get(session)?.cwd);
        $('.agent-view', el).appendChild(frag);
        observeMermaid(el);
      } catch (e) { el.innerHTML = `<div class="loading err">${esc(e.message)}</div>`; }
    })();
    return { el };
  }

  // ------------------------------------------------------------- open helpers
  function openFile(absPath, root, opts) { if (absPath) return OY.wb.open(fileDesc(absPath, root), opts); }
  function openDiff(root, rel, staged = false, opts) {
    if (!root) return;
    return OY.wb.open({ kind: 'diff', key: `diff:${OY.norm(root)}:${rel || '*'}:${staged ? 1 : 0}`, title: (rel ? basename(rel) : basename(root)) + ' 差分', icon: '±', data: { root, rel, staged } }, opts);
  }
  function openCommit(root, hash, subject, opts) {
    return OY.wb.open({ kind: 'commit', key: `commit:${OY.norm(root)}:${hash}`, title: `${hash.slice(0, 7)} ${subject || ''}`, icon: '◉', data: { root, hash } }, opts);
  }
  function openUrl(url, opts) {
    if (!url) return;
    if (!/^https?:/i.test(url)) { window.open(url, '_blank', 'noopener'); return; }
    if (!framable(url)) dockedWindow(url);
    return OY.wb.open({ kind: 'url', key: 'url:' + url, title: hostOf(url), icon: '🔗', data: { url } }, opts);
  }
  function openAgent(session, agent, opts) {
    return OY.wb.open({ kind: 'agent', key: `agent:${session}:${agent}`, title: 'サブエージェント', icon: '🤖', data: { session, agent } }, opts);
  }
  function askUrl() {
    OY.modal({
      title: 'URL を開く',
      body: '<label class="field"><span>URL</span><input type="text" id="url-in" placeholder="https://…"></label><p class="note">claude.ai や GitHub など埋め込みを拒否するサイトは、右横の別ウィンドウとして開きます。</p>',
      actions: [{ label: 'キャンセル' }, { label: '開く', primary: true, onClick: (card) => { const u = $('#url-in', card).value.trim(); if (!u) throw new Error('URL を入力してください'); openUrl(u); } }],
      onOpen: (card) => { const i = $('#url-in', card); i.focus(); i.addEventListener('keydown', (e) => { if (e.key === 'Enter') $('.modal-actions .primary', card).click(); }); },
    });
  }

  OY.wb.registerKind('file', fileEditor);
  OY.wb.registerKind('diff', diffViewer);
  OY.wb.registerKind('commit', commitViewer);
  OY.wb.registerKind('url', urlViewer);
  OY.wb.registerKind('agent', agentViewer);

  OY.editors = { fileDesc, openFile, openDiff, openCommit, openUrl, openAgent, askUrl, highlight, langFromPath };
})();
