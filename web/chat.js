/* OYAKATA chat pane: one Claude Code session rendered as a conversation, with a composer
   and status line laid out like Claude Code's terminal (permission mode under the input,
   model and context usage on the right), permission / question / plan cards, a sticky
   "current prompt" ribbon and a turn rail for long sessions. Several can be open at once.
   A chat can also start as a draft (folder chosen, nothing sent yet). */
(() => {
  'use strict';
  const {
    $, $$, esc, api, state, bus, md, toast, sessionTitle, statusLabel, stripCwd, fmtTokens, fmtDuration, shortModel, buildRange,
    observeMermaid, bindTranscript, TOOL_ICON, toolInputHtml, MODELS, EFFORTS, MODES, MODE_CYCLE, DEFAULT_MODE, copyText, relTo,
    contextWindow, basename, isPrompt, repoOfCwd, agentInfo, agentLabel,
  } = OY;

  const INITIAL_WINDOW = 300;
  const PAGE = 300;
  const instances = new Map();

  const TEMPLATE = `
    <div class="chat-head">
      <span class="st-dot" title=""></span>
      <h2 class="chat-title"></h2>
      <span class="ch-repo"></span>
      <span class="ch-agent chip tiny agent" hidden></span>
      <button type="button" class="head-btn b-team" data-pop hidden title="${t("体制図: サブエージェントの構成と、それぞれの役割・作業状況")}">👥 <span class="n"></span></button>
      <button type="button" class="head-btn b-more" data-pop title="${t("メニュー")}">⋯</button>
      <div class="popover pop-menu" hidden></div>
      <div class="popover pop-outline" hidden></div>
      <div class="popover pop-files" hidden></div>
      <div class="popover pop-artifacts" hidden></div>
    </div>
    <div class="chat-body">
      <button type="button" class="turn-ribbon" hidden></button>
      <div class="chat-scroller">
        <div class="load-more" hidden><button type="button" class="btn">${t("さらに前を表示")}</button></div>
        <div class="transcript"></div>
        <div class="welcome" hidden></div>
      </div>
      <div class="turn-rail" hidden></div>
      <button type="button" class="jump-latest" hidden>${t("新しい出力 ↓")}</button>
    </div>
    <div class="composer">
      <div class="pending-cards"></div>
      <div class="queued-msgs"></div>
      <div class="activity" hidden><span class="act-glyph">✻</span><span class="act-verb"></span><span class="act-detail"></span><span class="act-meta"></span></div>
      <div class="prompt-box">
        <span class="prompt-glyph">&gt;</span>
        <textarea rows="1" spellcheck="false"></textarea>
        <button type="button" class="send-btn" title="${t("送信（Enter）")}">↵</button>
      </div>
      <div class="statusline">
        <button type="button" class="sl-mode" data-pop></button>
        <span class="sl-hint"></span>
        <button type="button" class="sl-wt" hidden></button>
        <span class="spacer"></span>
        <button type="button" class="sl-model" data-pop></button>
        <button type="button" class="sl-effort" data-pop></button>
        <span class="sl-ctx" hidden><span class="ctx-bar"><i></i></span><span class="ctx-text"></span></span>
      </div>
      <div class="popover pop-sl" hidden></div>
    </div>`;

  class Chat {
    constructor(id, key, data = {}) {
      this.id = id;
      this.key = key;
      this.draftCwd = data.draft ? data.cwd : null;
      this.items = [];
      this.agents = [];
      this.session = state.byId.get(id) || null;
      this.agent = this.session?.agent || data.agent || 'claude';
      this.run = null;
      // Choices for a draft or a resumed session; a running one takes them from Claude Code.
      this.opts = { model: '', mode: DEFAULT_MODE, effort: '', worktree: state.config.worktree !== false };
      this.renderedFrom = 0;
      this.lastDateKey = null;
      this.follow = true;
      this.loadSeq = 0;
      this.sending = false;
      this.starting = false;
      this.awaitingTranscript = false;
      this.pendingKey = '';
      this.queuedKey = '';
      this.turnEls = [];
      this.lastStatus = this.session?.status || null;
      this.doneFlash = null;
      this.el = document.createElement('div');
      this.el.className = 'chat';
      this.el.innerHTML = TEMPLATE;
      this.q = (sel) => $(sel, this.el);
      this.scroller = this.q('.chat-scroller');
      this.transcript = this.q('.transcript');
      this.ta = this.q('textarea');
      this.el.classList.toggle('conv-only', !state.showLogs);
      this.activityTimer = setInterval(() => this.updateActivity(), 1000);
      this.unsubs = [
        bus.on('logs-mode', (v) => { this.el.classList.toggle('conv-only', !v); this.refreshTurns(); if (this.follow) this.scrollToBottom(); }),
        bus.on('sessions', () => this.onSessions()),
        bus.on('append', (d) => { if (d.session === this.id) this.onAppend(d); }),
        bus.on('patch', (d) => { if (d.session === this.id) this.patchItem(d.index, d.item); }),
        bus.on('reset', (d) => { if (d.session === this.id) this.load(); }),
        bus.on('run', (d) => { if (d.run?.session_id === this.id) this.onRun(d.run); }),
        bus.on('lagged', () => this.load()),
      ];
      bindTranscript(this.scroller, { items: () => this.items, cwd: () => this.cwd(), sessionId: () => this.id });
      bindTranscript(this.q('.pending-cards'), { items: () => [], cwd: () => this.cwd(), sessionId: () => this.id });
      this.bindUi();
      state.openChats.add(id);
      instances.set(id, this);
      this.load();
    }

    cwd() { return this.session?.cwd || this.draftCwd; }
    isDraft() { return !this.session && !!this.draftCwd; }
    /// The agent's display name, for message headers and hints.
    who() { return agentLabel(this.agent); }
    agentCfg() { return agentInfo(this.agent) || {}; }
    /// Only Claude Code asks for permissions and takes prompts mid-turn; the others run a
    /// turn per process and get the next prompt when it ends.
    interactive() { return this.agent === 'claude'; }
    /// Agents that assign their own session id answer `run/start` with it: move this chat
    /// (and its tab) over to that id.
    rekey(newId) {
      if (!newId || newId === this.id) return;
      const oldKey = this.key;
      instances.delete(this.id);
      state.openChats.delete(this.id);
      this.id = newId;
      this.key = 'chat:' + newId;
      instances.set(newId, this);
      state.openChats.add(newId);
      OY.wb.rekey(oldKey, this.key, { id: newId, agent: this.agent });
      const s = state.byId.get(newId);
      if (s) { this.session = s; this.lastStatus = s.status; }
    }

    // ------------------------------------------------------------ loading
    async load() {
      const seq = ++this.loadSeq;
      this.q('.load-more').hidden = true;
      const known = state.byId.get(this.id);
      if (known) this.session = known;
      this.renderHeader();
      this.renderComposer();
      if (!known || known.size === 0) {
        if (this.isDraft() || known) {
          this.transcript.innerHTML = '';
          this.items = [];
          this.awaitingTranscript = true;
          this.showWelcome();
          return;
        }
      }
      this.transcript.innerHTML = t('<div class="loading">読み込み中…</div>');
      let data;
      try {
        data = await api.get(`/api/sessions/${encodeURIComponent(this.id)}`);
      } catch (e) {
        if (seq !== this.loadSeq) return;
        this.transcript.innerHTML = `<div class="loading err">${t("読み込みに失敗しました:")} ${esc(e.message)}</div>`;
        return;
      }
      if (seq !== this.loadSeq) return;
      this.awaitingTranscript = false;
      this.hideWelcome();
      this.items = data.items || [];
      this.agents = data.agents || [];
      this.session = data.session;
      this.run = data.run || null;
      this.lastStatus = this.session?.status;
      this.renderHeader();
      this.renderComposer();
      this.renderInitial();
    }
    showWelcome() {
      const w = this.q('.welcome');
      const cwd = this.cwd();
      const repo = this.session?.repo || repoOfCwd(cwd);
      const name = repo?.name || basename(cwd || '');
      const starting = this.starting || (this.session && this.session.status !== 'ended');
      w.innerHTML = `<img class="brand-mark big" src="/assets/icon.svg" alt="">
        <div class="wl-greet">${starting ? t('開始しています…') : t('何をしましょう？')}</div>
        <div class="wl-where"><span class="wl-repo">📁 ${esc(name)}</span><span class="wl-branch"></span><div class="wl-path">${esc(cwd || '')}</div></div>
        ${this.pendingPrompt ? `<div class="msg user"><div class="bubble">${esc(this.pendingPrompt)}</div></div>` : ''}
        ${starting ? '' : t('<div class="wl-tips"><kbd>Enter</kbd> 送信 · <kbd>Shift</kbd>+<kbd>Enter</kbd> 改行 · <kbd>Shift</kbd>+<kbd>Tab</kbd> 権限モード · <kbd>Esc</kbd> 中断</div>')}`;
      w.hidden = false;
      const root = repo?.worktree || repo?.root;
      if (this.isDraft() && !starting && this.opts.worktree) {
        const b = $('.wl-branch', w);
        if (b) b.textContent = t('⎇ 新しい worktree で始めます');
      } else if (root) {
        api.get(`/api/git/status?root=${encodeURIComponent(root)}`).then((st) => {
          const b = $('.wl-branch', w);
          if (b && st.branch) b.textContent = `⎇ ${st.branch}${st.entries?.length ? ` ${t("· 変更")} ${st.entries.length}` : ''}`;
        }).catch(() => {});
      }
    }
    hideWelcome() { this.q('.welcome').hidden = true; this.pendingPrompt = null; }
    /// Scroll to the message written at `ts` (from the conversation search) and flash it.
    revealTs(ts) {
      if (!ts) return;
      const idx = this.items.findIndex((it) => it.ts === ts && (it.t === 'text' || it.t === 'user'));
      if (idx < 0) { this.pendingTs = ts; return; }
      this.pendingTs = null;
      this.jumpTo(idx);
      const el = $(`.item[data-idx="${idx}"]`, this.transcript);
      if (el) { el.classList.remove('flash'); void el.offsetWidth; el.classList.add('flash'); }
    }
    focusComposer() { if (!this.ta.disabled) this.ta.focus(); }
    onShow() {
      state.lastChat = this.id;
      if (this.follow) this.scrollToBottom();
      this.updateRibbon();
    }
    onSessions() {
      const s = state.byId.get(this.id);
      if (!s) return;
      const was = this.lastStatus;
      this.session = s;
      if (s.agent) this.agent = s.agent;
      if (was === 'busy' && s.status === 'idle') this.flashDone();
      this.lastStatus = s.status;
      if (s.owner === 'oyakata') this.starting = false;
      this.renderHeader();
      this.renderComposer();
      this.updateActivity();
      OY.wb.setTitle(this.key, sessionTitle(s));
      if (this.awaitingTranscript && s.size > 0) this.load();
    }
    onAppend(d) {
      if (this.awaitingTranscript) { this.load(); return; }
      if (d.start !== this.items.length) { this.load(); return; }
      this.appendItems(d.start, d.items);
    }
    onRun(run) {
      this.run = run;
      if (this.session) {
        const was = this.session.status;
        this.session.owner = run.status === 'exited' ? null : 'oyakata';
        this.session.status = run.status === 'exited' ? 'ended' : run.status;
        if (run.permission_mode) this.session.permission_mode = run.permission_mode;
        if (was === 'busy' && this.session.status === 'idle') this.flashDone();
        this.lastStatus = this.session.status;
        this.renderHeader();
      }
      this.renderComposer();
      this.updateActivity();
      if (run.last_error && run.status !== 'busy') toast(run.last_error);
    }
    dispose() {
      for (const u of this.unsubs) u();
      clearInterval(this.activityTimer);
      state.openChats.delete(this.id);
      if (instances.get(this.id) === this) instances.delete(this.id);
    }

    // ------------------------------------------------------- activity line
    turnStart() {
      for (let i = this.items.length - 1; i >= 0; i--) if (isPrompt(this.items[i])) return { idx: i, ts: this.items[i].ts };
      return { idx: 0, ts: this.session?.last_at || null };
    }
    flashDone() {
      const turn = this.turnStart();
      const secs = turn.ts ? (Date.now() - new Date(turn.ts).getTime()) / 1000 : null;
      this.doneFlash = { until: Date.now() + 6000, text: `${t('完了')}${secs != null ? `（${fmtDuration(secs)}）` : ''}` };
      this.updateActivity();
    }
    /// The line above the input that says what the agent is doing, like the terminal's spinner:
    /// "✻ 作業中… Bash: cargo test (12秒 · esc で中断)".
    updateActivity() {
      const el = this.q('.activity');
      const s = this.session;
      const set = (cls, glyph, verb, detail, meta) => {
        el.className = 'activity ' + cls;
        this.q('.act-glyph').textContent = glyph;
        this.q('.act-verb').textContent = verb;
        this.q('.act-detail').textContent = detail || '';
        this.q('.act-meta').textContent = meta || '';
        el.hidden = false;
      };
      if (this.doneFlash && Date.now() < this.doneFlash.until && s?.status === 'idle') { set('done', '✓', this.doneFlash.text, '', ''); return; }
      this.doneFlash = null;
      if (this.starting && !s) { set('busy', '✻', `${t('作業中')}…`, '', ''); return; }
      if (!s || (s.status !== 'busy' && s.status !== 'waiting')) { el.hidden = true; return; }
      if (s.status === 'waiting') {
        const what = s.waiting_for ? s.waiting_for.replace(/^permission: /, '').replace(/^question$/, t('質問')) : '';
        set('waiting', '✋', t('確認待ち'), what, s.owner === 'terminal' ? t('（ターミナルで答えてください）') : '');
        return;
      }
      const turn = this.turnStart();
      let detail = '';
      for (let i = this.items.length - 1; i >= 0 && i >= this.items.length - 40; i--) {
        const it = this.items[i];
        if (it.t === 'tool' && !it.result) { detail = `${it.name}: ${stripCwd(it.summary, this.cwd())}`; break; }
        if (it.t === 'text' || isPrompt(it)) break;
      }
      if (!detail && s.owner === 'oyakata' && this.run?.last_tool) detail = stripCwd(this.run.last_tool, this.cwd());
      const secs = turn.ts ? (Date.now() - new Date(turn.ts).getTime()) / 1000 : null;
      const canInterrupt = s.owner === 'oyakata' || (s.owner === 'terminal' && s.live?.typeable && state.config.can_type !== false);
      const queued = s.owner === 'oyakata' ? (this.run?.queued?.length || 0) : 0;
      const meta = [secs != null ? fmtDuration(secs) : '', queued ? `${t("指示")} ${queued} ${t("件待機中")}` : '', canInterrupt ? t('esc で中断') : ''].filter(Boolean).join(' · ');
      set('busy', '✻', `${t('作業中')}…`, detail, meta ? `(${meta})` : '');
    }

    // ------------------------------------------------------------- header
    renderHeader() {
      const s = this.session;
      const title = s ? sessionTitle(s) : t('新しいセッション');
      this.q('.chat-title').textContent = s?.title === t('（開始中）') && this.pendingPrompt ? this.pendingPrompt.slice(0, 60) : title;
      const status = s?.status || (this.isDraft() ? 'draft' : 'ended');
      const dot = this.q('.st-dot');
      dot.className = 'st-dot ' + status;
      dot.title = this.isDraft() ? t('未開始') : `${statusLabel(s?.status)}${s?.owner === 'oyakata' ? t('（OYAKATA が実行）') : s?.owner === 'terminal' ? t('（ターミナルで実行）') : s?.owner === 'external' ? t('（別のプロセスで実行）') : ''}`;
      const agentEl = this.q('.ch-agent');
      agentEl.hidden = this.agent === 'claude';
      agentEl.textContent = this.who();
      const repo = s?.repo || repoOfCwd(this.cwd());
      const r = this.q('.ch-repo');
      r.textContent = (repo?.name ? repo.name + (repo.subdir ? '/' + repo.subdir : '') : basename(this.cwd() || '')) + (repo?.worktree ? ` ⎇ ${repo.branch || basename(repo.worktree)}` : '');
      r.title = this.cwd() || '';
      const team = this.q('.b-team');
      team.hidden = !s?.subagents;
      $('.n', team).textContent = s?.subagents || '';
      this.renderStatusline();
    }
    closePopovers() { $$('.popover', this.el).forEach((p) => { p.hidden = true; }); }
    togglePopover(sel, build, anchor) {
      const o = this.q(sel);
      const wasOpen = !o.hidden;
      $$('.popover:not(.settings)').forEach((p) => { p.hidden = true; });
      if (wasOpen) return;
      o.innerHTML = build();
      o.hidden = false;
      if (anchor && o.classList.contains('pop-sl')) {
        const box = this.q('.composer').getBoundingClientRect();
        const a = anchor.getBoundingClientRect();
        const left = Math.max(8, Math.min(a.left - box.left, box.width - o.offsetWidth - 8));
        o.style.left = left + 'px';
      }
    }
    menuHtml() {
      const s = this.session;
      const row = (act, icon, label, extra = '', cls = '') => `<a href="#" data-act="${act}" class="${cls}"><span class="mi">${icon}</span><span class="ml">${label}</span>${extra}</a>`;
      let h = '';
      h += row('outline', '🧭', t('プロンプト一覧'));
      if (s?.edited_files?.length) h += row('files', '📝', t('変更したファイル'), `<span class="mc">${s.edited_files.length}</span>`);
      if (s?.artifacts?.length) h += row('artifacts', '🧩', t('アーティファクト'), `<span class="mc">${s.artifacts.length}</span>`);
      if (s?.subagents) h += row('team', '👥', t('体制図'), `<span class="mc">${s.subagents}</span>`);
      h += row('logs', state.showLogs ? '🙈' : '👁', state.showLogs ? t('作業ログを隠す') : t('作業ログを表示'));
      h += row('tree', '🌲', t('このリポジトリのツリーと Git'));
      if (s?.repo?.worktree && s.status === 'ended') h += row('wt-remove', '🧹', t('worktree を削除'));
      if (s) h += row('copy-id', '📋', t('セッション ID をコピー'));
      if (s?.owner === 'oyakata') h += '<div class="msep"></div>' + row('end', '⏹', t('セッションを終了'), '', 'danger');
      if (s) h += (s.owner === 'oyakata' ? '' : '<div class="msep"></div>') + row('delete', '🗑', t('セッションを削除'), '', 'danger');
      return h;
    }
    outlineHtml() {
      const rows = [];
      this.items.forEach((it, i) => {
        if (isPrompt(it)) rows.push(`<a href="#" data-jump="${i}"><span class="n">${rows.length + 1}</span>${esc(it.text.slice(0, 120))}</a>`);
      });
      return rows.length ? rows.join('') : t('<div class="sb-empty">プロンプトがありません</div>');
    }
    filesHtml() {
      const s = this.session;
      const root = OY.workRoot(s);
      return t('<div class="ptitle">このセッションが編集したファイル</div>') + (s?.edited_files || []).map((f) => {
        const rel = relTo(root, f.path);
        return `<div class="prow" data-path="${esc(f.path)}" data-rel="${esc(rel || '')}" draggable="true"><span class="pp" title="${esc(f.path)}">${esc(stripCwd(f.path, s.cwd))}</span><span class="pc">${f.edits ? `${t("編集")} ${f.edits}` : ''}${f.edits && f.writes ? ' · ' : ''}${f.writes ? `${t("書込")} ${f.writes}` : ''}</span>${rel ? t('<button type="button" class="btn open-diff">差分</button>') : ''}<button type="button" class="btn open-file">${t("開く")}</button></div>`;
      }).join('');
    }
    artifactsHtml() {
      return t('<div class="ptitle">アーティファクト（claude.ai）</div>') + (this.session?.artifacts || []).map((u) =>
        `<div class="prow" data-url="${esc(u)}"><span class="pp">${esc(u)}</span><button type="button" class="btn open-url">${t("開く")}</button><button type="button" class="btn copy-url">${t("コピー")}</button></div>`).join('');
    }
    async menuAction(act) {
      this.closePopovers();
      const s = this.session;
      if (act === 'outline') this.togglePopover('.pop-outline', () => this.outlineHtml());
      else if (act === 'files') this.togglePopover('.pop-files', () => this.filesHtml());
      else if (act === 'artifacts') this.togglePopover('.pop-artifacts', () => this.artifactsHtml());
      else if (act === 'team') OY.team.open(this.id);
      else if (act === 'logs') { OY.setShowLogs(!state.showLogs); toast(state.showLogs ? t('作業ログを表示します') : t('作業ログを隠しました（会話だけを表示）')); }
      else if (act === 'tree') { if (this.session) OY.followSession(this.session); OY.sidebar.show('explorer'); }
      else if (act === 'wt-remove' && s?.repo?.worktree) this.removeWorktree(s.repo.worktree, s.repo.branch);
      else if (act === 'copy-id') copyText(this.id);
      else if (act === 'end') {
        if (!(await OY.confirmDialog(t('セッションを終了'), `${t("OYAKATA 側の")} ${this.who()} ${t("プロセスを終了します。会話は残るので、あとからこの画面で続きを送れば再開できます。")}`, { label: t('終了'), danger: true }))) return;
        try { await api.post(`/api/run/${encodeURIComponent(this.id)}/stop`); } catch (e) { toast(e.message); }
      } else if (act === 'delete' && s) OY.deleteSession(this.id);
    }
    /// Remove this session's worktree (and its branch, if merged). Uncommitted changes make
    /// git refuse; then ask again before forcing.
    async removeWorktree(path, branch) {
      const msg = `${esc(path)}<br>${t('この worktree のフォルダを消します。ブランチ')} <code>${esc(branch || '')}</code> ${t('は、マージ済みなら一緒に消し、そうでなければ残します。会話の記録は残ります。')}`;
      if (!(await OY.confirmDialog(t('worktree を削除'), msg, { label: t('削除'), danger: true }))) return;
      const done = (r) => toast(r.branch_deleted ? t('worktree とブランチを削除しました') : r.branch ? `${t('worktree を削除しました。ブランチは残しています:')} ${r.branch}` : t('worktree を削除しました'));
      try { done(await api.post('/api/worktree/remove', { path })); return; }
      catch (e) {
        if (e.status !== 409) { toast(e.message); return; }
        if (!(await OY.confirmDialog(t('未コミットの変更があります'), `${esc(e.message)}<br>${t('変更を捨てて削除しますか？')}`, { label: t('変更を捨てて削除'), danger: true }))) return;
      }
      try { done(await api.post('/api/worktree/remove', { path, force: true })); } catch (e) { toast(e.message); }
    }
    jumpTo(idx) {
      while (this.renderedFrom > idx) this.loadMore(true);
      const el = $(`.item[data-idx="${idx}"]`, this.transcript);
      if (el) { el.scrollIntoView({ block: 'start', behavior: 'smooth' }); this.follow = false; }
    }

    // ----------------------------------------------------------- status line
    /// What the status line shows: a running session reports its own mode/model; a draft or an
    /// ended session shows what will be used on (re)start; a terminal session is read-only.
    current() {
      const s = this.session;
      const r = this.run && this.run.status !== 'exited' ? this.run : null;
      if (s?.owner === 'oyakata' || (r && !s)) {
        return { mode: r?.permission_mode || s?.permission_mode || DEFAULT_MODE, model: r?.model || s?.model || '', effort: r?.effort || s?.effort || '', edit: 'live' };
      }
      if (s?.owner === 'terminal' || s?.owner === 'external') return { mode: s.permission_mode || '', model: s.model || '', effort: s.effort || '', edit: null };
      return { mode: this.opts.mode, model: this.opts.model || s?.model || '', effort: this.opts.effort || s?.effort || '', edit: 'local' };
    }
    renderStatusline() {
      const c = this.current();
      const s = this.session;
      const mb = this.q('.sl-mode');
      const mi = MODES[c.mode];
      mb.className = `sl-mode m-${c.mode || 'unknown'}`;
      mb.innerHTML = mi ? `<span class="glyph">${mi.glyph}</span> ${esc(mi.label)}${c.edit ? t(' <span class="kh">(shift+tab で切替)</span>') : ''}` : (c.mode ? esc(c.mode) : t('<span class="kh">権限モード不明</span>'));
      mb.disabled = !c.edit;
      mb.title = mi ? mi.desc : '';
      // Worktree: a toggle while the session is a draft, the branch afterwards.
      const wt = this.q('.sl-wt');
      const srepo = s?.repo;
      if (this.isDraft()) {
        wt.hidden = false;
        wt.disabled = this.starting;
        wt.className = 'sl-wt' + (this.opts.worktree ? ' on' : '');
        wt.textContent = this.opts.worktree ? t('⎇ worktree で作業') : t('⎇ worktree なし');
        wt.title = this.opts.worktree ? t('新しいブランチの worktree を作り、その中で作業します（Git リポジトリのとき）。クリックで切替') : t('このフォルダでそのまま作業します。クリックで worktree を使う');
      } else if (srepo?.worktree) {
        wt.hidden = false;
        wt.disabled = true;
        wt.className = 'sl-wt on';
        wt.textContent = `⎇ ${srepo.branch || basename(srepo.worktree)}`;
        wt.title = srepo.worktree;
      } else wt.hidden = true;
      const hint = this.q('.sl-hint');
      hint.textContent = s?.owner === 'terminal' ? t('ターミナルで実行中') : s?.owner === 'external' ? t('別のプロセスで実行中') : (s && s.status === 'ended' && !this.isDraft()) ? t('再開時に適用') : (!this.interactive() && c.edit === 'live') ? t('次のターンから適用') : '';
      const model = this.q('.sl-model');
      model.textContent = c.model ? shortModel(c.model) : t('既定のモデル');
      model.disabled = !c.edit;
      model.title = c.edit ? t('モデルを切り替える') : t('モデル');
      const effort = this.q('.sl-effort');
      effort.hidden = (!c.effort && c.edit !== 'local') || this.agent === 'gemini';
      effort.textContent = c.effort ? `effort ${c.effort}` : t('effort 既定');
      effort.disabled = c.edit !== 'local';
      effort.title = c.edit === 'local' ? t('努力レベル（開始・再開時に適用）') : t('努力レベル');
      // Context usage: tokens in the window after the latest response.
      const ctx = this.q('.sl-ctx');
      const used = s?.context_tokens || 0;
      const win = contextWindow(c.model || s?.model, s?.context_window || this.run?.context_window, used);
      if (!used || !win) { ctx.hidden = true; return; }
      const pct = Math.min(100, (used / win) * 100);
      ctx.hidden = false;
      ctx.className = 'sl-ctx ' + (pct >= 85 ? 'hot' : pct >= 65 ? 'warn' : 'ok');
      $('.ctx-bar i', ctx).style.width = pct.toFixed(1) + '%';
      $('.ctx-text', ctx).textContent = `ctx ${pct < 1 ? pct.toFixed(1) : Math.round(pct)}% · ${fmtTokens(used)}/${fmtTokens(win)}`;
      ctx.title = `${t("コンテキスト使用量:")} ${used.toLocaleString()} / ${win.toLocaleString()} ${t("トークン（残り")} ${Math.max(0, win - used).toLocaleString()}${t("）\n上限に近づくと Claude Code が自動で圧縮します。")}${s?.compactions ? `${t("\nこのセッションの圧縮:")} ${s.compactions} ${t("回")}` : ''}`;
    }
    modeMenuHtml() {
      const c = this.current();
      return t('<div class="ptitle">権限モード</div>') + Object.entries(MODES).map(([k, m]) =>
        `<a href="#" data-mode="${k}" class="${k === c.mode ? 'cur' : ''}"><span class="mi m-${k}">${m.glyph}</span><span class="ml"><b>${esc(k)}</b><span class="md2">${esc(m.desc)}</span></span>${k === c.mode ? '<span class="mc">✓</span>' : ''}</a>`).join('');
    }
    modelMenuHtml() {
      const c = this.current();
      if (!this.interactive()) {
        // Other agents accept whatever model name their CLI knows; offer the default and a box.
        const cur = !c.model ? ' class="cur"' : '';
        return `<div class="ptitle">${t("モデル（")}${esc(this.who())}）</div><a href="#" data-model=""${cur}><span class="ml">${t("既定のモデル")}</span>${!c.model ? '<span class="mc">✓</span>' : ''}</a>
          <div class="pinput"><input type="text" class="model-input" placeholder="${t("モデル名を入力して Enter（例: gpt-5.5 / pro / provider/model）")}" value="${esc(c.model || '')}" spellcheck="false"></div>`;
      }
      return t('<div class="ptitle">モデル</div>') + MODELS.map(([v, l]) =>
        `<a href="#" data-model="${esc(v)}" class="${v === c.model || (!v && !c.model) ? 'cur' : ''}"><span class="ml">${esc(l)}</span>${v ? `<span class="md2">${esc(v)}</span>` : ''}${v === c.model || (!v && !c.model) ? '<span class="mc">✓</span>' : ''}</a>`).join('');
    }
    effortMenuHtml() {
      const c = this.current();
      return t('<div class="ptitle">努力レベル</div>') + EFFORTS.map(([v, l]) => `<a href="#" data-effort="${esc(v)}" class="${v === (c.effort || '') ? 'cur' : ''}"><span class="ml">${esc(l)}</span>${v === (c.effort || '') ? '<span class="mc">✓</span>' : ''}</a>`).join('');
    }
    async setMode(mode) {
      const c = this.current();
      if (!c.edit || !MODES[mode]) return;
      if (c.edit === 'live') {
        try { await api.post(`/api/run/${encodeURIComponent(this.id)}/mode`, { mode }); if (this.run) this.run.permission_mode = mode; if (this.session) this.session.permission_mode = mode; }
        catch (e) { toast(e.message); return; }
      } else this.opts.mode = mode;
      this.renderStatusline();
    }
    cycleMode() {
      const c = this.current();
      const i = MODE_CYCLE.indexOf(c.mode);
      this.setMode(MODE_CYCLE[(i + 1) % MODE_CYCLE.length]);
    }
    async setModel(model) {
      const c = this.current();
      if (!c.edit) return;
      if (c.edit === 'live') {
        try { await api.post(`/api/run/${encodeURIComponent(this.id)}/model`, { model: model || undefined }); if (this.run) this.run.model = model || null; toast(`${t("モデルを")} ${model ? shortModel(model) : t('既定')} ${t("に切り替えました")}`); }
        catch (e) { toast(e.message); return; }
      } else this.opts.model = model;
      this.renderStatusline();
    }

    // --------------------------------------------------------- transcript
    renderInitial() {
      this.transcript.innerHTML = '';
      const from = Math.max(0, this.items.length - INITIAL_WINDOW);
      this.renderedFrom = from;
      const { frag, lastDate } = buildRange(this.items, from, this.items.length, this.cwd(), this.who());
      this.lastDateKey = lastDate;
      this.transcript.appendChild(frag);
      observeMermaid(this.transcript);
      this.q('.load-more').hidden = from === 0;
      this.follow = true;
      this.q('.jump-latest').hidden = true;
      this.refreshTurns();
      this.updateActivity();
      this.scrollToBottom();
      if (this.pendingTs) this.revealTs(this.pendingTs);
    }
    loadMore(silent = false) {
      if (this.renderedFrom === 0) return;
      const sc = this.scroller;
      const before = sc.scrollHeight;
      const newFrom = Math.max(0, this.renderedFrom - PAGE);
      const { frag } = buildRange(this.items, newFrom, this.renderedFrom, this.cwd());
      this.transcript.prepend(frag);
      const divs = $$('.divider.date', this.transcript);
      for (let k = 1; k < divs.length; k++) if (divs[k].dataset.date === divs[k - 1].dataset.date) divs[k].remove();
      this.renderedFrom = newFrom;
      observeMermaid(this.transcript);
      this.q('.load-more').hidden = newFrom === 0;
      this.refreshTurns();
      if (!silent) sc.scrollTop += sc.scrollHeight - before;
    }
    appendItems(start, items) {
      for (let k = 0; k < items.length; k++) this.items[start + k] = items[k];
      const frag = document.createDocumentFragment();
      let last = this.lastDateKey;
      for (let i = start; i < start + items.length; i++) {
        const it = this.items[i];
        const dk = it.ts ? OY.dateKey(it.ts) : null;
        if (dk && dk !== last) { frag.appendChild(OY.divider(OY.fmtDate(it.ts), dk)); last = dk; }
        frag.appendChild(OY.itemNode(i, it, this.cwd()));
      }
      this.lastDateKey = last;
      this.transcript.appendChild(frag);
      observeMermaid(this.transcript);
      if (items.some(isPrompt)) this.refreshTurns();
      this.updateActivity();
      if (this.follow) this.scrollToBottom();
      else if (items.some((it) => !OY.isLog(it) || state.showLogs)) this.q('.jump-latest').hidden = false;
      if (items.some((it) => it.t === 'tool' && /^(Edit|Write|MultiEdit|NotebookEdit)$/.test(it.name))) bus.emit('files-changed', { session: this.id, cwd: this.cwd(), root: OY.workRoot(this.session) });
      if (items.some((it) => it.t === 'tool' && (it.name === 'Agent' || it.name === 'Task'))) bus.emit('team-changed', { session: this.id });
    }
    patchItem(index, item) {
      this.items[index] = item;
      const el = $(`.item[data-idx="${index}"]`, this.transcript);
      if (!el) return;
      const wasOpen = $('details.tool', el)?.open;
      el.innerHTML = OY.itemHtml(item, index, this.cwd());
      if (item.t === 'text') OY.decorateText($('.body.md', el));
      if (wasOpen) {
        const d = $('details.tool', el);
        if (d) { d.open = true; d.dispatchEvent(new Event('toggle')); }
      }
      observeMermaid(el);
      this.updateActivity();
      if (this.follow) this.scrollToBottom();
      if (item.t === 'tool' && item.result && /^(Edit|Write|MultiEdit|NotebookEdit)$/.test(item.name)) bus.emit('files-changed', { session: this.id, cwd: this.cwd(), root: OY.workRoot(this.session), path: item.input?.file_path });
      if (item.t === 'tool' && (item.name === 'Agent' || item.name === 'Task')) bus.emit('team-changed', { session: this.id });
    }
    scrollToBottom() { this.scroller.scrollTop = this.scroller.scrollHeight; }

    // ------------------------------------------------- long-session helpers
    /// Prompts rendered right now (for the ribbon) and all prompts (for the rail).
    refreshTurns() {
      this.turnEls = $$('.item.turn', this.transcript);
      const rail = this.q('.turn-rail');
      const prompts = [];
      this.items.forEach((it, i) => { if (isPrompt(it)) prompts.push(i); });
      if (prompts.length < 3) { rail.hidden = true; this.updateRibbon(); return; }
      const n = this.items.length || 1;
      rail.innerHTML = prompts.map((i, k) => `<button type="button" class="tick" data-idx="${i}" style="top:${((i / n) * 100).toFixed(2)}%" title="${k + 1}. ${esc(this.items[i].text.slice(0, 80))}"></button>`).join('');
      rail.hidden = false;
      this.updateRibbon();
    }
    /// While reading a long answer, keep the prompt it answers pinned at the top.
    updateRibbon() {
      const rib = this.q('.turn-ribbon');
      const top = this.scroller.scrollTop;
      const els = this.turnEls;
      let lo = 0, hi = els.length - 1, cur = -1;
      while (lo <= hi) {
        const mid = (lo + hi) >> 1;
        if (els[mid].offsetTop <= top + 8) { cur = mid; lo = mid + 1; } else hi = mid - 1;
      }
      const el = cur >= 0 ? els[cur] : null;
      $$('.turn-rail .tick.cur', this.el).forEach((t) => t.classList.remove('cur'));
      if (el) $(`.turn-rail .tick[data-idx="${el.dataset.idx}"]`, this.el)?.classList.add('cur');
      if (!el || el.offsetTop + el.offsetHeight > top + 4) { rib.hidden = true; return; }
      const it = this.items[+el.dataset.idx];
      if (!it) { rib.hidden = true; return; }
      rib.dataset.idx = el.dataset.idx;
      const text = it.text.replace(/<\/?[a-z_-]+(\s[^>]*)?>/gi, ' ').replace(/\s+/g, ' ').trim();
      rib.innerHTML = `<span class="rb-who">${t("あなた")}</span><span class="rb-text">${esc(text.slice(0, 200))}</span><span class="rb-go">↑</span>`;
      rib.hidden = false;
    }

    // ----------------------------------------------------------- composer
    renderComposer() {
      const s = this.session;
      const ta = this.ta;
      const send = this.q('.send-btn');
      let ph = '';
      let disabled = false;
      const pending = s?.owner === 'oyakata' && s.status === 'waiting' ? (this.run?.pending || []) : [];
      const key = pending.map((p) => p.request_id).join(',');
      if (key !== this.pendingKey) {
        this.pendingKey = key;
        if (pending.length) this.renderPendingCards(pending); else this.q('.pending-cards').innerHTML = '';
      }
      // Prompts Claude has not taken up yet (sent mid-task, or just sent): shown here until
      // they land in the transcript.
      const queued = s?.owner === 'oyakata' ? (this.run?.queued || []) : [];
      const qkey = queued.map((q) => `${q.sent_at}:${q.text}`).join('\n');
      if (qkey !== this.queuedKey) {
        this.queuedKey = qkey;
        this.q('.queued-msgs').innerHTML = queued.map((q) =>
          `<div class="qmsg" title="${this.interactive() ? t('送信済み。Claude が次のツール呼び出しの区切り（その前に今の作業が終わればその直後）で読みます') : t('送信済み。今のターンが終わると次のターンとして渡します')}"><span class="qm-glyph">⏳</span><span class="qm-text">${esc(q.text)}</span><span class="qm-note">${this.interactive() ? t('次の区切りで渡します') : t('次のターンで渡します')}</span></div>`).join('');
      }
      const tips = t('Enter で送信 · Shift+Enter で改行 · Shift+Tab で権限モード');
      if (this.isDraft()) {
        disabled = this.starting;
        ph = this.starting ? `${this.who()} ${t("が準備しています…")}` : `${t("何をしましょう？（")}${tips}）`;
      } else if (!s) {
        disabled = true;
      } else if (s.owner === 'oyakata') {
        if (pending.length) { disabled = true; ph = t('上のカードに答えると続きます'); }
        else if (s.status === 'busy') ph = this.interactive() ? t('作業中に指示を送る…（次の区切りで Claude に渡ります · Esc で中断）') : `${t("作業中…（送った指示は今のターンが終わってから")} ${this.who()} ${t("に渡します · Esc で中断）")}`;
        else ph = `${t("指示を入力…（")}${tips}）`;
      } else if (s.owner === 'external') {
        disabled = true;
        ph = `${this.who()} ${t("が別のプロセス（ターミナルなど）でこのセッションを動かしています。終わると送れます")}`;
      } else if (s.owner === 'terminal') {
        if (state.config.can_type === false || !s.live?.typeable) {
          disabled = true;
          ph = state.config.can_type === false ? t('ターミナルで稼働中のセッションへの送信は Windows でのみ使えます') : t('IDE や SDK で動いているセッションには送れません（終了後に引き継げます）');
        } else if (s.status === 'waiting') { disabled = true; ph = t('ターミナルで確認待ちです。ターミナル側で答えると送れます'); }
        else if (s.status === 'busy') ph = t('ターミナルへ送信（作業中に打ち込んだ扱いになります・Esc で中断）');
        else ph = t('ターミナルへ送信…（ターミナルの入力欄に打ち込んで Enter）');
      } else if (this.agentCfg().can_run === false || (this.agent === 'claude' && state.config.can_run === false)) {
        disabled = true;
        ph = `${this.agentCfg().exe_name || this.agent} ${t("コマンドが見つからないため、ここからは送れません")}`;
      } else {
        ph = t('送信すると OYAKATA がこのセッションを引き継いで再開します…');
      }
      ta.disabled = disabled;
      ta.placeholder = ph;
      send.disabled = disabled;
      this.renderStatusline();
    }
    renderPendingCards(pending) {
      const cwd = this.cwd();
      this.q('.pending-cards').innerHTML = pending.map((p) => {
        if (p.tool_name === 'AskUserQuestion') {
          const qs = (p.input?.questions || []).map((q, qi) => `<div class="qblock" data-q="${qi}">${q.header ? `<div class="qhead">${esc(q.header)}</div>` : ''}<div class="qtext">${esc(q.question)}</div>${(q.options || []).map((o, oi) =>
            `<label><input type="${q.multiSelect ? 'checkbox' : 'radio'}" name="q${qi}-${esc(p.request_id)}" value="${esc(o.label)}"${oi === 0 && !q.multiSelect ? ' checked' : ''}><span><b>${esc(o.label)}</b>${o.description ? `<div class="od">${esc(o.description)}</div>` : ''}</span></label>`).join('')}
            <label><input type="${q.multiSelect ? 'checkbox' : 'radio'}" name="q${qi}-${esc(p.request_id)}" value="__other__"><span>${t("その他:")} <input type="text" class="other-text" placeholder="${t("自由記述")}"></span></label></div>`).join('');
          return `<div class="pcard" data-req="${esc(p.request_id)}"><div class="pcard-head"><span class="kicker">${t("質問")}</span><span>${t("Claude からの質問")}</span></div><div class="pcard-body">${qs}</div><div class="pcard-actions"><button type="button" class="btn primary answer-q">${t("回答する")}</button></div></div>`;
        }
        if (p.tool_name === 'ExitPlanMode') {
          return `<div class="pcard plan" data-req="${esc(p.request_id)}"><div class="pcard-head"><span class="kicker">${t("計画の承認")}</span><span>${t("Claude が計画を提示しています")}</span></div><div class="pcard-body md">${md(p.input?.plan || '')}</div><div class="pcard-actions"><button type="button" class="btn primary perm-allow">${t("承認して進める")}</button><button type="button" class="btn danger perm-deny">${t("修正を依頼")}</button><textarea class="deny-reason" rows="1" placeholder="${t("修正してほしい点（拒否時に Claude へ伝わります）")}"></textarea></div></div>`;
        }
        const sugg = Array.isArray(p.suggestions) && p.suggestions.length;
        return `<div class="pcard" data-req="${esc(p.request_id)}"><div class="pcard-head"><span class="kicker">${t("許可の確認")}</span><span class="tool-icon">${TOOL_ICON[p.tool_name] || '🔧'}</span><span>${esc(p.display_name || p.tool_name)}</span>${p.description ? `<span class="muted">${esc(p.description)}</span>` : ''}</div>
          <div class="pcard-body">${toolInputHtml(p.tool_name, p.input, cwd)}</div>
          <div class="pcard-actions"><button type="button" class="btn primary perm-allow">${t("許可")}</button>${sugg ? t('<button type="button" class="btn perm-allow-always" title="Claude Code が提案する範囲でこのセッション中は確認を省く">許可（以後も）</button>') : ''}<button type="button" class="btn danger perm-deny">${t("拒否")}</button><input type="text" class="deny-reason" placeholder="${t("拒否の理由（任意、Claude に伝わります）")}"></div></div>`;
      }).join('');
      observeMermaid(this.q('.pending-cards'));
    }
    async answerPermission(card, behavior, applySuggestions) {
      const requestId = card.dataset.req;
      const body = { request_id: requestId, behavior };
      if (behavior === 'deny') body.message = $('.deny-reason', card)?.value?.trim() || undefined;
      if (applySuggestions) body.apply_suggestions = true;
      if ($('.answer-q', card)) {
        const answers = {};
        const pending = this.run?.pending?.find((p) => p.request_id === requestId);
        (pending?.input?.questions || []).forEach((q, qi) => {
          const block = $(`.qblock[data-q="${qi}"]`, card);
          const picked = $$('input:checked', block).map((i) => i.value === '__other__' ? ($('.other-text', block)?.value?.trim() || '') : i.value).filter(Boolean);
          answers[q.question] = picked.join(', ');
        });
        body.answers = answers;
      }
      $$('button', card).forEach((b) => { b.disabled = true; });
      try {
        await api.post(`/api/run/${encodeURIComponent(this.id)}/permission`, body);
        OY.fx.stamp(card, behavior === 'deny' ? 'deny' : body.answers ? 'answer' : 'approve');
      } catch (e) { toast(e.message); $$('button', card).forEach((b) => { b.disabled = false; }); }
    }
    async sendPrompt() {
      const s = this.session;
      const text = this.ta.value.trim();
      if (!text || this.sending || this.q('.send-btn').disabled) return;
      this.sending = true;
      this.q('.send-btn').disabled = true;
      try {
        if (this.isDraft()) {
          const r = await api.post('/api/run/start', {
            agent: this.agent, cwd: this.draftCwd, prompt: text, session_id: this.agentCfg().assigns_id ? undefined : this.id, worktree: this.opts.worktree,
            model: this.opts.model || undefined, permission_mode: this.opts.mode, effort: this.opts.effort || undefined,
          });
          if (r.cwd) this.draftCwd = r.cwd;
          if (r.note) toast(r.note);
          if (r.session_id && r.session_id !== this.id) this.rekey(r.session_id);
          this.starting = true;
          this.pendingPrompt = text;
          this.awaitingTranscript = true;
          this.showWelcome();
          this.renderHeader();
        } else if (s.owner === 'oyakata') {
          await api.post(`/api/run/${encodeURIComponent(s.id)}/send`, { text });
        } else if (s.owner === 'terminal') {
          await api.post(`/api/terminal/${encodeURIComponent(s.id)}/send`, { text });
        } else {
          await api.post('/api/run/start', {
            agent: this.agent, cwd: s.cwd, prompt: text, resume: s.id,
            model: this.opts.model || s.model || undefined, permission_mode: this.opts.mode, effort: this.opts.effort || s.effort || undefined,
          });
        }
        this.ta.value = '';
        this.autoGrow();
        this.follow = true;
        this.scrollToBottom();
      } catch (e) {
        toast(e.message);
      } finally {
        this.sending = false;
        this.renderComposer();
        this.updateActivity();
      }
    }
    async interrupt() {
      const s = this.session;
      if (!s || s.status !== 'busy') return false;
      const kind = s.owner === 'terminal' ? 'terminal' : s.owner === 'oyakata' ? 'run' : null;
      if (!kind) return false;
      try { await api.post(`/api/${kind}/${encodeURIComponent(this.id)}/interrupt`); toast('中断しました'); } catch (e) { toast(e.message); }
      return true;
    }
    autoGrow() {
      const ta = this.ta;
      ta.style.height = 'auto';
      ta.style.height = Math.min(ta.scrollHeight, window.innerHeight * 0.4) + 'px';
    }

    // ----------------------------------------------------------------- ui
    bindUi() {
      const q = this.q.bind(this);
      q('.b-more').addEventListener('click', () => this.togglePopover('.pop-menu', () => this.menuHtml()));
      q('.b-team').addEventListener('click', () => OY.team.open(this.id));
      q('.pop-menu').addEventListener('click', (e) => { const a = e.target.closest('a[data-act]'); if (a) { e.preventDefault(); this.menuAction(a.dataset.act); } });
      q('.chat-head').addEventListener('click', (e) => {
        const jump = e.target.closest('a[data-jump]');
        if (jump) { e.preventDefault(); this.closePopovers(); this.jumpTo(+jump.dataset.jump); return; }
        const row = e.target.closest('.prow');
        if (!row) return;
        const root = OY.workRoot(this.session);
        if (e.target.closest('.open-diff')) { this.closePopovers(); OY.editors.openDiff(root, row.dataset.rel); }
        else if (e.target.closest('.open-file') || (row.dataset.path && !e.target.closest('button'))) { this.closePopovers(); OY.editors.openFile(row.dataset.path, root); }
        else if (e.target.closest('.open-url')) { this.closePopovers(); OY.editors.openUrl(row.dataset.url); }
        else if (e.target.closest('.copy-url')) copyText(row.dataset.url);
      });
      q('.chat-head').addEventListener('dragstart', (e) => {
        const row = e.target.closest('.prow[data-path]');
        if (!row) return;
        e.dataTransfer.setData('text/oy-open', JSON.stringify(OY.editors.fileDesc(row.dataset.path, OY.workRoot(this.session))));
      });
      q('.load-more button').addEventListener('click', () => this.loadMore());
      q('.jump-latest').addEventListener('click', () => { this.follow = true; this.scrollToBottom(); q('.jump-latest').hidden = true; });
      q('.turn-ribbon').addEventListener('click', () => { const i = +q('.turn-ribbon').dataset.idx; if (!Number.isNaN(i)) this.jumpTo(i); });
      q('.turn-rail').addEventListener('click', (e) => { const t = e.target.closest('.tick'); if (t) this.jumpTo(+t.dataset.idx); });
      let raf = 0;
      this.scroller.addEventListener('scroll', () => {
        const sc = this.scroller;
        const near = sc.scrollHeight - sc.scrollTop - sc.clientHeight < 150;
        this.follow = near;
        if (near) q('.jump-latest').hidden = true;
        if (!raf) raf = requestAnimationFrame(() => { raf = 0; this.updateRibbon(); });
      }, { passive: true });
      new ResizeObserver(() => { if (this.follow) this.scrollToBottom(); }).observe(this.transcript);
      q('.send-btn').addEventListener('click', () => this.sendPrompt());
      this.ta.addEventListener('input', () => this.autoGrow());
      this.ta.addEventListener('keydown', (e) => {
        if (e.key === 'Enter' && !e.shiftKey && !e.isComposing && !e.altKey) { e.preventDefault(); this.sendPrompt(); }
        else if (e.key === 'Tab' && e.shiftKey) { e.preventDefault(); this.cycleMode(); }
        else if (e.key === 'Escape' && this.session?.status === 'busy') { e.preventDefault(); e.stopPropagation(); this.interrupt(); }
      });
      this.el.addEventListener('keydown', (e) => {
        if (e.key === 'Escape' && e.target === this.el && this.session?.status === 'busy') this.interrupt();
      });
      q('.sl-mode').addEventListener('click', (e) => this.togglePopover('.pop-sl', () => this.modeMenuHtml(), e.currentTarget));
      q('.sl-wt').addEventListener('click', () => {
        if (!this.isDraft() || this.starting) return;
        this.opts.worktree = !this.opts.worktree;
        this.renderStatusline();
        if (!this.q('.welcome').hidden) this.showWelcome();
      });
      q('.sl-model').addEventListener('click', (e) => this.togglePopover('.pop-sl', () => this.modelMenuHtml(), e.currentTarget));
      q('.sl-effort').addEventListener('click', (e) => this.togglePopover('.pop-sl', () => this.effortMenuHtml(), e.currentTarget));
      q('.pop-sl').addEventListener('keydown', (e) => {
        const input = e.target.closest('.model-input');
        if (!input || e.key !== 'Enter') return;
        e.preventDefault();
        const v = input.value.trim();
        this.closePopovers();
        this.setModel(v);
        this.ta.focus();
      });
      q('.pop-sl').addEventListener('click', (e) => {
        if (e.target.closest('.model-input')) return;
        const a = e.target.closest('a');
        if (!a) return;
        e.preventDefault();
        this.closePopovers();
        if (a.dataset.mode !== undefined) this.setMode(a.dataset.mode);
        else if (a.dataset.model !== undefined) this.setModel(a.dataset.model);
        else if (a.dataset.effort !== undefined) { this.opts.effort = a.dataset.effort; this.renderStatusline(); }
        this.ta.focus();
      });
      q('.pending-cards').addEventListener('click', (e) => {
        const card = e.target.closest('.pcard');
        if (!card) return;
        if (e.target.closest('.perm-allow')) this.answerPermission(card, 'allow', false);
        else if (e.target.closest('.perm-allow-always')) this.answerPermission(card, 'allow', true);
        else if (e.target.closest('.perm-deny')) this.answerPermission(card, 'deny', false);
        else if (e.target.closest('.answer-q')) this.answerPermission(card, 'allow', false);
      });
      q('.pending-cards').addEventListener('focusin', (e) => {
        const other = e.target.closest('label')?.querySelector('input[value="__other__"]');
        if (e.target.classList.contains('other-text') && other) other.checked = true;
      });
    }
  }

  /// Open a session's chat. Selecting a session of another repository first switches the
  /// workbench to that repository's workspace. `opts.ts` scrolls to a message.
  function open(id, opts = {}) {
    const s = state.byId.get(id);
    if (s) OY.followSession(s);
    const tab = OY.wb.open({ kind: 'chat', key: 'chat:' + id, title: s ? sessionTitle(s) : t('セッション'), icon: '💬', data: { id } }, { where: 'bottom', ...opts });
    if (opts.ts) tab?.inst?.revealTs?.(opts.ts);
    return tab;
  }
  /// A new session in `cwd`: opens an empty chat; the agent starts when the first prompt is sent.
  function openDraft(cwd, agent) {
    const id = OY.uuid();
    agent = agent || OY.defaultAgent();
    OY.setActiveRepo(repoOfCwd(cwd)?.root || cwd);
    const repo = repoOfCwd(cwd);
    const tag = agent !== 'claude' ? ` (${agentLabel(agent)})` : '';
    const tab = OY.wb.open({ kind: 'chat', key: 'chat:' + id, title: `${t("新しいセッション ·")} ${repo?.name || basename(cwd)}${tag}`, icon: '💬', data: { id, cwd, draft: true, agent } }, { where: 'bottom' });
    setTimeout(() => tab?.inst?.focus?.(), 30);
    return tab;
  }
  function desc(id) {
    const s = state.byId.get(id);
    return { kind: 'chat', key: 'chat:' + id, title: s ? sessionTitle(s) : t('セッション'), icon: '💬', data: { id } };
  }

  OY.wb.registerKind('chat', (d) => {
    const c = new Chat(d.data.id, d.key, d.data);
    return { el: c.el, chat: c, onShow: () => c.onShow(), dispose: () => c.dispose(), focus: () => c.focusComposer(), revealTs: (ts) => c.revealTs(ts) };
  });

  OY.chat = { open, openDraft, desc, instances };
})();
