/* OYAKATA core: utilities, API, theme, markdown, transcript rendering, event bus, settings,
   dialogs (new session, add repository, folder picker). workbench.js (panes), chat.js
   (session view), team.js (体制図), editors.js (files/diffs/urls) and sidebar.js build on
   window.OY. */
(() => {
  'use strict';

  const $ = (sel, el = document) => el.querySelector(sel);
  const $$ = (sel, el = document) => Array.from(el.querySelectorAll(sel));
  const esc = (s) => String(s ?? '').replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));

  const LS = {
    get(k, d) { try { const v = localStorage.getItem('oyakata.' + k); return v === null ? d : JSON.parse(v); } catch { return d; } },
    set(k, v) { try { localStorage.setItem('oyakata.' + k, JSON.stringify(v)); } catch { /* private mode etc. */ } },
    del(k) { try { localStorage.removeItem('oyakata.' + k); } catch { /* ignore */ } },
  };

  const THEMES = {
    system: { label: t('システムに合わせる'), dark: null },
    light: { label: t('ライト'), dark: false },
    dark: { label: t('ダーク'), dark: true },
    sepia: { label: t('セピア'), dark: false },
    'solar-light': { label: t('Solarized ライト'), dark: false },
    'solar-dark': { label: t('Solarized ダーク'), dark: true },
    nord: { label: 'Nord', dark: true },
    dracula: { label: 'Dracula', dark: true },
    contrast: { label: t('高コントラスト（明）'), dark: false },
    'contrast-dark': { label: t('高コントラスト（暗）'), dark: true },
  };
  const ACCENTS = ['amber', 'red', 'blue', 'green', 'violet', 'pink', 'teal'];
  const ACCENT_HEX = { amber: '#d97706', red: '#dc2626', blue: '#2563eb', green: '#059669', violet: '#7c3aed', pink: '#db2777', teal: '#0d9488' };
  const MODELS = [['', t('既定のモデル')], ['claude-opus-5-5', 'Opus 5.5'], ['claude-fable-5-1', 'Fable 5.1'], ['claude-sonnet-5-5', 'Sonnet 5.5'], ['claude-haiku-4-5-20251001', 'Haiku 4.5']];
  /// What the server knows about an agent (`/api/config` → `agents[]`), by id.
  function agentInfo(id) { return (state.config.agents || []).find((a) => a.id === (id || 'claude')) || null; }
  function agentLabel(id) { return agentInfo(id)?.label || (id && id !== 'claude' ? id : 'Claude'); }
  /// Agents the user can start a session with right now.
  function runnableAgents() { return (state.config.agents || []).filter((a) => a.enabled && a.can_run); }
  function defaultAgent() {
    const saved = LS.get('runAgent', 'claude');
    const list = runnableAgents();
    return list.some((a) => a.id === saved) ? saved : (list[0]?.id || 'claude');
  }
  /// Model a new Claude Code session starts with unless another one is picked.
  const DEFAULT_MODEL = 'claude-opus-5-5';
  /// What an agent falls back to when OYAKATA passes no model / effort: its own settings,
  /// read by the server (`default_model`, `default_effort`, `effort_by_model`).
  function agentDefaultModel(id) { return agentInfo(id)?.default_model || ''; }
  function agentDefaultEffort(id, model) {
    const a = agentInfo(id);
    const by = a?.effort_by_model || {};
    // Per-model settings are keyed by full id; an alias such as "opus" matches the one id
    // that contains it.
    const alias = model && !by[model] ? Object.keys(by).filter((k) => k.includes(`-${model}-`) || k.endsWith(`-${model}`)) : [];
    return (model && by[model]) || (alias.length === 1 ? by[alias[0]] : '') || a?.default_effort || '';
  }
  const EFFORTS = [['', t('既定')], ['low', 'low'], ['medium', 'medium'], ['high', 'high'], ['xhigh', 'xhigh'], ['max', 'max']];
  /// Permission modes, labelled the way Claude Code's terminal shows them under the input.
  const MODES = {
    auto: { glyph: '⏵⏵', label: 'auto mode on', desc: t('安全な操作は自動で許可し、危ない操作だけ確認する（既定）') },
    default: { glyph: '⏵', label: 'default mode', desc: t('操作のたびに確認する') },
    acceptEdits: { glyph: '⏵⏵', label: 'accept edits on', desc: t('ファイルの編集は確認なしで許可する') },
    plan: { glyph: '⏸', label: 'plan mode on', desc: t('調べて計画を立てるだけ（変更しない）') },
    bypassPermissions: { glyph: '⏵⏵', label: 'bypass permissions on', desc: t('すべて確認なしで実行する（危険）') },
  };
  const MODE_CYCLE = ['auto', 'default', 'acceptEdits', 'plan'];
  const DEFAULT_MODE = 'auto';

  const state = {
    sessions: [], byId: new Map(), repos: [], config: {},
    activeRepo: LS.get('activeRepo', null),
    filter: '',
    showEmpty: LS.get('showEmpty', false),
    showLogs: LS.get('showLogs2', false),
    width: LS.get('width', 'narrow'),
    notify: LS.get('notify', false),
    prevStatus: new Map(),
    theme: LS.get('theme', 'system'),
    accent: LS.get('accent', 'amber'),
    fontSize: LS.get('fontSize', '14'),
    unread: new Map(),
    openChats: new Set(),
  };

  // ------------------------------------------------------------------- bus
  const listeners = new Map();
  const bus = {
    on(type, fn) { if (!listeners.has(type)) listeners.set(type, new Set()); listeners.get(type).add(fn); return () => listeners.get(type)?.delete(fn); },
    emit(type, data) { for (const fn of listeners.get(type) || []) { try { fn(data); } catch (e) { console.error(type, e); } } },
  };

  // ------------------------------------------------------------------- api
  const api = {
    async get(path) {
      const r = await fetch(path, { headers: { 'X-Oyakata': '1' } });
      const data = await r.json().catch(() => ({}));
      if (!r.ok) throw new Error(data.error || `HTTP ${r.status}`);
      return data;
    },
    async post(path, body) {
      const r = await fetch(path, { method: 'POST', headers: { 'X-Oyakata': '1', 'Content-Type': 'application/json' }, body: JSON.stringify(body || {}) });
      const data = await r.json().catch(() => ({}));
      if (!r.ok) { const e = new Error(data.error || `HTTP ${r.status}`); e.status = r.status; throw e; }
      return data;
    },
  };

  // ----------------------------------------------------------------- theme
  function isDark() {
    const t = THEMES[state.theme] || THEMES.system;
    if (t.dark === null) return matchMedia('(prefers-color-scheme: dark)').matches;
    return t.dark;
  }
  function applyTheme() {
    const dark = isDark();
    const name = state.theme === 'system' ? (dark ? 'dark' : 'light') : state.theme;
    const root = document.documentElement;
    root.dataset.theme = name;
    root.dataset.accent = state.accent;
    root.dataset.dark = dark ? '1' : '0';
    root.dataset.width = state.width;
    root.style.setProperty('--fs', `${state.fontSize}px`);
    $('#hljs-light').disabled = dark;
    $('#hljs-dark').disabled = !dark;
    mermaid.initialize({ startOnLoad: false, securityLevel: 'strict', theme: dark ? 'dark' : 'default', fontFamily: 'inherit' });
    $$('.mermaid-block[data-rendered]').forEach((b) => {
      delete b.dataset.rendered;
      const out = $('.mermaid-out', b);
      if (out) out.innerHTML = '';
      mermaidObserver.observe(b);
    });
    const sel = $('#set-theme');
    if (sel && sel.value !== state.theme) sel.value = state.theme;
    $$('#set-accent .swatch').forEach((s) => s.classList.toggle('on', s.dataset.accent === state.accent));
    bus.emit('theme', { dark });
  }
  function toggleTheme() {
    state.theme = isDark() ? 'light' : 'dark';
    LS.set('theme', state.theme);
    applyTheme();
  }

  // -------------------------------------------------------------- markdown
  const LANG_ALIAS = {
    sh: 'bash', shell: 'bash', zsh: 'bash', console: 'bash', ps: 'powershell', ps1: 'powershell', pwsh: 'powershell',
    yml: 'yaml', rs: 'rust', ts: 'typescript', tsx: 'typescript', js: 'javascript', jsx: 'javascript', mjs: 'javascript', cjs: 'javascript',
    py: 'python', md: 'markdown', jsonl: 'json', json5: 'json', toml: 'ini', text: 'plaintext', txt: 'plaintext', '': 'plaintext',
    html: 'xml', htm: 'xml', svg: 'xml', vue: 'xml', kt: 'kotlin', cs: 'csharp', 'c++': 'cpp', h: 'c', hpp: 'cpp', rb: 'ruby', golang: 'go',
  };
  function normLang(l) {
    l = (l || '').trim().toLowerCase().split(/\s+/)[0];
    return LANG_ALIAS[l] || l;
  }
  function langFromPath(p) {
    const m = /\.([a-z0-9]+)$/i.exec(p || '');
    return m ? normLang(m[1]) : 'plaintext';
  }
  function highlight(code, lang) {
    const l = normLang(lang);
    if (l && l !== 'plaintext' && hljs.getLanguage(l)) {
      try { return { html: hljs.highlight(code, { language: l, ignoreIllegals: true }).value, lang: l }; } catch { /* fall through */ }
    }
    return { html: esc(code), lang: l || 'text' };
  }
  /// Code blocks longer than this many lines start folded, so one long listing does not push
  /// the rest of the answer off screen.
  const TALL_CODE = 28;
  function codeBlock(code, lang) {
    if (normLang(lang) === 'mermaid') {
      return `<div class="mermaid-block"><pre class="mermaid-src">${esc(code)}</pre><div class="mermaid-out"></div></div>`;
    }
    const h = highlight(code, lang);
    const lines = code.split('\n').length;
    const tall = lines > TALL_CODE;
    return `<div class="code-block${tall ? ' tall folded' : ''}"><div class="code-head"><span>${esc(h.lang)}${tall ? ` · ${lines} ${t("行")}` : ''}</span><button type="button" class="copy-btn">${t("コピー")}</button></div><pre><code class="hljs language-${esc(h.lang)}">${h.html}</code></pre>${tall ? `<button type="button" class="code-more">${t("全体を表示（")}${lines} ${t("行）")}</button>` : ''}</div>`;
  }
  marked.use({ gfm: true, renderer: { code({ text, lang }) { return codeBlock(text, lang); } } });
  DOMPurify.addHook('afterSanitizeAttributes', (node) => {
    if (node.tagName === 'A') { node.setAttribute('target', '_blank'); node.setAttribute('rel', 'noopener noreferrer'); node.classList.add('oy-link'); }
  });
  function md(src) {
    let html;
    try { html = marked.parse(src || ''); } catch { html = `<pre>${esc(src)}</pre>`; }
    return DOMPurify.sanitize(html, { ADD_ATTR: ['target'] });
  }

  /// Long answers: a table of contents at the top and foldable sections under each heading.
  function enhanceLongMd(body) {
    if (!body || body.dataset.enhanced) return;
    body.dataset.enhanced = '1';
    const heads = $$(':scope > h1, :scope > h2, :scope > h3', body);
    if (heads.length < 2) return;
    heads.forEach((h, i) => {
      h.classList.add('sec-head');
      h.dataset.sec = i;
      const level = +h.tagName[1];
      const wrap = document.createElement('div');
      wrap.className = 'sec-body';
      let n = h.nextSibling;
      while (n && !(n.nodeType === 1 && /^H[1-3]$/.test(n.tagName) && +n.tagName[1] <= level)) {
        const next = n.nextSibling;
        wrap.appendChild(n);
        n = next;
      }
      h.after(wrap);
    });
    if (heads.length >= 3 || body.textContent.length > 2400) {
      const toc = document.createElement('nav');
      toc.className = 'toc';
      // Many headings: list only the top levels so the contents stay one glance long.
      const top = Math.min(...heads.map((h) => +h.tagName[1]));
      const shown = heads.map((h, i) => ({ h, i })).filter(({ h }) => heads.length <= 10 || +h.tagName[1] <= top + 1);
      toc.innerHTML = t('<span class="toc-label">目次</span>') + shown.map(({ h, i }) => `<a href="#" data-sec="${i}" class="lv${h.tagName[1]}">${esc(h.textContent.trim().slice(0, 40))}</a>`).join('');
      body.prepend(toc);
    }
  }

  // ---------------------------------------------------------------- mermaid
  let mermaidSeq = 0;
  const mermaidObserver = new IntersectionObserver((entries) => {
    for (const e of entries) {
      if (e.isIntersecting) { mermaidObserver.unobserve(e.target); renderMermaid(e.target); }
    }
  }, { rootMargin: '600px 0px' });
  async function renderMermaid(block) {
    if (block.dataset.rendered) return;
    block.dataset.rendered = '1';
    const src = $('.mermaid-src', block)?.textContent || '';
    const out = $('.mermaid-out', block);
    if (!out) return;
    try {
      const { svg } = await mermaid.render('mmd-' + (++mermaidSeq), src);
      out.innerHTML = svg;
    } catch (err) {
      out.innerHTML = `<div class="mermaid-error">${t("Mermaid の描画に失敗:")} ${esc(err?.message || err)}</div><pre class="mermaid-fallback">${esc(src)}</pre>`;
    }
  }
  function observeMermaid(root) {
    $$('.mermaid-block:not([data-rendered])', root).forEach((b) => mermaidObserver.observe(b));
  }

  // ------------------------------------------------------------------ utils
  const WEEKDAYS = t('日月火水木金土');
  function fmtTime(ts) { return ts ? new Date(ts).toLocaleTimeString(OY_I18N.locale(), { hour: '2-digit', minute: '2-digit' }) : ''; }
  function fmtDate(ts) { const d = new Date(ts); const wd = OY_I18N.lang() === 'ja' ? WEEKDAYS[d.getDay()] : d.toLocaleDateString('en-US', { weekday: 'short' }); return `${d.getFullYear()}/${d.getMonth() + 1}/${d.getDate()} (${wd})`; }
  function dateKey(ts) { const d = new Date(ts); return `${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`; }
  function ago(ts) {
    if (!ts) return '';
    const diff = Date.now() - new Date(ts).getTime();
    const m = Math.floor(diff / 60000);
    if (m < 1) return t('たった今');
    if (m < 60) return `${m}${t("分前")}`;
    const h = Math.floor(m / 60);
    if (h < 24) return `${h}${t("時間前")}`;
    const d = Math.floor(h / 24);
    if (d === 1) return t('昨日');
    if (d < 7) return `${d}${t("日前")}`;
    const dt = new Date(ts);
    return `${dt.getMonth() + 1}/${dt.getDate()}`;
  }
  function fmtDuration(sec) {
    sec = Math.max(0, Math.round(sec));
    if (sec < 60) return `${sec}${t("秒")}`;
    const m = Math.floor(sec / 60);
    if (m < 60) return `${m}${t("分")}${sec % 60 ? `${sec % 60}${t("秒")}` : ''}`;
    return `${Math.floor(m / 60)}${t("時間")}${m % 60}${t("分")}`;
  }
  function fmtTokens(n) {
    n = n || 0;
    if (n >= 1e6) return (n / 1e6).toFixed(n >= 1e7 ? 0 : 1).replace(/\.0$/, '') + 'M';
    if (n >= 1e3) return (n / 1e3).toFixed(n >= 1e5 ? 0 : 1).replace(/\.0$/, '') + 'k';
    return String(n);
  }
  function shortModel(m) { return (m || '').replace(/^claude-/, '').replace(/-\d{8}$/, ''); }
  /// Context window of a model: what Claude Code reported when known, else by model family
  /// (Opus / Sonnet 4.6 and later, Fable and Mythos have 1M; Haiku and older ones 200k).
  function contextWindow(model, reported, used) {
    if (reported) return reported;
    const m = (model || '').toLowerCase();
    let w = null;
    if (m.includes('[1m]') || /fable|mythos/.test(m)) w = 1e6;
    else if (/haiku|claude-3/.test(m)) w = 2e5;
    else {
      const v = /(opus|sonnet)-(\d+)(?:-(\d{1,2}))?(?:-|$|\[)/.exec(m);
      if (v) w = +v[2] >= 5 || (+v[2] === 4 && +(v[3] || 0) >= 6) ? 1e6 : 2e5;
    }
    if (used && w && used > w) w = 1e6;
    return w;
  }
  function stripCwd(p, cwd) {
    if (!p) return '';
    if (cwd && p.toLowerCase().startsWith(cwd.toLowerCase())) {
      const rest = p.slice(cwd.length).replace(/^[\\/]/, '');
      return rest || '.';
    }
    return p;
  }
  function norm(p) { return (p || '').replace(/\\/g, '/').replace(/\/+$/, '').toLowerCase(); }
  function joinPath(root, rel) { return root.replace(/[\\/]$/, '') + (root.includes('\\') ? '\\' : '/') + rel.replace(/\//g, root.includes('\\') ? '\\' : '/'); }
  function relTo(root, abs) {
    if (!root || !abs) return null;
    const r = norm(root), a = norm(abs);
    if (a === r) return '';
    if (!a.startsWith(r + '/')) return null;
    return abs.slice(root.replace(/[\\/]$/, '').length + 1).replace(/\\/g, '/');
  }
  function sessionTitle(s) { return s?.title || s?.first_prompt || t('（無題）'); }
  function basename(p) { return (p || '').replace(/[\\/]+$/, '').split(/[\\/]/).pop() || p; }
  function parentPath(p) {
    const t = (p || '').replace(/[\\/]+$/, '');
    const i = Math.max(t.lastIndexOf('\\'), t.lastIndexOf('/'));
    if (i < 0) return null;
    const up = t.slice(0, i);
    return /^[a-z]:$/i.test(up) ? up + '\\' : (up || '/');
  }
  function statusLabel(s) {
    switch (s) {
      case 'busy': return t('作業中');
      case 'idle': return t('待機中');
      case 'waiting': return t('判断待ち');
      default: return t('終了');
    }
  }
  function toast(msg, { action, onAction, ms = 2600 } = {}) {
    let t = $('#toast');
    if (!t) { t = document.createElement('div'); t.id = 'toast'; document.body.appendChild(t); }
    t.innerHTML = `<span>${esc(msg)}</span>${action ? `<button type="button" class="toast-act">${esc(action)}</button>` : ''}`;
    if (action) $('.toast-act', t).addEventListener('click', () => { t.classList.remove('show'); onAction?.(); });
    t.classList.toggle('actionable', !!action);
    t.classList.add('show');
    clearTimeout(toast._timer);
    toast._timer = setTimeout(() => t.classList.remove('show'), action ? Math.max(ms, 7000) : ms);
  }
  async function copyText(text) {
    try { await navigator.clipboard.writeText(text); toast('コピーしました'); }
    catch { toast('コピーできませんでした'); }
  }
  function repoOfCwd(cwd) {
    if (!cwd) return null;
    const c = norm(cwd);
    let best = null;
    for (const r of state.repos) {
      const k = norm(r.root);
      if ((c === k || c.startsWith(k + '/')) && (!best || k.length > norm(best.root).length)) best = r;
    }
    if (best) return best;
    // A linked worktree lives outside its repository's folder: resolve it through a session
    // that ran there, and answer with the worktree as the root (that is where its files are).
    for (const s of state.sessions) {
      const w = s.repo?.worktree;
      if (!w) continue;
      const k = norm(w);
      if (c === k || c.startsWith(k + '/')) return { root: w, name: s.repo.name, worktree: w, branch: s.repo.branch };
    }
    return null;
  }
  /// The checkout a session works in: its worktree when it has one, else the repository.
  function workRoot(s) { return s?.repo?.worktree || s?.repo?.root || s?.cwd || null; }
  function uuid() {
    if (crypto.randomUUID) return crypto.randomUUID();
    const b = crypto.getRandomValues(new Uint8Array(16));
    b[6] = (b[6] & 0x0f) | 0x40; b[8] = (b[8] & 0x3f) | 0x80;
    const h = [...b].map((x) => x.toString(16).padStart(2, '0')).join('');
    return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
  }

  // ------------------------------------------------------------------ modal
  function modal({ title, body, actions = [], wide = false, onOpen, cls = '' }) {
    const m = $('#modal');
    const card = $('#modal-card');
    card.style.width = wide ? 'min(960px, 100%)' : '';
    card.className = 'modal-card ' + cls;
    card.innerHTML = `<h3>${esc(title)}</h3><div class="modal-body">${body}</div><div class="modal-actions"></div>`;
    const acts = $('.modal-actions', card);
    const close = () => { m.hidden = true; card.innerHTML = ''; };
    for (const a of actions) {
      const b = document.createElement('button');
      b.className = 'btn' + (a.primary ? ' primary' : '') + (a.danger ? ' danger' : '');
      b.textContent = a.label;
      b.addEventListener('click', async () => {
        if (a.onClick) {
          b.disabled = true;
          try { const keep = await a.onClick(card, close); if (keep !== true) close(); else b.disabled = false; }
          catch (e) { toast(e.message || String(e)); b.disabled = false; }
        } else close();
      });
      acts.appendChild(b);
    }
    m.hidden = false;
    if (onOpen) onOpen(card, close);
    return close;
  }
  function confirmDialog(title, text, { label = t('実行'), danger = false } = {}) {
    return new Promise((resolve) => {
      modal({
        title,
        body: `<p>${text}</p>`,
        actions: [
          { label: t('キャンセル'), onClick: () => { resolve(false); } },
          { label, primary: !danger, danger, onClick: () => { resolve(true); } },
        ],
      });
    });
  }
  function showOutput(title, text) {
    modal({ title, body: `<pre class="out">${esc(text || t('(出力なし)'))}</pre>`, actions: [{ label: t('閉じる'), primary: true }] });
  }

  // ------------------------------------------------------ transcript items
  const TOOL_ICON = {
    Bash: '⌘', PowerShell: '⌘', Read: '📄', Edit: '✏️', MultiEdit: '✏️', Write: '📝', NotebookEdit: '📓', Grep: '🔍', Glob: '🗂',
    Agent: '🤖', Task: '🤖', Skill: '⚡', WebFetch: '🌐', WebSearch: '🌐', AskUserQuestion: '❓', Artifact: '🧩', TodoWrite: '☑', ToolSearch: '🧰',
    ExitPlanMode: '📋', EnterPlanMode: '📋',
  };
  function divider(text, key) {
    const el = document.createElement('div');
    el.className = 'divider date';
    el.dataset.date = key;
    el.textContent = text;
    return el;
  }
  /// Tool calls that are part of the conversation rather than the work log.
  const CONV_TOOLS = new Set(['ExitPlanMode', 'AskUserQuestion']);
  /// Work log: tool calls, thinking, local commands, turn timings and harness-injected
  /// messages. Hidden entirely unless "作業ログを表示する" is on.
  function isLog(it) {
    if (it.t === 'tool') return !CONV_TOOLS.has(it.name);
    if (it.t === 'user') return !!it.meta;
    return it.t === 'thinking' || it.t === 'note' || it.t === 'turn_end';
  }
  function isPrompt(it) { return it.t === 'user' && !it.meta && !it.compact_summary; }
  /// Name shown on assistant messages while a range is rendered (the session's agent).
  let renderWho = 'Claude';
  function itemNode(i, it, cwd, who) {
    if (who) renderWho = who;
    const el = document.createElement('div');
    el.className = 'item' + (isLog(it) ? ' log' : '') + (isPrompt(it) ? ' turn' : '');
    el.dataset.idx = i;
    el.innerHTML = itemHtml(it, i, cwd);
    if (it.t === 'text') decorateText($('.body.md', el));
    return el;
  }
  /// Long-answer helpers plus clickable file references (`src/main.rs:42` → the code).
  function decorateText(body) {
    if (!body) return;
    enhanceLongMd(body);
    for (const c of $$('code', body)) {
      if (c.closest('pre') || c.classList.contains('path-ref')) continue;
      if (OY.code?.parseRef(c.textContent)) { c.classList.add('path-ref'); c.title = t('クリックでファイルを開く'); }
    }
  }
  function itemHtml(it, i, cwd) {
    switch (it.t) {
      case 'user': return userHtml(it);
      case 'text':
        return `<article class="msg assistant">
          <div class="msg-head"><span class="who">${esc(renderWho)}</span>${it.model ? `<span class="model">${esc(shortModel(it.model))}</span>` : ''}<span class="time">${esc(fmtTime(it.ts))}</span><button type="button" class="link-btn copy-md" data-idx="${i}">${t("コピー")}</button></div>
          <div class="body md">${md(it.md)}</div>
        </article>`;
      case 'thinking':
        return `<details class="thinking"><summary>${t("思考")} <span class="muted">${it.text.length.toLocaleString()} ${t("文字")}</span></summary><div class="thinking-body">${esc(it.text)}</div></details>`;
      case 'tool': return CONV_TOOLS.has(it.name) ? convToolHtml(it) : toolHtml(it, i, cwd);
      case 'compact':
        return `<div class="divider compact">${t("コンテキストを圧縮")}${it.pre_tokens ? ` · ${fmtTokens(it.pre_tokens)} → ${fmtTokens(it.post_tokens)} tokens` : ''}${it.trigger ? ` (${esc(it.trigger)})` : ''}</div>`;
      case 'turn_end':
        return `<div class="turn-end">⏱ ${(it.duration_ms / 1000).toFixed(1)}s</div>`;
      case 'note': {
        const m = /<command-name>([^<]*)<\/command-name>/.exec(it.text);
        const label = m ? m[1].trim() : t('ローカルコマンド');
        const out = /<local-command-stdout>([\s\S]*?)<\/local-command-stdout>/.exec(it.text);
        const body = (out ? out[1] : it.text).trim();
        if (!body) return `<div class="note-line">${esc(label)}</div>`;
        return `<details class="note"><summary>${esc(label)}</summary><pre>${esc(body)}</pre></details>`;
      }
      default: return '';
    }
  }
  /// Plans and questions read as part of the conversation, so they stay when logs are hidden.
  function convToolHtml(it) {
    const r = it.result;
    const inp = it.input || {};
    if (it.name === 'ExitPlanMode') {
      const verdict = !r ? t('<span class="pc-wait">承認待ち…</span>') : r.is_error ? `<span class="hanko-mini ai">${t("差戻")}</span>${r.text ? `<span class="muted"> ${esc(r.text.slice(0, 200))}</span>` : ''}` : t('<span class="hanko-mini">承認</span>');
      return `<div class="conv-card plan"><div class="cc-head">${t("📋 計画")}<span class="spacer"></span>${verdict}</div><div class="md">${md(inp.plan || '')}</div></div>`;
    }
    // Claude Code reports answers as `"question"="answer", ...`; show each under its question.
    const answers = new Map();
    for (const m of (r && !r.is_error ? r.text || '' : '').matchAll(/"([^"]+)"="([^"]*)"/g)) answers.set(m[1], m[2]);
    const qs = (inp.questions || []).map((q) => {
      const a = answers.get(q.question);
      const opts = (q.options || []).map((o) => `<span class="opt${a && a.split(', ').includes(o.label) ? ' picked' : ''}">${esc(o.label)}</span>`).join('');
      return `<div class="cc-q"><div class="q">${esc(q.question)}</div><div class="opts">${opts}</div>${a ? `<div class="cc-ans"><b>${t("回答")}</b> ${esc(a)}</div>` : ''}</div>`;
    }).join('');
    let ans = '';
    if (!r) ans = t('<div class="cc-ans muted">回答待ち…</div>');
    else if (r.is_error) ans = t('<div class="cc-ans muted">（回答されませんでした）</div>');
    else if (!answers.size) ans = `<div class="cc-ans">${esc((r.text || '').replace(/^(The user answered|User has answered your questions?):?\s*/i, '').slice(0, 600))}</div>`;
    return `<div class="conv-card question"><div class="cc-head">❓ ${esc(renderWho)} ${t("からの質問")}</div>${qs}${ans}</div>`;
  }
  function userHtml(it) {
    const imgs = (it.images || []).map((im) => `<img class="user-img" src="data:${esc(im.media_type)};base64,${im.data}" alt="${t("添付画像")}">`).join('');
    if (it.compact_summary) {
      return `<details class="msg user compact-summary"><summary>${t("前のセッションからの引き継ぎ要約")}</summary><div class="body md">${md(it.text)}</div></details>`;
    }
    if (it.meta) {
      const m = /^\s*<([a-z_-]+)/i.exec(it.text);
      const label = m ? m[1] : t('システム');
      return `<details class="msg user meta"><summary><span class="who">${t("システム")}</span> ${esc(label)}</summary><pre class="meta-body">${esc(it.text)}</pre></details>`;
    }
    return `<article class="msg user">
      <div class="msg-head"><span class="who">${t("あなた")}</span><span class="time">${esc(fmtTime(it.ts))}</span></div>
      <div class="bubble">${esc(it.text)}${imgs}</div>
    </article>`;
  }
  function toolHtml(it, i, cwd) {
    const r = it.result;
    const cls = ['tool'];
    if (r?.is_error) cls.push('err');
    if (!r) cls.push('pending');
    const status = !r ? '…' : r.is_error ? '✗' : '✓';
    const agent = it.agent_id ? '<span class="chip tiny">subagent</span>' : '';
    const summary = stripCwd(it.summary, cwd);
    return `<details class="${cls.join(' ')}" data-idx="${i}">
      <summary><span class="tool-icon">${TOOL_ICON[it.name] || '🔧'}</span><span class="tool-name">${esc(it.name)}</span><span class="tool-sum" title="${esc(it.summary)}">${esc(summary)}</span>${agent}<span class="tool-status">${status}</span><span class="time">${esc(fmtTime(it.ts))}</span></summary>
      <div class="tool-body" data-filled="0"></div>
    </details>`;
  }
  /// Input part of a tool call, shared by transcript chips and permission cards.
  function toolInputHtml(name, inp, cwd) {
    inp = inp && typeof inp === 'object' ? inp : {};
    const kv = (k, v, link) => (v == null || v === '' ? '' : `<div class="kv"><span class="k">${esc(k)}</span><span class="v${link ? ' link' : ''}"${link ? ` data-open-file="${esc(link)}"` : ''}>${esc(String(v))}</span></div>`);
    let h = '';
    switch (name) {
      case 'Bash': h += codeBlock(inp.command || '', 'bash'); h += kv(t('説明'), inp.description); break;
      case 'PowerShell': h += codeBlock(inp.command || '', 'powershell'); h += kv(t('説明'), inp.description); break;
      case 'Read': h += kv('file', stripCwd(inp.file_path, cwd), inp.file_path); h += kv('offset', inp.offset); h += kv('limit', inp.limit); break;
      case 'Edit':
        h += kv('file', stripCwd(inp.file_path, cwd), inp.file_path);
        h += `<div class="diff"><pre class="del">${esc(inp.old_string || '')}</pre><pre class="add">${esc(inp.new_string || '')}</pre></div>`;
        if (inp.replace_all) h += kv('replace_all', 'true');
        break;
      case 'Write': h += kv('file', stripCwd(inp.file_path, cwd), inp.file_path); h += codeBlock(inp.content || '', langFromPath(inp.file_path)); break;
      case 'Grep': h += kv('pattern', inp.pattern); h += kv('path', stripCwd(inp.path, cwd)); h += kv('glob', inp.glob); h += kv('type', inp.type); break;
      case 'Glob': h += kv('pattern', inp.pattern); h += kv('path', stripCwd(inp.path, cwd)); break;
      case 'Agent': case 'Task':
        h += kv('type', inp.subagent_type); h += kv(t('説明'), inp.description);
        h += `<div class="md agent-prompt">${md(inp.prompt || '')}</div>`;
        break;
      case 'Skill': h += kv('skill', inp.skill); h += kv('args', inp.args); break;
      case 'WebFetch': h += kv('url', inp.url); h += kv('prompt', inp.prompt); break;
      case 'WebSearch': h += kv('query', inp.query); break;
      case 'ExitPlanMode': h += `<div class="md agent-prompt">${md(inp.plan || '')}</div>`; break;
      case 'AskUserQuestion':
        for (const q of inp.questions || []) {
          h += `<div class="question"><div class="q">${esc(q.question)}</div><ul>${(q.options || []).map((o) => `<li><b>${esc(o.label)}</b>${o.description ? ' — ' + esc(o.description) : ''}</li>`).join('')}</ul></div>`;
        }
        break;
      default: h += `<pre class="json">${esc(JSON.stringify(inp, null, 2))}</pre>`;
    }
    return h;
  }
  const RESULT_PREVIEW = 4000;
  function toolBodyHtml(it, full, cwd) {
    let h = toolInputHtml(it.name, it.input, cwd);
    if (it.agent_id) h += `<div class="agent-link"><button type="button" class="btn open-agent" data-agent="${esc(it.agent_id)}">${t("サブエージェントの会話を開く")}</button></div>`;
    const r = it.result;
    if (!r) { h += t('<div class="result-head pending">結果待ち…</div>'); return h; }
    h += `<div class="result-head ${r.is_error ? 'err' : ''}">${r.is_error ? t('エラー') : t('結果')}${r.truncated ? t(' <span class="muted">(先頭 60,000 文字のみ)</span>') : ''}</div>`;
    const text = r.text || '';
    if (text) {
      const shown = full || text.length <= RESULT_PREVIEW ? text : text.slice(0, RESULT_PREVIEW);
      h += `<pre class="tool-result">${esc(shown)}</pre>`;
      if (!full && text.length > RESULT_PREVIEW) h += `<button type="button" class="link-btn show-all">${t("すべて表示 (")}${text.length.toLocaleString()} ${t("文字)")}</button>`;
    }
    for (const im of r.images || []) h += `<img class="result-img" src="data:${esc(im.media_type)};base64,${im.data}" alt="${t("ツール結果の画像")}">`;
    if (!text && !(r.images || []).length) h += t('<div class="muted">（出力なし）</div>');
    return h;
  }
  function buildRange(items, from, to, cwd, who) {
    if (who) renderWho = who;
    const frag = document.createDocumentFragment();
    let last = null;
    for (let i = from; i < to; i++) {
      const it = items[i];
      const dk = it.ts ? dateKey(it.ts) : null;
      if (dk && dk !== last) { frag.appendChild(divider(fmtDate(it.ts), dk)); last = dk; }
      frag.appendChild(itemNode(i, it, cwd));
    }
    return { frag, lastDate: last };
  }
  /// Wire the click/toggle handlers every transcript container needs.
  /// `ctx` = { items: () => array, cwd: () => string, sessionId: () => string }
  function bindTranscript(root, ctx) {
    const fill = (details, full) => {
      const body = $('.tool-body', details);
      if (!body) return;
      const it = ctx.items()[+details.dataset.idx];
      if (!it || it.t !== 'tool') return;
      body.innerHTML = toolBodyHtml(it, full, ctx.cwd());
      body.dataset.filled = full ? '2' : '1';
      observeMermaid(body);
    };
    root.addEventListener('toggle', (e) => {
      const d = e.target;
      if (d.classList?.contains('tool') && d.open && $('.tool-body', d)?.dataset.filled === '0') fill(d);
    }, true);
    root.addEventListener('click', (e) => {
      const tg = e.target;
      if (tg.closest('.copy-btn')) {
        const code = $('code', tg.closest('.code-block'));
        if (code) copyText(code.textContent);
      } else if (tg.closest('.code-more')) {
        const cb = tg.closest('.code-block');
        const folded = cb.classList.toggle('folded');
        tg.closest('.code-more').textContent = folded ? `${t("全体を表示（")}${$('code', cb).textContent.split('\n').length} ${t("行）")}` : t('折り畳む');
        if (folded) cb.scrollIntoView({ block: 'nearest' });
      } else if (tg.closest('code.path-ref')) {
        OY.code.openRef(tg.closest('code.path-ref').textContent, ctx.cwd());
      } else if (tg.closest('.toc a')) {
        e.preventDefault();
        const a = tg.closest('.toc a');
        const h = $(`.sec-head[data-sec="${a.dataset.sec}"]`, a.closest('.body'));
        if (h) { h.classList.remove('folded'); h.scrollIntoView({ block: 'start', behavior: 'smooth' }); }
      } else if (tg.closest('.sec-head') && !tg.closest('a')) {
        tg.closest('.sec-head').classList.toggle('folded');
      } else if (tg.closest('.copy-md')) {
        const it = ctx.items()[+tg.closest('.copy-md').dataset.idx];
        if (it?.md) copyText(it.md);
      } else if (tg.closest('.show-all')) {
        fill(tg.closest('details.tool'), true);
      } else if (tg.closest('.open-agent')) {
        OY.editors.openAgent(ctx.sessionId(), tg.closest('.open-agent').dataset.agent);
      } else if (tg.closest('[data-open-file]')) {
        OY.editors.openFile(tg.closest('[data-open-file]').dataset.openFile);
      } else if (tg.closest('a.oy-link')) {
        const a = tg.closest('a.oy-link');
        if (/^https?:/i.test(a.href)) { e.preventDefault(); OY.editors.openUrl(a.href); }
      }
    });
    return { fill };
  }

  // -------------------------------------------------------------- sessions
  function applySessions(list) {
    const prev = state.prevStatus;
    state.sessions = list;
    state.byId = new Map(list.map((s) => [s.id, s]));
    for (const s of list) {
      const was = prev.get(s.id);
      if (was && was !== s.status) {
        if (s.status === 'idle' && was === 'busy') {
          notifyEvent(s, `${sessionTitle(s)} — ${t('完了')}`, (s.last_text_snippet || '').slice(0, 160));
          OY.fx?.hyoshigi();
        }
        if (s.status === 'waiting') notifyEvent(s, `${sessionTitle(s)} ${t("が")}${s.waiting_for?.startsWith('question') ? t('質問') : t('判断')}${t("を待っています")}`, s.waiting_for || '');
      }
    }
    state.prevStatus = new Map(list.filter((s) => s.status !== 'ended').map((s) => [s.id, s.status]));
    updateTitle();
    bus.emit('sessions', list);
  }
  function updateTitle() {
    const busy = state.sessions.filter((s) => s.status === 'busy').length;
    const waiting = state.sessions.filter((s) => s.status === 'waiting').length;
    let prefix = '';
    if (waiting) prefix += `[${waiting} ${t("判断待ち]")} `;
    if (busy) prefix += `(${busy}) `;
    document.title = prefix + 'OYAKATA';
    updateFavicon(waiting ? 'waiting' : busy ? 'busy' : null);
  }
  /// The tab icon carries a dot while someone is working (amber) or waiting on you (violet).
  let faviconImg = null;
  let faviconState;
  function updateFavicon(kind) {
    if (kind === faviconState) return;
    faviconState = kind;
    const link = $('#favicon');
    if (!kind) { link.href = '/assets/icon.svg'; return; }
    const draw = () => {
      const c = document.createElement('canvas');
      c.width = c.height = 64;
      const g = c.getContext('2d');
      g.drawImage(faviconImg, 0, 0, 64, 64);
      g.beginPath();
      g.arc(50, 50, 13, 0, Math.PI * 2);
      g.fillStyle = kind === 'waiting' ? '#8b5cf6' : '#f59e0b';
      g.fill();
      g.lineWidth = 4;
      g.strokeStyle = '#ffffff';
      g.stroke();
      if (faviconState === kind) link.href = c.toDataURL('image/png');
    };
    if (faviconImg?.complete) draw();
    else { faviconImg = new Image(); faviconImg.onload = draw; faviconImg.src = '/assets/icon.svg'; }
  }
  function notifyEvent(s, title, body) {
    if (!state.notify || !('Notification' in window) || Notification.permission !== 'granted') return;
    try {
      const n = new Notification(title, { body, tag: 'oyakata-' + s.id, icon: '/assets/icon.svg' });
      n.onclick = () => { window.focus(); location.hash = '#/s/' + s.id; };
    } catch { /* notifications unavailable */ }
  }
  async function toggleNotify() {
    if (!('Notification' in window)) { toast('このブラウザは通知に対応していません'); return; }
    if (state.notify) { state.notify = false; }
    else {
      const perm = Notification.permission === 'granted' ? 'granted' : await Notification.requestPermission();
      if (perm !== 'granted') { toast('通知が許可されていません'); return; }
      state.notify = true;
      toast('待機・判断待ちになったら通知します');
    }
    LS.set('notify', state.notify);
    $('#btn-notify').classList.toggle('on', state.notify);
  }
  async function refreshRepos() {
    try { state.repos = (await api.get('/api/repos')).repos || []; bus.emit('repos', state.repos); } catch { /* keep the old list */ }
  }
  async function refreshSessions() {
    try {
      const [s, r] = await Promise.all([api.get('/api/sessions'), api.get('/api/repos')]);
      state.repos = r.repos || [];
      bus.emit('repos', state.repos);
      applySessions(s.sessions);
    } catch { /* retried by the next SSE event */ }
  }
  /// Point the tree and Git views at a folder (the repository of the selected session).
  function setActiveRepo(root) {
    if (!root) return;
    if (norm(root) === norm(state.activeRepo)) { bus.emit('active-repo', root); return; }
    state.activeRepo = root;
    LS.set('activeRepo', root);
    bus.emit('active-repo', root);
  }
  function followSession(s) {
    const root = workRoot(s);
    if (root) setActiveRepo(root);
  }
  async function deleteSession(id) {
    const s = state.byId.get(id);
    const running = !!s && s.status !== 'ended';
    if (running && s.owner !== 'oyakata') { toast(s.owner === 'external' ? t('別のプロセスで動いているセッションは削除できません。終わってから削除してください。') : t('ターミナルで動いているセッションは削除できません。ターミナルで終了してから削除してください。')); return false; }
    const title = s ? sessionTitle(s) : id;
    const stop = running ? `${t("OYAKATA が動かしている")} ${agentLabel(s.agent)} ${t("を終了してから（作業中なら中断されます）、")}` : '';
    const trash = s?.agent === 'opencode' ? t('OpenCode のセッションはその場で消え、元に戻せません。') : t('会話の記録はゴミ箱（~/.oyakata/trash）に移り、30 日後に消えます。');
    if (!(await confirmDialog(t('セッションを削除'), `${stop}「${esc(title)}${t("」を一覧から削除します。")}${trash}`, { label: running ? t('終了して削除') : t('削除'), danger: true }))) return false;
    try {
      await api.post(`/api/sessions/${encodeURIComponent(id)}/delete`);
      if (OY.wb.has('chat:' + id)) OY.wb.close('chat:' + id, { force: true });
      // A session that never wrote a transcript has nothing in the trash to restore.
      toast('削除しました', s && (!s.size || s.agent === 'opencode') ? {} : { action: t('元に戻す'), onAction: () => restoreSession(id) });
      return true;
    } catch (e) { toast(e.message); return false; }
  }
  async function restoreSession(id) {
    try { await api.post(`/api/sessions/${encodeURIComponent(id)}/restore`); toast('元に戻しました'); }
    catch (e) { toast(e.message); }
  }

  // ------------------------------------------------------- run defaults
  function fillSelect(sel, pairs, value) {
    sel.innerHTML = pairs.map(([v, l]) => `<option value="${esc(v)}">${esc(l)}</option>`).join('');
    sel.value = value || '';
  }
  function runDefaults() {
    const mode = LS.get('runMode2', DEFAULT_MODE);
    return { model: LS.get('runModel', ''), mode: MODES[mode] ? mode : DEFAULT_MODE, effort: LS.get('runEffort', '') };
  }
  function saveRunDefaults({ model, mode, effort }) {
    if (model !== undefined) LS.set('runModel', model);
    if (mode !== undefined) LS.set('runMode2', mode);
    if (effort !== undefined) LS.set('runEffort', effort);
  }

  // ------------------------------------------------------- folder picker
  /// A small folder browser: path input with typed navigation, a list of subfolders, and
  /// "up". `onPick(path)` runs on double-click / Enter / the choose button.
  function folderBrowser(el, start, { onPick, onChange } = {}) {
    let cur = start || state.config.home || '';
    el.innerHTML = `<div class="fb"><div class="fb-path"><button type="button" class="icon-btn small fb-up" title="${t("上のフォルダ")}">↑</button><input type="text" class="fb-input" spellcheck="false"></div><div class="fb-list"></div></div>`;
    const input = $('.fb-input', el);
    const list = $('.fb-list', el);
    const go = async (path) => {
      if (!path) return;
      list.innerHTML = t('<div class="fb-empty">読み込み中…</div>');
      try {
        const r = await api.get(`/api/fs/list?path=${encodeURIComponent(path)}`);
        cur = path;
        input.value = path;
        onChange?.(path);
        const dirs = (r.entries || []).filter((e) => e.dir && !e.name.startsWith('.'));
        list.innerHTML = dirs.map((d) => `<div class="fb-row" data-name="${esc(d.name)}">📁 ${esc(d.name)}</div>`).join('') || t('<div class="fb-empty">サブフォルダはありません</div>');
      } catch (e) {
        list.innerHTML = `<div class="fb-empty err">${esc(e.message)}</div>`;
      }
    };
    const child = (name) => cur.replace(/[\\/]+$/, '') + (cur.includes('/') && !cur.includes('\\') ? '/' : '\\') + name;
    $('.fb-up', el).addEventListener('click', () => { const up = parentPath(cur); if (up) go(up); });
    input.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); go(input.value.trim()); } });
    input.addEventListener('change', () => go(input.value.trim()));
    list.addEventListener('click', (e) => { const r = e.target.closest('.fb-row'); if (r) go(child(r.dataset.name)); });
    list.addEventListener('dblclick', (e) => { const r = e.target.closest('.fb-row'); if (r) onPick?.(child(r.dataset.name)); });
    go(cur);
    return { path: () => input.value.trim() || cur, go };
  }

  // ---------------------------------------------------------- new session
  /// Start a new session: just pick a folder. Everything else (model, permission mode — auto
  /// by default) is set from the chat's status line, like in the terminal.
  function newSessionDialog(cwd) {
    if (cwd) { OY.chat.openDraft(cwd, defaultAgent()); return; }
    const repos = state.repos.slice().sort((a, b) => (b.last_at || '').localeCompare(a.last_at || '') || a.name.localeCompare(b.name));
    let selected = state.activeRepo || repos[0]?.root || state.config.home || '';
    let agent = defaultAgent();
    // Every enabled agent found on this machine; ones whose command is missing are shown
    // disabled so the user sees what was detected.
    const agents = (state.config.agents || []).filter((a) => a.enabled && (a.installed || a.can_run));
    const agentsHtml = agents.length > 1
      ? `<div class="ns-agents" id="ns-agents">${agents.map((a) => `<button type="button" class="ns-agent${a.id === agent ? ' sel' : ''}" data-agent="${esc(a.id)}"${a.can_run ? '' : ` disabled title="${esc(t('コマンド未検出'))}"`}>${esc(a.label)}</button>`).join('')}</div>`
      : '';
    const start = (p) => { LS.set('runAgent', agent); OY.chat.openDraft(p, agent); };
    modal({
      title: t('新しいセッション'),
      cls: 'ns-dialog',
      body: `
        ${agentsHtml}
        <div class="ns-search"><input type="search" id="ns-q" placeholder="${t("リポジトリを絞り込む、またはフォルダのパスを入力")}" autocomplete="off" spellcheck="false"></div>
        <div class="ns-list" id="ns-list"></div>
        <details class="ns-browse"><summary>${t("📂 フォルダを参照…")}</summary><div id="ns-fb"></div></details>
        <p class="note">${t("権限モードは")} <b>auto</b> ${t("で始まります。モデルや権限はチャット下のステータス行（Shift+Tab）でいつでも変えられます。")}${state.config.worktree !== false ? ` ${t("Git リポジトリでは新しい worktree を作って作業します（ステータス行の ⎇ で切替）。")}` : ''}</p>`,
      actions: [
        { label: t('キャンセル') },
        { label: t('開始'), primary: true, onClick: (card) => { const p = ($('#ns-q', card).value.trim().match(/^([a-z]:[\\/]|\/|\\\\)/i) ? $('#ns-q', card).value.trim() : selected); if (!p) throw new Error(t('フォルダを選んでください')); start(p); } },
      ],
      onOpen: (card, close) => {
        $('#ns-agents', card)?.addEventListener('click', (e) => {
          const b = e.target.closest('.ns-agent');
          if (!b) return;
          agent = b.dataset.agent;
          $$('.ns-agent', card).forEach((x) => x.classList.toggle('sel', x === b));
        });
        const list = $('#ns-list', card);
        const render = () => {
          const q = $('#ns-q', card).value.trim().toLowerCase();
          const hits = repos.filter((r) => !q || r.name.toLowerCase().includes(q) || r.root.toLowerCase().includes(q)).slice(0, 80);
          list.innerHTML = hits.map((r) => `<div class="ns-row${norm(r.root) === norm(selected) ? ' sel' : ''}" data-root="${esc(r.root)}"><span class="ns-name">${esc(r.name)}</span><span class="ns-path">${esc(r.root)}</span>${r.sessions ? `<span class="ns-n">${r.sessions}</span>` : ''}</div>`).join('') || t('<div class="fb-empty">一致するリポジトリはありません。パスを入力して Enter で開始できます。</div>');
        };
        render();
        $('#ns-q', card).addEventListener('input', render);
        $('#ns-q', card).addEventListener('keydown', (e) => {
          if (e.key !== 'Enter') return;
          e.preventDefault();
          const v = e.target.value.trim();
          const first = $('.ns-row', list);
          const path = /^([a-z]:[\\/]|\/|\\\\)/i.test(v) ? v : first?.dataset.root;
          if (path) { close(); start(path); }
        });
        list.addEventListener('click', (e) => { const r = e.target.closest('.ns-row'); if (r) { close(); start(r.dataset.root); } });
        let fb = null;
        $('.ns-browse', card).addEventListener('toggle', (e) => {
          if (!e.target.open || fb) return;
          fb = folderBrowser($('#ns-fb', card), selected, { onChange: (p) => { selected = p; }, onPick: (p) => { close(); start(p); } });
        });
        $('#ns-q', card).focus();
      },
    });
  }

  // ------------------------------------------------------ add repository
  function cloneDest(url) {
    const root = state.config.clone_root || state.config.home || '';
    const ghq = !!state.config.ghq_root;
    let u = (url || '').trim().replace(/\/+$/, '').replace(/\.git$/, '');
    if (!u) return '';
    let rest = u.includes('://') ? u.split('://')[1] : u.replace(/^[^@]*@/, '').replace(':', '/');
    rest = rest.replace(/^[^@/]*@/, '');
    const parts = rest.split(/[\\/]/).filter((s) => s && s !== '.' && s !== '..');
    if (!parts.length) return '';
    const sep = root.includes('\\') ? '\\' : '/';
    const segs = ghq && parts.length >= 3 ? [parts[0].split(':')[0], ...parts.slice(1)] : [parts[parts.length - 1]];
    return root.replace(/[\\/]+$/, '') + sep + segs.join(sep);
  }
  function addRepoDialog() {
    let tab = 'local';
    let fb = null;
    modal({
      title: t('リポジトリを追加'),
      cls: 'repo-dialog',
      body: `
        <div class="seg"><button type="button" class="seg-b on" data-tab="local">${t("ローカルのフォルダ")}</button><button type="button" class="seg-b" data-tab="clone">Git clone</button></div>
        <div class="tab-local"><div id="ar-fb"></div><p class="note">${t("選んだフォルダを一覧に加えます（Git リポジトリでなくても可）。")}</p></div>
        <div class="tab-clone" hidden>
          <label class="field"><span>${t("リポジトリの URL")}</span><input type="text" id="ar-url" placeholder="${t("https://github.com/owner/name.git または git@github.com:owner/name.git")}" spellcheck="false"></label>
          <label class="field"><span>${t("clone 先")}</span><input type="text" id="ar-dest" spellcheck="false"></label>
          <p class="note">${state.config.ghq_root ? `${t("ghq と同じ配置（")}<code>${esc(state.config.ghq_root)}</code>${t("/ホスト/オーナー/名前）に置きます。")}` : t('既定では <code>~/repos/名前</code> に置きます。')}${t("認証が必要な場合は、ターミナルで一度 git の認証を済ませておいてください。")}</p>
          <div class="clone-progress" hidden><span class="spinner"></span> ${t("clone しています…（大きなリポジトリは数分かかります）")}</div>
        </div>`,
      actions: [
        { label: t('キャンセル') },
        {
          label: t('追加'), primary: true,
          onClick: async (card) => {
            let r;
            if (tab === 'local') {
              const path = fb?.path();
              if (!path) throw new Error(t('フォルダを選んでください'));
              r = await api.post('/api/repos/add', { path });
              toast(`${t("追加しました:")} ${basename(r.root)}`);
            } else {
              const url = $('#ar-url', card).value.trim();
              const dest = $('#ar-dest', card).value.trim();
              if (!url) throw new Error(t('URL を入力してください'));
              $('.clone-progress', card).hidden = false;
              try { r = await api.post('/api/repos/clone', { url, dest: dest || undefined }); }
              finally { $('.clone-progress', card).hidden = true; }
              toast(`${t("clone しました:")} ${basename(r.root)}`);
            }
            state.repos = r.repos || state.repos;
            bus.emit('repos', state.repos);
            setActiveRepo(r.root);
            OY.sidebar.show('explorer');
          },
        },
      ],
      onOpen: (card) => {
        fb = folderBrowser($('#ar-fb', card), state.config.ghq_root || state.config.home);
        $('.seg', card).addEventListener('click', (e) => {
          const b = e.target.closest('.seg-b');
          if (!b) return;
          tab = b.dataset.tab;
          $$('.seg-b', card).forEach((x) => x.classList.toggle('on', x === b));
          $('.tab-local', card).hidden = tab !== 'local';
          $('.tab-clone', card).hidden = tab !== 'clone';
          if (tab === 'clone') $('#ar-url', card).focus();
        });
        let touched = false;
        $('#ar-dest', card).addEventListener('input', () => { touched = true; });
        $('#ar-url', card).addEventListener('input', (e) => { if (!touched) $('#ar-dest', card).value = cloneDest(e.target.value); });
      },
    });
  }

  // ------------------------------------------------------------------ events
  let es = null;
  function connect() {
    if (es) es.close();
    es = new EventSource('/api/events');
    es.onopen = () => { $('#conn').textContent = t('接続中'); $('#conn').classList.remove('off'); };
    es.onerror = () => { $('#conn').textContent = t('再接続中…'); $('#conn').classList.add('off'); };
    es.addEventListener('sessions', (e) => { applySessions(JSON.parse(e.data).sessions); });
    for (const kind of ['append', 'patch', 'reset', 'run']) {
      es.addEventListener(kind, (e) => bus.emit(kind, JSON.parse(e.data)));
    }
    es.addEventListener('lagged', async () => { await refreshSessions(); bus.emit('lagged'); });
  }
  function handleHash() {
    const m = /^#\/s\/([^/?#]+)/.exec(location.hash);
    if (m) { OY.chat.open(decodeURIComponent(m[1])); return; }
    const r = /^#\/r\/(.+)$/.exec(location.hash);
    if (r) { setActiveRepo(decodeURIComponent(r[1])); OY.sidebar.show('explorer'); }
  }

  // --------------------------------------------------------------- settings
  function renderSettings() {
    const sel = $('#set-theme');
    sel.innerHTML = Object.entries(THEMES).map(([k, t]) => `<option value="${k}">${esc(t.label)}</option>`).join('');
    sel.value = state.theme;
    $('#set-accent').innerHTML = ACCENTS.map((a) => `<span class="swatch${a === state.accent ? ' on' : ''}" data-accent="${a}" style="background:${ACCENT_HEX[a]}" title="${a}"></span>`).join('');
    $('#set-font').value = state.fontSize;
    $('#set-width').value = state.width;
    $('#set-show-empty').checked = state.showEmpty;
    $('#set-show-logs').checked = state.showLogs;
    $('#set-stamp').checked = state.fxStamp;
    $('#set-sound').checked = state.fxSound;
    const lang = $('#set-lang');
    if (lang) lang.value = OY.i18n.lang();
    const wt = $('#set-worktree');
    if (wt) wt.checked = state.config.worktree !== false;
    const ag = $('#set-agents');
    if (ag) {
      const list = (state.config.agents || []).filter((a) => a.installed || a.enabled);
      ag.innerHTML = list.length
        ? list.map((a) => `<label><input type="checkbox" data-agent="${esc(a.id)}"${a.enabled ? ' checked' : ''}> ${esc(a.label)}${a.exe ? '' : t(' <span class="muted">（コマンド未検出）</span>')}</label>`).join('')
        : t('<span class="muted">検出されたエージェントはありません</span>');
    }
  }
  /// First run: let the user pick the language before anything else is read.
  function langDialog() {
    const pick = async (lang) => {
      try { await api.post('/api/config', { lang }); } catch { /* the choice still applies locally */ }
      OY.i18n.setLang(lang);
      location.reload();
    };
    modal({
      title: t('言語 / Language'),
      cls: 'lang-dialog',
      body: `<p>${t("OYAKATA の表示言語を選んでください。あとから設定で変えられます。")}<br>Choose the language for OYAKATA. You can change it later in Settings.</p>
        <div class="lang-btns"><button type="button" class="btn big" data-lang="ja">${t("日本語")}</button><button type="button" class="btn big" data-lang="en">English</button></div>`,
      actions: [],
      onOpen: (card) => { card.addEventListener('click', (e) => { const b = e.target.closest('[data-lang]'); if (b) pick(b.dataset.lang); }); },
    });
  }

  // -------------------------------------------------------------- shortcuts
  /// Keyboard shortcuts, after VS Code. Shown by "ショートカット一覧" and the README.
  const KEYS = [
    ['Ctrl+P', t('ファイルを開く（名前のあいまい検索、main.rs:42 で行も指定）')],
    ['Ctrl+Shift+P / F1', t('コマンドパレット')],
    ['Ctrl+Shift+F', t('全文検索（ファイル / すべての会話）')],
    ['Ctrl+F', t('エディタ内を検索（F3 / Shift+F3 で次 / 前）')],
    ['Ctrl+G', t('行へ移動')],
    [t('F12 / Ctrl+クリック'), t('定義へ移動')],
    ['Shift+F12', t('参照を検索')],
    ['Alt+← / Alt+→', t('移動前の場所へ戻る / 進む')],
    ['Ctrl+Shift+E / G', t('ツリー / Git を表示')],
    ['Ctrl+B', t('サイドバーの表示 / 非表示')],
    ['Ctrl+`', t('チャットの入力欄へ')],
    ['Ctrl+\\', t('アクティブなタブを右に分割')],
    ['Ctrl+Alt+N', t('新しいセッション')],
    ['Ctrl+,', t('設定')],
    ['Ctrl+S', t('保存')],
    ['Shift+Tab', t('（入力欄で）権限モードを切替')],
    ['Esc', t('（作業中なら）中断 / メニューを閉じる')],
    ['/', t('セッション検索')],
  ];
  function showKeys() {
    modal({ title: t('キーボードショートカット'), body: `<table class="keys">${KEYS.map(([k, d]) => `<tr><td>${k.split(' / ').map((x) => x.split('+').map((p) => `<kbd>${esc(p)}</kbd>`).join('+')).join(' / ')}</td><td>${esc(d)}</td></tr>`).join('')}</table><p class="note">${t("Ctrl+W・Ctrl+Tab などはブラウザが先に使うため割り当てていません（タブは中クリックで閉じられます）。")}</p>`, actions: [{ label: t('閉じる'), primary: true }] });
  }
  function toggleSidebar(force) {
    const hidden = force ?? !document.body.classList.contains('sb-hidden');
    document.body.classList.toggle('sb-hidden', hidden);
    LS.set('sbHidden', hidden);
    bus.emit('layout-resized');
  }
  function activeChat() {
    const t = OY.wb.activeTab();
    if (t?.desc?.kind === 'chat') return t.inst?.chat || null;
    for (const p of OY.wb.panes()) {
      const tab = p.active && OY.wb.get(p.active);
      if (tab?.desc?.kind === 'chat') return tab.inst?.chat || null;
    }
    return null;
  }
  function selectedText() {
    const s = String(window.getSelection?.() || '').trim();
    if (s && !s.includes('\n') && s.length < 200) return s;
    const cmEl = document.activeElement?.closest?.('.CodeMirror');
    const sel = cmEl?.CodeMirror?.getSelection();
    return sel && !sel.includes('\n') && sel.length < 200 ? sel : '';
  }
  /// Global shortcuts. Editor-local ones (Ctrl+F, F12…) are CodeMirror key bindings; when the
  /// editor handled a key, the event arrives here already default-prevented.
  function onShortcut(e) {
    if (e.defaultPrevented || e.isComposing) return false;
    const ctrl = e.ctrlKey || e.metaKey;
    const k = e.key.length === 1 ? e.key.toLowerCase() : e.key;
    const run = (fn) => { e.preventDefault(); fn(); return true; };
    if (e.key === 'F1' || (ctrl && e.shiftKey && k === 'p')) return run(() => OY.palette.open('>'));
    if (ctrl && !e.shiftKey && !e.altKey && k === 'p') return run(() => OY.palette.open(''));
    if (ctrl && e.shiftKey && k === 'f') return run(() => OY.search.run({ q: selectedText() || undefined }));
    if (ctrl && e.shiftKey && k === 'e') return run(() => OY.sidebar.show('explorer'));
    if (ctrl && e.shiftKey && k === 'g') return run(() => OY.sidebar.show('git'));
    if (ctrl && !e.shiftKey && k === 'b') return run(() => toggleSidebar());
    if (ctrl && (e.code === 'Backquote' || k === '`')) return run(() => activeChat()?.focusComposer());
    if (ctrl && k === '\\') return run(() => OY.wb.splitActive('right'));
    if (ctrl && k === ',') return run(() => { $('#settings').hidden = false; });
    if (ctrl && e.altKey && k === 'n') return run(() => newSessionDialog());
    if (ctrl && !e.shiftKey && k === 'g') return run(() => OY.palette.open(':'));
    if (e.altKey && !ctrl && e.key === 'ArrowLeft') return run(() => OY.code.back());
    if (e.altKey && !ctrl && e.key === 'ArrowRight') return run(() => OY.code.forward());
    return false;
  }
  function registerCommands() {
    const c = (label, run, keys = '', icon = '›', when) => OY.palette.register({ label, run, keys, icon, when });
    c(t('ファイルを開く…'), () => OY.palette.open(''), 'Ctrl+P', '📄');
    c(t('セッションを開く…'), () => OY.palette.open('@'), '', '💬');
    c(t('新しいセッション'), () => newSessionDialog(), 'Ctrl+Alt+N', '＋');
    c(t('このリポジトリで新しいセッション'), () => newSessionDialog(state.activeRepo), '', '＋', () => !!state.activeRepo);
    c(t('全文検索（ファイル）'), () => OY.search.run({ scope: 'files' }), 'Ctrl+Shift+F', '🔍');
    c(t('全文検索（すべての会話）'), () => OY.search.run({ scope: 'sessions' }), '', '🔍');
    c(t('行へ移動…'), () => OY.palette.open(':'), 'Ctrl+G', '↧');
    c(t('定義へ移動'), () => OY.wb.activeTab()?.inst?.goDef?.(), 'F12', '◆');
    c(t('戻る'), () => OY.code.back(), 'Alt+←', '←');
    c(t('進む'), () => OY.code.forward(), 'Alt+→', '→');
    c(t('リポジトリを追加（ローカル / git clone）'), () => addRepoDialog(), '', '📁');
    c(t('表示: セッション一覧'), () => OY.sidebar.show('sessions'), '', '☰');
    c(t('表示: ツリー'), () => OY.sidebar.show('explorer'), 'Ctrl+Shift+E', '🌲');
    c(t('表示: Git'), () => OY.sidebar.show('git'), 'Ctrl+Shift+G', '⎇');
    c(t('表示: Worktree（一覧と削除）'), () => OY.sidebar.show('worktrees'), '', '⎇');
    c(t('表示: サイドバーの表示 / 非表示'), () => toggleSidebar(), 'Ctrl+B', '◧');
    c(t('表示: 作業ログの表示 / 非表示'), () => { setShowLogs(!state.showLogs); toast(state.showLogs ? t('作業ログを表示します') : t('作業ログを隠しました')); }, '', '👁');
    c(t('表示: ライト / ダーク切替'), () => toggleTheme(), 't', '◐');
    c(t('チャットの入力欄へ'), () => activeChat()?.focusComposer(), 'Ctrl+`', '›', () => !!activeChat());
    c(t('体制図を開く'), () => OY.team.open(activeChat().id), '', '👥', () => !!activeChat()?.session?.subagents);
    c(t('タブを右に分割'), () => OY.wb.splitActive('right'), 'Ctrl+\\', '◫');
    c(t('ペイン配置を初期化（このリポジトリ）'), () => OY.wb.reset(), '', '⟲');
    c(t('完了の音を鳴らす'), () => OY.fx.hyoshigi({ force: true }), '', '🔊');
    c(t('設定'), () => { $('#settings').hidden = false; }, 'Ctrl+,', '⚙');
    c(t('キーボードショートカット一覧'), () => showKeys(), '', '⌨');
  }
  function setShowLogs(v) {
    state.showLogs = v;
    LS.set('showLogs2', v);
    const cb = $('#set-show-logs');
    if (cb) cb.checked = v;
    bus.emit('logs-mode', v);
  }
  /// Drag the sidebar's right edge to resize it; double-click to reset.
  function bindSidebarResize() {
    const root = document.documentElement;
    const apply = (w) => root.style.setProperty('--sb-w', `${w}px`);
    const saved = LS.get('sbWidth', null);
    if (saved) apply(saved);
    const handle = $('#sb-resizer');
    handle.addEventListener('mousedown', (e) => {
      e.preventDefault();
      const startX = e.clientX;
      const startW = $('#sidebar').getBoundingClientRect().width;
      handle.classList.add('dragging');
      document.body.classList.add('resizing-x');
      const move = (ev) => apply(Math.round(Math.min(Math.max(200, startW + ev.clientX - startX), window.innerWidth * 0.6)));
      const up = () => {
        window.removeEventListener('mousemove', move);
        window.removeEventListener('mouseup', up);
        handle.classList.remove('dragging');
        document.body.classList.remove('resizing-x');
        LS.set('sbWidth', Math.round($('#sidebar').getBoundingClientRect().width));
        bus.emit('layout-resized');
      };
      window.addEventListener('mousemove', move);
      window.addEventListener('mouseup', up);
    });
    handle.addEventListener('dblclick', () => { root.style.removeProperty('--sb-w'); LS.del('sbWidth'); bus.emit('layout-resized'); });
  }
  function bind() {
    $('#btn-notify').addEventListener('click', toggleNotify);
    $('#btn-notify').classList.toggle('on', state.notify);
    $('#btn-settings').addEventListener('click', (e) => { e.stopPropagation(); const s = $('#settings'); s.hidden = !s.hidden; });
    $('#set-theme').addEventListener('change', (e) => { state.theme = e.target.value; LS.set('theme', state.theme); applyTheme(); });
    $('#set-accent').addEventListener('click', (e) => { const s = e.target.closest('.swatch'); if (!s) return; state.accent = s.dataset.accent; LS.set('accent', state.accent); applyTheme(); });
    $('#set-font').addEventListener('change', (e) => { state.fontSize = e.target.value; LS.set('fontSize', state.fontSize); applyTheme(); });
    $('#set-width').addEventListener('change', (e) => { state.width = e.target.value; LS.set('width', state.width); applyTheme(); });
    $('#set-show-empty').addEventListener('change', (e) => { state.showEmpty = e.target.checked; LS.set('showEmpty', state.showEmpty); bus.emit('sessions', state.sessions); });
    $('#set-show-logs').addEventListener('change', (e) => setShowLogs(e.target.checked));
    $('#set-stamp').addEventListener('change', (e) => OY.fx.setStamp(e.target.checked));
    $('#set-sound').addEventListener('change', (e) => OY.fx.setSound(e.target.checked));
    $('#set-sound-test').addEventListener('click', () => OY.fx.hyoshigi({ force: true }));
    $('#set-keys').addEventListener('click', () => { $('#settings').hidden = true; showKeys(); });
    $('#set-worktrees').addEventListener('click', () => { $('#settings').hidden = true; OY.sidebar.show('worktrees'); });
    $('#set-reset-layout').addEventListener('click', () => { OY.wb.reset(); $('#settings').hidden = true; });
    $('#set-lang')?.addEventListener('change', async (e) => {
      const lang = e.target.value;
      try { await api.post('/api/config', { lang }); } catch (err) { toast(err.message); }
      OY.i18n.setLang(lang);
      location.reload();
    });
    $('#set-worktree')?.addEventListener('change', async (e) => {
      try { state.config = await api.post('/api/config', { worktree: e.target.checked }); }
      catch (err) { toast(err.message); e.target.checked = !e.target.checked; }
    });
    $('#set-agents')?.addEventListener('change', async (e) => {
      const cb = e.target.closest('input[data-agent]');
      if (!cb) return;
      try {
        state.config = await api.post('/api/config', { agents: { [cb.dataset.agent]: { enabled: cb.checked } } });
        renderSettings();
        await refreshSessions();
        toast(cb.checked ? `${agentLabel(cb.dataset.agent)} ${t("のセッションを表示します")}` : `${agentLabel(cb.dataset.agent)} ${t("のセッションを隠しました")}`);
      } catch (err) { toast(err.message); cb.checked = !cb.checked; }
    });
    $('#btn-new').addEventListener('click', () => newSessionDialog());
    bindSidebarResize();
    if (LS.get('sbHidden', false)) document.body.classList.add('sb-hidden');
    document.addEventListener('keydown', (e) => {
      if (onShortcut(e)) return;
      const typing = /^(INPUT|TEXTAREA|SELECT)$/.test(e.target.tagName) || e.target.isContentEditable || e.target.closest?.('.CodeMirror');
      if (e.key === 'Escape') {
        if (OY.palette.isOpen()) { OY.palette.close(); return; }
        if (!$('#modal').hidden) { $('#modal').hidden = true; return; }
        if (!$('#settings').hidden) { $('#settings').hidden = true; return; }
        if ($$('.popover:not(.settings):not([hidden])').length) { $$('.popover:not(.settings)').forEach((p) => { p.hidden = true; }); return; }
        if (typing) { e.target.blur(); return; }
        document.body.classList.remove('sb-open');
        return;
      }
      if (typing || e.ctrlKey || e.metaKey || e.altKey) return;
      if (e.key === '/') { e.preventDefault(); OY.sidebar.show('sessions'); $('#search').focus(); $('#search').select(); }
      else if (e.key === 't') toggleTheme();
    });
    document.addEventListener('click', (e) => {
      if (!e.target.closest('#settings') && !e.target.closest('#btn-settings')) $('#settings').hidden = true;
      if (!e.target.closest('.popover') && !e.target.closest('[data-pop]')) $$('.popover:not(.settings)').forEach((p) => { p.hidden = true; });
    });
    $('#modal').addEventListener('click', (e) => { if (e.target.id === 'modal') $('#modal').hidden = true; });
    matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => { if (state.theme === 'system') applyTheme(); });
    window.addEventListener('hashchange', handleHash);
    setInterval(() => {
      for (const id of state.openChats) fetch(`/api/sessions/${encodeURIComponent(id)}/touch`, { method: 'POST', headers: { 'X-Oyakata': '1' } }).catch(() => {});
      bus.emit('tick');
    }, 60000);
  }

  async function init() {
    renderSettings();
    applyTheme();
    bind();
    registerCommands();
    OY.wb.init($('#workbench'));
    OY.sidebar.init();
    OY.worktrees.init();
    try { state.config = await api.get('/api/config'); } catch { state.config = {}; }
    renderSettings();
    if (!OY.i18n.chosen()) {
      if (state.config.lang) { OY.i18n.setLang(state.config.lang); if (state.config.lang !== OY.i18n.lang()) { location.reload(); return; } }
      else langDialog();
    }
    await refreshSessions();
    connect();
    OY.wb.restore();
    handleHash();
  }

  window.OY = {
    state, api, bus, LS, $, $$, esc, md, codeBlock, highlight, langFromPath, toast, copyText, modal, confirmDialog, showOutput,
    stripCwd, norm, joinPath, relTo, basename, parentPath, fmtTime, fmtDate, dateKey, ago, fmtDuration, fmtTokens, shortModel, contextWindow,
    sessionTitle, statusLabel, observeMermaid, itemHtml, itemNode, divider, buildRange, toolInputHtml, toolBodyHtml, bindTranscript, enhanceLongMd, decorateText,
    showKeys, toggleSidebar, activeChat,
    isLog, isPrompt, TOOL_ICON, MODELS, EFFORTS, MODES, MODE_CYCLE, DEFAULT_MODE, runDefaults, saveRunDefaults, fillSelect, uuid,
    agentInfo, agentLabel, runnableAgents, defaultAgent, DEFAULT_MODEL, agentDefaultModel, agentDefaultEffort,
    i18n: window.OY_I18N, t: window.OY_I18N.t,
    newSessionDialog, addRepoDialog, folderBrowser, setActiveRepo, followSession, repoOfCwd, workRoot,
    deleteSession, restoreSession, applyTheme, isDark, refreshSessions, refreshRepos, setShowLogs, init,
  };
})();
