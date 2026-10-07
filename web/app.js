/* OYAKATA core: utilities, API, theme, markdown, transcript rendering, event bus, settings.
   workbench.js (panes), chat.js (session view), editors.js (files/diffs/urls) and sidebar.js
   build on window.OY. */
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
    system: { label: 'システムに合わせる', dark: null },
    light: { label: 'ライト', dark: false },
    dark: { label: 'ダーク', dark: true },
    sepia: { label: 'セピア', dark: false },
    'solar-light': { label: 'Solarized ライト', dark: false },
    'solar-dark': { label: 'Solarized ダーク', dark: true },
    nord: { label: 'Nord', dark: true },
    dracula: { label: 'Dracula', dark: true },
    contrast: { label: '高コントラスト（明）', dark: false },
    'contrast-dark': { label: '高コントラスト（暗）', dark: true },
  };
  const ACCENTS = ['amber', 'red', 'blue', 'green', 'violet', 'pink', 'teal'];
  const ACCENT_HEX = { amber: '#d97706', red: '#dc2626', blue: '#2563eb', green: '#059669', violet: '#7c3aed', pink: '#db2777', teal: '#0d9488' };
  const MODELS = [['', '既定のモデル'], ['claude-fable-5-1', 'Fable 5.1'], ['claude-opus-5-5', 'Opus 5.5'], ['claude-sonnet-5-5', 'Sonnet 5.5'], ['claude-haiku-4-5-20251001', 'Haiku 4.5']];
  const MODES = [['', '既定の権限モード'], ['default', 'default（都度確認）'], ['acceptEdits', 'acceptEdits（編集は自動許可）'], ['auto', 'auto（分類器）'], ['plan', 'plan'], ['bypassPermissions', 'bypassPermissions（確認なし）']];
  const EFFORTS = [['', '既定の努力'], ['low', 'low'], ['medium', 'medium'], ['high', 'high'], ['xhigh', 'xhigh'], ['max', 'max']];

  const state = {
    sessions: [], byId: new Map(), repos: [], config: {},
    activeRepo: LS.get('activeRepo', null), followRepo: LS.get('followRepo', true),
    filter: '',
    showEmpty: LS.get('showEmpty', false),
    showThinking: LS.get('showThinking', true),
    showLogs: LS.get('showLogs', true),
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
    root.style.setProperty('--fs', `${state.fontSize}px`);
    $('#hljs-light').disabled = dark;
    $('#hljs-dark').disabled = !dark;
    document.body.classList.toggle('hide-thinking', !state.showThinking);
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
  function codeBlock(code, lang) {
    if (normLang(lang) === 'mermaid') {
      return `<div class="mermaid-block"><pre class="mermaid-src">${esc(code)}</pre><div class="mermaid-out"></div></div>`;
    }
    const h = highlight(code, lang);
    return `<div class="code-block"><div class="code-head"><span>${esc(h.lang)}</span><button type="button" class="copy-btn">コピー</button></div><pre><code class="hljs language-${esc(h.lang)}">${h.html}</code></pre></div>`;
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
      out.innerHTML = `<div class="mermaid-error">Mermaid の描画に失敗: ${esc(err?.message || err)}</div><pre class="mermaid-fallback">${esc(src)}</pre>`;
    }
  }
  function observeMermaid(root) {
    $$('.mermaid-block:not([data-rendered])', root).forEach((b) => mermaidObserver.observe(b));
  }

  // ------------------------------------------------------------------ utils
  const WEEKDAYS = '日月火水木金土';
  function fmtTime(ts) { return ts ? new Date(ts).toLocaleTimeString('ja-JP', { hour: '2-digit', minute: '2-digit' }) : ''; }
  function fmtDate(ts) { const d = new Date(ts); return `${d.getFullYear()}/${d.getMonth() + 1}/${d.getDate()} (${WEEKDAYS[d.getDay()]})`; }
  function dateKey(ts) { const d = new Date(ts); return `${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`; }
  function ago(ts) {
    if (!ts) return '';
    const diff = Date.now() - new Date(ts).getTime();
    const m = Math.floor(diff / 60000);
    if (m < 1) return 'たった今';
    if (m < 60) return `${m}分前`;
    const h = Math.floor(m / 60);
    if (h < 24) return `${h}時間前`;
    const d = Math.floor(h / 24);
    if (d === 1) return '昨日';
    if (d < 7) return `${d}日前`;
    const dt = new Date(ts);
    return `${dt.getMonth() + 1}/${dt.getDate()}`;
  }
  function fmtTokens(n) {
    n = n || 0;
    if (n >= 1e6) return (n / 1e6).toFixed(1) + 'M';
    if (n >= 1e3) return (n / 1e3).toFixed(n >= 1e5 ? 0 : 1) + 'k';
    return String(n);
  }
  function shortModel(m) { return (m || '').replace(/^claude-/, '').replace(/-\d{8}$/, ''); }
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
  function sessionTitle(s) { return s?.title || s?.first_prompt || '（無題）'; }
  function basename(p) { return (p || '').replace(/[\\/]+$/, '').split(/[\\/]/).pop() || p; }
  function statusLabel(s) {
    switch (s) {
      case 'busy': return '稼働中';
      case 'idle': return '待機中';
      case 'waiting': return '許可待ち';
      default: return '終了';
    }
  }
  function toast(msg) {
    let t = $('#toast');
    if (!t) { t = document.createElement('div'); t.id = 'toast'; document.body.appendChild(t); }
    t.textContent = msg;
    t.classList.add('show');
    clearTimeout(toast._timer);
    toast._timer = setTimeout(() => t.classList.remove('show'), 2200);
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
    return best;
  }

  // ------------------------------------------------------------------ modal
  function modal({ title, body, actions = [], wide = false, onOpen }) {
    const m = $('#modal');
    const card = $('#modal-card');
    card.style.width = wide ? 'min(960px, 100%)' : '';
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
          try { const keep = await a.onClick(card, close); if (keep !== true) close(); }
          catch (e) { toast(e.message || String(e)); b.disabled = false; }
        } else close();
      });
      acts.appendChild(b);
    }
    m.hidden = false;
    if (onOpen) onOpen(card, close);
    return close;
  }
  function confirmDialog(title, text, { label = '実行', danger = false } = {}) {
    return new Promise((resolve) => {
      modal({
        title,
        body: `<p>${text}</p>`,
        actions: [
          { label: 'キャンセル', onClick: () => { resolve(false); } },
          { label, primary: !danger, danger, onClick: () => { resolve(true); } },
        ],
      });
    });
  }
  function showOutput(title, text) {
    modal({ title, body: `<pre class="out">${esc(text || '(出力なし)')}</pre>`, actions: [{ label: '閉じる', primary: true }] });
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
  const LOG_KINDS = new Set(['tool', 'thinking', 'note', 'turn_end']);
  function itemNode(i, it, cwd) {
    const el = document.createElement('div');
    el.className = LOG_KINDS.has(it.t) ? 'item log' : 'item';
    el.dataset.idx = i;
    el.innerHTML = itemHtml(it, i, cwd);
    return el;
  }
  function itemHtml(it, i, cwd) {
    switch (it.t) {
      case 'user': return userHtml(it);
      case 'text':
        return `<article class="msg assistant">
          <div class="msg-head"><span class="who">Claude</span>${it.model ? `<span class="model">${esc(shortModel(it.model))}</span>` : ''}<span class="time">${esc(fmtTime(it.ts))}</span><button type="button" class="link-btn copy-md" data-idx="${i}">Markdown をコピー</button></div>
          <div class="body md">${md(it.md)}</div>
        </article>`;
      case 'thinking':
        return `<details class="thinking"><summary>思考 <span class="muted">${it.text.length.toLocaleString()} 文字</span></summary><div class="thinking-body">${esc(it.text)}</div></details>`;
      case 'tool': return toolHtml(it, i, cwd);
      case 'compact':
        return `<div class="divider compact">コンテキストを圧縮${it.pre_tokens ? ` · ${fmtTokens(it.pre_tokens)} → ${fmtTokens(it.post_tokens)} tokens` : ''}${it.trigger ? ` (${esc(it.trigger)})` : ''}</div>`;
      case 'turn_end':
        return `<div class="turn-end">⏱ ${(it.duration_ms / 1000).toFixed(1)}s</div>`;
      case 'note': {
        const m = /<command-name>([^<]*)<\/command-name>/.exec(it.text);
        const label = m ? m[1].trim() : 'ローカルコマンド';
        const out = /<local-command-stdout>([\s\S]*?)<\/local-command-stdout>/.exec(it.text);
        const body = (out ? out[1] : it.text).trim();
        if (!body) return `<div class="note-line">${esc(label)}</div>`;
        return `<details class="note"><summary>${esc(label)}</summary><pre>${esc(body)}</pre></details>`;
      }
      default: return '';
    }
  }
  function userHtml(it) {
    const imgs = (it.images || []).map((im) => `<img class="user-img" src="data:${esc(im.media_type)};base64,${im.data}" alt="添付画像">`).join('');
    if (it.compact_summary) {
      return `<details class="msg user compact-summary"><summary>前のセッションからの引き継ぎ要約</summary><div class="body md">${md(it.text)}</div></details>`;
    }
    if (it.meta) {
      const m = /^\s*<([a-z_-]+)/i.exec(it.text);
      const label = m ? m[1] : 'システム';
      return `<details class="msg user meta"><summary><span class="who">システム</span> ${esc(label)}</summary><pre class="meta-body">${esc(it.text)}</pre></details>`;
    }
    return `<article class="msg user">
      <div class="msg-head"><span class="who">あなた</span><span class="time">${esc(fmtTime(it.ts))}</span></div>
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
      case 'Bash': h += codeBlock(inp.command || '', 'bash'); h += kv('説明', inp.description); break;
      case 'PowerShell': h += codeBlock(inp.command || '', 'powershell'); h += kv('説明', inp.description); break;
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
        h += kv('type', inp.subagent_type); h += kv('説明', inp.description);
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
    if (it.agent_id) h += `<div class="agent-link"><button type="button" class="btn open-agent" data-agent="${esc(it.agent_id)}">サブエージェントの会話を開く</button></div>`;
    const r = it.result;
    if (!r) { h += '<div class="result-head pending">結果待ち…</div>'; return h; }
    h += `<div class="result-head ${r.is_error ? 'err' : ''}">${r.is_error ? 'エラー' : '結果'}${r.truncated ? ' <span class="muted">(先頭 60,000 文字のみ)</span>' : ''}</div>`;
    const text = r.text || '';
    if (text) {
      const shown = full || text.length <= RESULT_PREVIEW ? text : text.slice(0, RESULT_PREVIEW);
      h += `<pre class="tool-result">${esc(shown)}</pre>`;
      if (!full && text.length > RESULT_PREVIEW) h += `<button type="button" class="link-btn show-all">すべて表示 (${text.length.toLocaleString()} 文字)</button>`;
    }
    for (const im of r.images || []) h += `<img class="result-img" src="data:${esc(im.media_type)};base64,${im.data}" alt="ツール結果の画像">`;
    if (!text && !(r.images || []).length) h += '<div class="muted">（出力なし）</div>';
    return h;
  }
  function buildRange(items, from, to, cwd) {
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
      const t = e.target;
      if (t.closest('.copy-btn')) {
        const code = $('code', t.closest('.code-block'));
        if (code) copyText(code.textContent);
      } else if (t.closest('.copy-md')) {
        const it = ctx.items()[+t.closest('.copy-md').dataset.idx];
        if (it?.md) copyText(it.md);
      } else if (t.closest('.show-all')) {
        fill(t.closest('details.tool'), true);
      } else if (t.closest('.open-agent')) {
        OY.editors.openAgent(ctx.sessionId(), t.closest('.open-agent').dataset.agent);
      } else if (t.closest('[data-open-file]')) {
        OY.editors.openFile(t.closest('[data-open-file]').dataset.openFile);
      } else if (t.closest('a.oy-link')) {
        const a = t.closest('a.oy-link');
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
        if (s.status === 'idle' && was === 'busy') notifyEvent(s, `${sessionTitle(s)} が待機中`, (s.last_text_snippet || '').slice(0, 160));
        if (s.status === 'waiting') notifyEvent(s, `${sessionTitle(s)} が${s.waiting_for?.startsWith('question') ? '質問' : '許可'}を待っています`, s.waiting_for || '');
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
    if (waiting) prefix += `[${waiting} 許可待ち] `;
    if (busy) prefix += `(${busy}) `;
    document.title = prefix + 'OYAKATA';
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
      toast('待機・許可待ちになったら通知します');
    }
    LS.set('notify', state.notify);
    $('#btn-notify').classList.toggle('on', state.notify);
  }
  async function refreshSessions() {
    try {
      const [s, r] = await Promise.all([api.get('/api/sessions'), api.get('/api/repos')]);
      state.repos = r.repos || [];
      bus.emit('repos', state.repos);
      applySessions(s.sessions);
    } catch { /* retried by the next SSE event */ }
  }
  function setActiveRepo(root, { explicit = false } = {}) {
    if (!root) return;
    if (explicit) { state.followRepo = false; LS.set('followRepo', false); }
    if (norm(root) === norm(state.activeRepo)) { bus.emit('active-repo', root); return; }
    state.activeRepo = root;
    LS.set('activeRepo', root);
    bus.emit('active-repo', root);
  }

  // --------------------------------------------------------------- composer
  function fillSelect(sel, pairs, value) {
    sel.innerHTML = pairs.map(([v, l]) => `<option value="${esc(v)}">${esc(l)}</option>`).join('');
    sel.value = value || '';
  }
  function runDefaults() {
    return { model: LS.get('runModel', ''), mode: LS.get('runMode', ''), effort: LS.get('runEffort', '') };
  }
  function saveRunDefaults(model, mode, effort) { LS.set('runModel', model); LS.set('runMode', mode); LS.set('runEffort', effort); }
  function modeOptions() {
    return MODES.map(([v, l]) => [v, v === '' ? `既定の権限モード${state.config.default_permission_mode ? `（${state.config.default_permission_mode}）` : ''}` : l]);
  }
  function newSessionDialog(cwd) {
    const roots = state.repos.map((r) => r.root);
    const d = runDefaults();
    modal({
      title: '新しいセッションを始める',
      body: `
        <label class="field"><span>フォルダ</span><input type="text" id="ns-cwd" list="ns-roots" value="${esc(cwd || state.activeRepo || roots[0] || state.config.home || '')}"><datalist id="ns-roots">${roots.map((r) => `<option value="${esc(r)}">`).join('')}</datalist></label>
        <label class="field"><span>最初の指示</span><textarea id="ns-prompt" placeholder="何をしてほしいか"></textarea></label>
        <div class="fields-row">
          <label class="field"><span>モデル</span><select id="ns-model"></select></label>
          <label class="field"><span>権限モード</span><select id="ns-mode"></select></label>
          <label class="field"><span>努力</span><select id="ns-effort"></select></label>
        </div>
        <p class="note">OYAKATA が <code>claude -p</code> を子プロセスとして起動します。権限の確認・質問・計画の承認はチャット内で答えられます。会話は通常どおり ~/.claude/projects に残ります。</p>`,
      actions: [
        { label: 'キャンセル' },
        {
          label: '開始', primary: true,
          onClick: async (card) => {
            const cwd = $('#ns-cwd', card).value.trim();
            const prompt = $('#ns-prompt', card).value.trim();
            if (!cwd || !prompt) throw new Error('フォルダと指示を入力してください');
            const model = $('#ns-model', card).value, mode = $('#ns-mode', card).value, effort = $('#ns-effort', card).value;
            saveRunDefaults(model, mode, effort);
            const r = await api.post('/api/run/start', { cwd, prompt, model: model || undefined, permission_mode: mode || undefined, effort: effort || undefined });
            OY.chat.open(r.session_id);
            toast('セッションを開始しました');
          },
        },
      ],
      onOpen: (card) => {
        fillSelect($('#ns-model', card), MODELS, d.model);
        fillSelect($('#ns-mode', card), modeOptions(), d.mode);
        fillSelect($('#ns-effort', card), EFFORTS, d.effort);
        $('#ns-prompt', card).focus();
      },
    });
  }

  // ------------------------------------------------------------------ events
  let es = null;
  function connect() {
    if (es) es.close();
    es = new EventSource('/api/events');
    es.onopen = () => { $('#conn').textContent = '接続中'; $('#conn').classList.remove('off'); };
    es.onerror = () => { $('#conn').textContent = '再接続中…'; $('#conn').classList.add('off'); };
    es.addEventListener('sessions', (e) => applySessions(JSON.parse(e.data).sessions));
    for (const kind of ['append', 'patch', 'reset', 'run']) {
      es.addEventListener(kind, (e) => bus.emit(kind, JSON.parse(e.data)));
    }
    es.addEventListener('lagged', async () => { await refreshSessions(); bus.emit('lagged'); });
  }
  function handleHash() {
    const m = /^#\/s\/([^/?#]+)/.exec(location.hash);
    if (m) { OY.chat.open(decodeURIComponent(m[1])); return; }
    const r = /^#\/r\/(.+)$/.exec(location.hash);
    if (r) { setActiveRepo(decodeURIComponent(r[1]), { explicit: true }); OY.sidebar.show('explorer'); }
  }

  // --------------------------------------------------------------- settings
  function renderSettings() {
    const sel = $('#set-theme');
    sel.innerHTML = Object.entries(THEMES).map(([k, t]) => `<option value="${k}">${esc(t.label)}</option>`).join('');
    sel.value = state.theme;
    $('#set-accent').innerHTML = ACCENTS.map((a) => `<span class="swatch${a === state.accent ? ' on' : ''}" data-accent="${a}" style="background:${ACCENT_HEX[a]}" title="${a}"></span>`).join('');
    $('#set-font').value = state.fontSize;
    $('#set-show-empty').checked = state.showEmpty;
    $('#set-show-thinking').checked = state.showThinking;
    $('#set-show-logs').checked = state.showLogs;
  }
  function setShowLogs(v) {
    state.showLogs = v;
    LS.set('showLogs', v);
    const cb = $('#set-show-logs');
    if (cb) cb.checked = v;
    bus.emit('logs-mode', v);
  }
  function bind() {
    $('#btn-notify').addEventListener('click', toggleNotify);
    $('#btn-notify').classList.toggle('on', state.notify);
    $('#btn-settings').addEventListener('click', (e) => { e.stopPropagation(); const s = $('#settings'); s.hidden = !s.hidden; });
    $('#set-theme').addEventListener('change', (e) => { state.theme = e.target.value; LS.set('theme', state.theme); applyTheme(); });
    $('#set-accent').addEventListener('click', (e) => { const s = e.target.closest('.swatch'); if (!s) return; state.accent = s.dataset.accent; LS.set('accent', state.accent); applyTheme(); });
    $('#set-font').addEventListener('change', (e) => { state.fontSize = e.target.value; LS.set('fontSize', state.fontSize); applyTheme(); });
    $('#set-show-empty').addEventListener('change', (e) => { state.showEmpty = e.target.checked; LS.set('showEmpty', state.showEmpty); bus.emit('sessions', state.sessions); });
    $('#set-show-thinking').addEventListener('change', (e) => { state.showThinking = e.target.checked; LS.set('showThinking', state.showThinking); applyTheme(); });
    $('#set-show-logs').addEventListener('change', (e) => setShowLogs(e.target.checked));
    $('#set-reset-layout').addEventListener('click', () => { OY.wb.reset(); $('#settings').hidden = true; });
    $('#btn-new').addEventListener('click', () => newSessionDialog(state.activeRepo));
    document.addEventListener('keydown', (e) => {
      const typing = /^(INPUT|TEXTAREA|SELECT)$/.test(e.target.tagName) || e.target.isContentEditable || e.target.closest?.('.CodeMirror');
      if (e.key === 'Escape') {
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
      if (!e.target.closest('.chat-head .popover') && !e.target.closest('.chat-head .btn')) $$('.chat-head .popover').forEach((p) => { p.hidden = true; });
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
    OY.wb.init($('#workbench'));
    OY.sidebar.init();
    try { state.config = await api.get('/api/config'); } catch { state.config = {}; }
    await refreshSessions();
    connect();
    OY.wb.restore();
    handleHash();
  }

  window.OY = {
    state, api, bus, LS, $, $$, esc, md, codeBlock, highlight, langFromPath, toast, copyText, modal, confirmDialog, showOutput,
    stripCwd, norm, joinPath, relTo, basename, fmtTime, fmtDate, dateKey, ago, fmtTokens, shortModel, sessionTitle, statusLabel,
    observeMermaid, itemHtml, itemNode, divider, buildRange, toolInputHtml, toolBodyHtml, bindTranscript, TOOL_ICON,
    MODELS, EFFORTS, modeOptions, runDefaults, saveRunDefaults, fillSelect, newSessionDialog, setActiveRepo, repoOfCwd,
    applyTheme, isDark, refreshSessions, setShowLogs, init,
  };
})();
