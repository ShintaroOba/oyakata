/* OYAKATA chat pane: one Claude Code session rendered as a conversation with a composer,
   permission / question / plan cards, and live updates. Several can be open at once. */
(() => {
  'use strict';
  const { $, $$, esc, api, state, bus, md, toast, sessionTitle, statusLabel, stripCwd, fmtTokens, shortModel, fmtTime, buildRange, observeMermaid, bindTranscript, TOOL_ICON, toolInputHtml, fillSelect, MODELS, EFFORTS, modeOptions, runDefaults, saveRunDefaults, copyText, relTo } = OY;

  const INITIAL_WINDOW = 300;
  const PAGE = 300;
  const instances = new Map();

  const TEMPLATE = `
    <div class="chat-head">
      <div class="head-row"><h2 class="chat-title"></h2><span class="chip owner" hidden></span><span class="status-chip"></span></div>
      <div class="head-row chips"></div>
      <div class="head-row tools">
        <button type="button" class="btn b-files" title="このセッションが編集したファイル">変更ファイル</button>
        <button type="button" class="btn b-artifacts" hidden>アーティファクト</button>
        <button type="button" class="btn b-outline" title="プロンプトの一覧へジャンプ">プロンプト一覧</button>
        <button type="button" class="btn b-logs" title="ツール呼び出しなどの作業ログを会話に混ぜるか、畳んで会話だけにするか">ログ</button>
        <button type="button" class="btn b-expand" title="ツール呼び出しを展開">展開</button>
        <button type="button" class="btn b-collapse" title="ツール呼び出しを折り畳む">折畳</button>
        <button type="button" class="btn b-resume" title="ターミナルで再開するコマンドをコピー">再開コマンド</button>
        <span class="path"></span>
      </div>
      <div class="popover pop-outline" hidden></div>
      <div class="popover pop-files" hidden></div>
      <div class="popover pop-artifacts" hidden></div>
    </div>
    <div class="chat-body">
      <div class="chat-scroller">
        <div class="load-more" hidden><button type="button" class="btn">さらに前を表示</button></div>
        <div class="transcript"></div>
        <div class="pending-focus" hidden>このセッションの記録を待っています…</div>
      </div>
      <button type="button" class="jump-latest" hidden>新しい出力 ↓</button>
    </div>
    <div class="activity-bar" hidden><span class="spinner"></span><span class="act-text"></span><span class="act-elapsed"></span><span class="spacer"></span><span class="act-hidden"></span></div>
    <div class="composer">
      <div class="pending-cards"></div>
      <div class="composer-row">
        <textarea rows="2" placeholder="このセッションに指示を送る…（Enter で送信、Shift+Enter で改行）"></textarea>
        <div class="composer-actions">
          <button type="button" class="btn primary b-send">送信</button>
          <button type="button" class="btn danger b-interrupt" hidden>中断</button>
        </div>
      </div>
      <div class="composer-foot">
        <span class="composer-hint"></span>
        <span class="composer-opts" hidden><select class="opt-model" title="モデル"></select><select class="opt-mode" title="権限モード"></select><select class="opt-effort" title="努力レベル"></select></span>
        <button type="button" class="link-btn b-end" hidden>セッションを終了</button>
      </div>
    </div>`;

  class Chat {
    constructor(id, key) {
      this.id = id;
      this.key = key;
      this.items = [];
      this.agents = [];
      this.session = state.byId.get(id) || null;
      this.run = null;
      this.renderedFrom = 0;
      this.lastDateKey = null;
      this.follow = true;
      this.loadSeq = 0;
      this.sending = false;
      this.el = document.createElement('div');
      this.el.className = 'chat';
      this.el.innerHTML = TEMPLATE;
      this.q = (sel) => $(sel, this.el);
      this.scroller = this.q('.chat-scroller');
      this.transcript = this.q('.transcript');
      this.ta = this.q('textarea');
      this.expandedRuns = new Set();
      this.el.classList.toggle('conv-only', !state.showLogs);
      this.activityTimer = setInterval(() => this.updateActivity(), 1000);
      this.unsubs = [
        bus.on('logs-mode', (v) => { this.el.classList.toggle('conv-only', !v); this.q('.b-logs').textContent = v ? 'ログ: 表示' : 'ログ: 畳む'; this.updateLogGroups(); this.updateActivity(); if (this.follow) this.scrollToBottom(); }),
        bus.on('sessions', () => this.onSessions()),
        bus.on('append', (d) => { if (d.session === this.id) this.onAppend(d); }),
        bus.on('patch', (d) => { if (d.session === this.id) this.patchItem(d.index, d.item); }),
        bus.on('reset', (d) => { if (d.session === this.id) this.load(); }),
        bus.on('run', (d) => { if (d.run?.session_id === this.id) this.onRun(d.run); }),
        bus.on('lagged', () => this.load()),
      ];
      bindTranscript(this.scroller, { items: () => this.items, cwd: () => this.session?.cwd, sessionId: () => this.id });
      bindTranscript(this.q('.pending-cards'), { items: () => [], cwd: () => this.session?.cwd, sessionId: () => this.id });
      this.bindUi();
      state.openChats.add(id);
      instances.set(id, this);
      this.load();
    }

    cwd() { return this.session?.cwd; }

    // ------------------------------------------------------------ loading
    async load() {
      const seq = ++this.loadSeq;
      this.q('.pending-focus').hidden = true;
      this.q('.load-more').hidden = true;
      this.transcript.innerHTML = '<div class="loading">読み込み中…</div>';
      const known = state.byId.get(this.id);
      if (known) { this.session = known; this.renderHeader(); this.renderComposer(); }
      let data;
      try {
        data = await api.get(`/api/sessions/${encodeURIComponent(this.id)}`);
      } catch (e) {
        if (seq !== this.loadSeq) return;
        if (!known || known.size === 0) {
          this.transcript.innerHTML = '';
          this.q('.pending-focus').hidden = false;
          this.q('.chat-title').textContent = known ? sessionTitle(known) : '新しいセッション';
          this.items = [];
          this.renderComposer();
        } else {
          this.transcript.innerHTML = `<div class="loading err">読み込みに失敗しました: ${esc(e.message)}</div>`;
        }
        return;
      }
      if (seq !== this.loadSeq) return;
      this.items = data.items || [];
      this.agents = data.agents || [];
      this.session = data.session;
      this.run = data.run || null;
      this.renderHeader();
      this.renderComposer();
      this.renderInitial();
      this.followRepo();
    }
    followRepo() {
      const root = this.session?.repo?.root;
      if (root && state.followRepo) OY.setActiveRepo(root);
    }
    onShow() {
      state.lastChat = this.id;
      this.followRepo();
      if (this.follow) this.scrollToBottom();
    }
    onSessions() {
      const s = state.byId.get(this.id);
      if (!s) return;
      const hadSize = this.session?.size || 0;
      this.session = s;
      this.renderHeader();
      this.renderComposer();
      this.updateActivity();
      OY.wb.setTitle(this.key, sessionTitle(s));
      if (!this.q('.pending-focus').hidden && s.size > 0 && hadSize === 0) this.load();
    }
    onAppend(d) {
      if (d.start !== this.items.length) { this.load(); return; }
      this.appendItems(d.start, d.items);
    }
    onRun(run) {
      this.run = run;
      if (this.session) {
        this.session.owner = run.status === 'exited' ? null : 'oyakata';
        this.session.status = run.status === 'exited' ? 'ended' : run.status;
        this.renderHeader();
      }
      this.renderComposer();
      if (run.last_error && run.status !== 'busy') toast(run.last_error);
    }
    dispose() {
      for (const u of this.unsubs) u();
      clearInterval(this.activityTimer);
      state.openChats.delete(this.id);
      instances.delete(this.id);
    }

    // ------------------------------------------------ logs vs conversation
    /// In conversation-only mode, fold each run of log items into one summary row.
    updateLogGroups() {
      const tr = this.transcript;
      $$('.act-summary', tr).forEach((s) => s.remove());
      if (state.showLogs) { $$('.item.log.show', tr).forEach((el) => el.classList.remove('show')); return; }
      const children = Array.from(tr.children);
      let run = [];
      const flush = () => {
        if (!run.length) return;
        const firstIdx = +run[0].dataset.idx;
        const expanded = this.expandedRuns.has(firstIdx);
        const counts = new Map();
        let pending = null;
        for (const el of run) {
          const it = this.items[+el.dataset.idx];
          if (!it) continue;
          const label = it.t === 'tool' ? it.name : it.t === 'thinking' ? '思考' : it.t === 'note' ? 'コマンド' : null;
          if (label) counts.set(label, (counts.get(label) || 0) + 1);
          if (it.t === 'tool' && !it.result) pending = it;
          el.classList.toggle('show', expanded);
        }
        const total = [...counts.values()].reduce((a, b) => a + b, 0);
        if (!total) { run = []; return; }
        const parts = [...counts.entries()].map(([k, v]) => `${esc(k)}${v > 1 ? ` ×${v}` : ''}`).join(' · ');
        const s = document.createElement('div');
        s.className = 'act-summary' + (pending ? ' running' : '') + (expanded ? ' open' : '');
        s.dataset.run = firstIdx;
        s.innerHTML = `<span class="spinner"></span><span class="act-icon">⚙</span><span class="act-label">作業ログ ${total} 件</span><span class="act-parts">${parts}</span>${pending ? `<span class="act-now">実行中: ${esc(pending.name)} ${esc(stripCwd(pending.summary, this.cwd()))}</span>` : ''}<span class="spacer"></span><button type="button" class="link-btn act-toggle">${expanded ? '隠す' : '表示'}</button>`;
        run[0].before(s);
        run = [];
      };
      for (const el of children) {
        if (el.classList.contains('item') && el.classList.contains('log')) run.push(el);
        else if (el.classList.contains('act-summary')) continue;
        else flush();
      }
      flush();
    }
    toggleRun(firstIdx) {
      if (this.expandedRuns.has(firstIdx)) this.expandedRuns.delete(firstIdx); else this.expandedRuns.add(firstIdx);
      this.updateLogGroups();
    }
    /// The strip above the composer that says what Claude is doing right now.
    updateActivity() {
      const bar = this.q('.activity-bar');
      const s = this.session;
      const busy = s && s.status === 'busy';
      if (!busy) { bar.hidden = true; return; }
      let text = '考え中…';
      let since = null;
      for (let i = this.items.length - 1; i >= 0 && i >= this.items.length - 30; i--) {
        const it = this.items[i];
        if (it.t === 'tool' && !it.result) { text = `${TOOL_ICON[it.name] || '🔧'} ${it.name} を実行中: ${stripCwd(it.summary, this.cwd())}`; since = it.ts; break; }
        if (it.t === 'text' || it.t === 'user' || (it.t === 'tool' && it.result)) { since = it.ts; break; }
      }
      const elapsed = since ? Math.max(0, Math.round((Date.now() - new Date(since).getTime()) / 1000)) : null;
      this.q('.act-text').textContent = text;
      this.q('.act-elapsed').textContent = elapsed != null ? (elapsed >= 60 ? `${Math.floor(elapsed / 60)}分${elapsed % 60}秒` : `${elapsed}秒`) : '';
      const hiddenCount = state.showLogs ? 0 : $$('.item.log:not(.show)', this.transcript).length;
      this.q('.act-hidden').textContent = hiddenCount ? `ログ ${hiddenCount} 件を畳んでいます` : '';
      bar.hidden = false;
    }

    // ------------------------------------------------------------- header
    renderHeader() {
      const s = this.session;
      if (!s) return;
      this.q('.chat-title').textContent = sessionTitle(s);
      const st = this.q('.status-chip');
      st.textContent = statusLabel(s.status) + (s.status === 'waiting' && s.waiting_for ? `: ${s.waiting_for.replace(/^permission: /, '')}` : '');
      st.className = 'status-chip ' + (s.status || 'ended');
      const owner = this.q('.chip.owner');
      owner.hidden = !s.owner;
      owner.textContent = s.owner === 'oyakata' ? 'OYAKATA が実行中' : s.owner === 'terminal' ? 'ターミナルで実行中' : '';
      const chips = [];
      const chip = (t, title) => chips.push(`<span class="chip" title="${esc(title || '')}">${esc(t)}</span>`);
      if (s.repo?.name) chip(s.repo.name + (s.repo.subdir ? '/' + s.repo.subdir : ''), s.repo.root || s.cwd);
      if (s.git_branch && s.git_branch !== 'HEAD') chip('⎇ ' + s.git_branch);
      if (s.model) chip(shortModel(s.model));
      if (s.live?.name) chip('名前: ' + s.live.name);
      chip(`${s.user_turns} 往復 · ${s.tool_calls} ツール`);
      if (s.output_tokens) chip(`出力 ${fmtTokens(s.output_tokens)} tok`, `入力 ${fmtTokens(s.input_tokens)} tok / キャッシュ読取 ${fmtTokens(s.cache_read_tokens)} tok`);
      if (s.started_at) chip('開始 ' + new Date(s.started_at).toLocaleString('ja-JP', { month: 'numeric', day: 'numeric', hour: '2-digit', minute: '2-digit' }));
      if (s.compactions) chip(`圧縮 ${s.compactions} 回`);
      if (s.subagents) chip(`サブエージェント ${s.subagents}`);
      if (s.continued_in) chips.push(`<a class="chip link" href="#/s/${esc(s.continued_in)}">→ 続きのセッション</a>`);
      this.q('.chips').innerHTML = chips.join('');
      const path = this.q('.tools .path');
      path.textContent = s.cwd || '';
      path.title = `session ${s.id}`;
      this.q('.b-logs').textContent = state.showLogs ? 'ログ: 表示' : 'ログ: 畳む';
      const bf = this.q('.b-files');
      bf.textContent = `変更ファイル${s.edited_files?.length ? ` ${s.edited_files.length}` : ''}`;
      bf.disabled = !s.edited_files?.length;
      const ab = this.q('.b-artifacts');
      ab.hidden = !s.artifacts?.length;
      ab.textContent = `アーティファクト ${s.artifacts?.length || 0}`;
    }
    closePopovers() { $$('.chat-head .popover', this.el).forEach((p) => { p.hidden = true; }); }
    togglePopover(sel, build) {
      const o = this.q(sel);
      const wasOpen = !o.hidden;
      $$('.chat-head .popover').forEach((p) => { p.hidden = true; });
      if (wasOpen) return;
      o.innerHTML = build();
      o.hidden = false;
    }
    outlineHtml() {
      const rows = [];
      this.items.forEach((it, i) => {
        if (it.t === 'user' && !it.meta && !it.compact_summary) rows.push(`<a href="#" data-jump="${i}"><span class="n">${rows.length + 1}</span>${esc(it.text.slice(0, 120))}</a>`);
      });
      return rows.length ? rows.join('') : '<div class="sb-empty">プロンプトがありません</div>';
    }
    filesHtml() {
      const s = this.session;
      const root = s?.repo?.root;
      return '<div class="ptitle">このセッションが編集したファイル</div>' + (s?.edited_files || []).map((f) => {
        const rel = relTo(root, f.path);
        return `<div class="prow" data-path="${esc(f.path)}" data-rel="${esc(rel || '')}" draggable="true"><span class="pp" title="${esc(f.path)}">${esc(stripCwd(f.path, s.cwd))}</span><span class="pc">${f.edits ? `編集 ${f.edits}` : ''}${f.edits && f.writes ? ' · ' : ''}${f.writes ? `書込 ${f.writes}` : ''}</span>${rel ? '<button type="button" class="btn open-diff">差分</button>' : ''}<button type="button" class="btn open-file">開く</button></div>`;
      }).join('');
    }
    artifactsHtml() {
      return '<div class="ptitle">アーティファクト（claude.ai）</div>' + (this.session?.artifacts || []).map((u) =>
        `<div class="prow" data-url="${esc(u)}"><span class="pp">${esc(u)}</span><button type="button" class="btn open-url">開く</button><button type="button" class="btn copy-url">コピー</button></div>`).join('');
    }
    jumpTo(idx) {
      while (this.renderedFrom > idx) this.loadMore(true);
      const el = $(`.item[data-idx="${idx}"]`, this.transcript);
      if (el) { el.scrollIntoView({ block: 'start', behavior: 'smooth' }); this.follow = false; }
    }

    // --------------------------------------------------------- transcript
    renderInitial() {
      this.transcript.innerHTML = '';
      const from = Math.max(0, this.items.length - INITIAL_WINDOW);
      this.renderedFrom = from;
      const { frag, lastDate } = buildRange(this.items, from, this.items.length, this.cwd());
      this.lastDateKey = lastDate;
      this.transcript.appendChild(frag);
      observeMermaid(this.transcript);
      this.q('.load-more').hidden = from === 0;
      this.follow = true;
      this.q('.jump-latest').hidden = true;
      this.updateLogGroups();
      this.updateActivity();
      this.scrollToBottom();
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
      this.updateLogGroups();
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
      this.updateLogGroups();
      this.updateActivity();
      if (this.follow) this.scrollToBottom();
      else this.q('.jump-latest').hidden = false;
      if (items.some((it) => it.t === 'tool' && /^(Edit|Write|MultiEdit|NotebookEdit)$/.test(it.name))) bus.emit('files-changed', { session: this.id, cwd: this.cwd(), root: this.session?.repo?.root });
    }
    patchItem(index, item) {
      this.items[index] = item;
      const el = $(`.item[data-idx="${index}"]`, this.transcript);
      if (!el) return;
      const wasOpen = $('details.tool', el)?.open;
      el.innerHTML = OY.itemHtml(item, index, this.cwd());
      if (wasOpen) {
        const d = $('details.tool', el);
        if (d) { d.open = true; d.dispatchEvent(new Event('toggle')); }
      }
      observeMermaid(el);
      this.updateLogGroups();
      this.updateActivity();
      if (this.follow) this.scrollToBottom();
      if (item.t === 'tool' && item.result && /^(Edit|Write|MultiEdit|NotebookEdit)$/.test(item.name)) bus.emit('files-changed', { session: this.id, cwd: this.cwd(), root: this.session?.repo?.root, path: item.input?.file_path });
    }
    scrollToBottom() { this.scroller.scrollTop = this.scroller.scrollHeight; }

    // ----------------------------------------------------------- composer
    renderComposer() {
      const s = this.session;
      const ta = this.ta;
      const send = this.q('.b-send');
      const hint = this.q('.composer-hint');
      const opts = this.q('.composer-opts');
      const interrupt = this.q('.b-interrupt');
      const end = this.q('.b-end');
      this.q('.pending-cards').innerHTML = '';
      interrupt.hidden = true;
      end.hidden = true;
      opts.hidden = true;
      ta.disabled = false;
      send.disabled = false;
      send.textContent = '送信';
      if (!s) { ta.disabled = true; send.disabled = true; hint.textContent = ''; return; }
      const canRun = state.config.can_run !== false;
      if (s.owner === 'oyakata') {
        end.hidden = false;
        if (s.status === 'waiting' && this.run?.pending?.length) {
          this.renderPendingCards(this.run.pending);
          ta.disabled = true; send.disabled = true;
          hint.textContent = '上のカードに答えると続きます。';
        } else if (s.status === 'busy') {
          send.disabled = true; interrupt.hidden = false;
          hint.textContent = 'Claude が作業中です。次の指示は待機中になってから送れます。';
        } else {
          hint.textContent = 'OYAKATA がこのセッションを実行しています。';
        }
        return;
      }
      if (s.owner === 'terminal') {
        if (state.config.can_type === false || !s.live?.typeable) {
          ta.disabled = true; send.disabled = true;
          hint.textContent = state.config.can_type === false
            ? 'ターミナルで稼働中のセッションへの送信は Windows でのみ使えます。ターミナル側で終了すると、ここから引き継いで続けられます。'
            : 'このセッションはターミナル以外（IDE や SDK）で動いているため、ここからは送れません。';
          return;
        }
        send.textContent = 'ターミナルへ送信';
        if (s.status === 'waiting') {
          ta.disabled = true; send.disabled = true;
          hint.textContent = 'ターミナルで確認待ちです。ターミナル側で答えると、また送れます。';
        } else if (s.status === 'busy') {
          interrupt.hidden = false;
          hint.textContent = 'Claude が作業中です。今送ると、ターミナルで作業中に打ち込んだのと同じ扱いになります。中断はターミナルで Esc を押すのと同じです。';
        } else {
          hint.textContent = 'ターミナルの入力欄に打ち込んで Enter を押します。ターミナルに書きかけの入力があると、その後ろにつながります。';
        }
        return;
      }
      if (!canRun) {
        ta.disabled = true; send.disabled = true;
        hint.textContent = 'claude コマンドが見つからないため、ここからは送れません（oyakata --claude <path>）。';
        return;
      }
      send.textContent = '引き継いで送信';
      opts.hidden = false;
      const d = runDefaults();
      fillSelect(this.q('.opt-model'), MODELS, d.model);
      fillSelect(this.q('.opt-mode'), modeOptions(), d.mode);
      fillSelect(this.q('.opt-effort'), EFFORTS, d.effort);
      hint.textContent = '送信すると OYAKATA がこのセッションを再開し（claude --resume）、以後ここから会話できます。';
    }
    renderPendingCards(pending) {
      const cwd = this.cwd();
      this.q('.pending-cards').innerHTML = pending.map((p) => {
        if (p.tool_name === 'AskUserQuestion') {
          const qs = (p.input?.questions || []).map((q, qi) => `<div class="qblock" data-q="${qi}">${q.header ? `<div class="qhead">${esc(q.header)}</div>` : ''}<div class="qtext">${esc(q.question)}</div>${(q.options || []).map((o, oi) =>
            `<label><input type="${q.multiSelect ? 'checkbox' : 'radio'}" name="q${qi}-${esc(p.request_id)}" value="${esc(o.label)}"${oi === 0 && !q.multiSelect ? ' checked' : ''}><span><b>${esc(o.label)}</b>${o.description ? `<div class="od">${esc(o.description)}</div>` : ''}</span></label>`).join('')}
            <label><input type="${q.multiSelect ? 'checkbox' : 'radio'}" name="q${qi}-${esc(p.request_id)}" value="__other__"><span>その他: <input type="text" class="other-text" placeholder="自由記述"></span></label></div>`).join('');
          return `<div class="pcard" data-req="${esc(p.request_id)}"><div class="pcard-head"><span class="kicker">質問</span><span>Claude からの質問</span></div><div class="pcard-body">${qs}</div><div class="pcard-actions"><button type="button" class="btn primary answer-q">回答する</button></div></div>`;
        }
        if (p.tool_name === 'ExitPlanMode') {
          return `<div class="pcard plan" data-req="${esc(p.request_id)}"><div class="pcard-head"><span class="kicker">計画の承認</span><span>Claude が計画を提示しています</span></div><div class="pcard-body md">${md(p.input?.plan || '')}</div><div class="pcard-actions"><button type="button" class="btn primary perm-allow">承認して進める</button><button type="button" class="btn danger perm-deny">修正を依頼</button><textarea class="deny-reason" rows="1" placeholder="修正してほしい点（拒否時に Claude へ伝わります）"></textarea></div></div>`;
        }
        const sugg = Array.isArray(p.suggestions) && p.suggestions.length;
        return `<div class="pcard" data-req="${esc(p.request_id)}"><div class="pcard-head"><span class="kicker">許可の確認</span><span class="tool-icon">${TOOL_ICON[p.tool_name] || '🔧'}</span><span>${esc(p.display_name || p.tool_name)}</span>${p.description ? `<span class="muted">${esc(p.description)}</span>` : ''}</div>
          <div class="pcard-body">${toolInputHtml(p.tool_name, p.input, cwd)}</div>
          <div class="pcard-actions"><button type="button" class="btn primary perm-allow">許可</button>${sugg ? '<button type="button" class="btn perm-allow-always" title="Claude Code が提案する範囲でこのセッション中は確認を省く">許可（以後も）</button>' : ''}<button type="button" class="btn danger perm-deny">拒否</button><input type="text" class="deny-reason" placeholder="拒否の理由（任意、Claude に伝わります）"></div></div>`;
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
      try { await api.post(`/api/run/${encodeURIComponent(this.id)}/permission`, body); }
      catch (e) { toast(e.message); $$('button', card).forEach((b) => { b.disabled = false; }); }
    }
    async sendPrompt() {
      const s = this.session;
      const text = this.ta.value.trim();
      if (!s || !text || this.sending) return;
      this.sending = true;
      this.q('.b-send').disabled = true;
      try {
        if (s.owner === 'oyakata') {
          await api.post(`/api/run/${encodeURIComponent(s.id)}/send`, { text });
        } else if (s.owner === 'terminal') {
          await api.post(`/api/terminal/${encodeURIComponent(s.id)}/send`, { text });
        } else {
          const model = this.q('.opt-model').value, mode = this.q('.opt-mode').value, effort = this.q('.opt-effort').value;
          saveRunDefaults(model, mode, effort);
          await api.post('/api/run/start', { cwd: s.cwd, prompt: text, resume: s.id, model: model || undefined, permission_mode: mode || undefined, effort: effort || undefined });
        }
        this.ta.value = '';
        this.follow = true;
      } catch (e) {
        toast(e.message);
      } finally {
        this.sending = false;
        this.renderComposer();
      }
    }

    // ----------------------------------------------------------------- ui
    bindUi() {
      const q = this.q.bind(this);
      q('.b-outline').addEventListener('click', (e) => { e.stopPropagation(); this.togglePopover('.pop-outline', () => this.outlineHtml()); });
      q('.b-files').addEventListener('click', (e) => { e.stopPropagation(); this.togglePopover('.pop-files', () => this.filesHtml()); });
      q('.b-artifacts').addEventListener('click', (e) => { e.stopPropagation(); this.togglePopover('.pop-artifacts', () => this.artifactsHtml()); });
      q('.b-logs').addEventListener('click', () => { OY.setShowLogs(!state.showLogs); OY.toast(state.showLogs ? '作業ログを会話に混ぜて表示します' : '作業ログを畳み、会話だけを表示します'); });
      this.transcript.addEventListener('click', (e) => {
        const s = e.target.closest('.act-summary');
        if (s && (e.target.closest('.act-toggle') || !e.target.closest('button'))) this.toggleRun(+s.dataset.run);
      });
      q('.b-expand').addEventListener('click', () => $$('details.tool', this.transcript).forEach((d) => { d.open = true; }));
      q('.b-collapse').addEventListener('click', () => $$('details.tool, details.thinking', this.transcript).forEach((d) => { d.open = false; }));
      q('.b-resume').addEventListener('click', () => copyText(this.session?.cwd ? `cd "${this.session.cwd}" && claude --resume ${this.id}` : `claude --resume ${this.id}`));
      q('.chat-head').addEventListener('click', (e) => {
        const jump = e.target.closest('a[data-jump]');
        if (jump) { e.preventDefault(); this.closePopovers(); this.jumpTo(+jump.dataset.jump); return; }
        const row = e.target.closest('.prow');
        if (!row) return;
        const root = this.session?.repo?.root;
        if (e.target.closest('.open-diff')) { this.closePopovers(); OY.editors.openDiff(root, row.dataset.rel); }
        else if (e.target.closest('.open-file') || (row.dataset.path && !e.target.closest('button'))) { this.closePopovers(); OY.editors.openFile(row.dataset.path, root); }
        else if (e.target.closest('.open-url')) { this.closePopovers(); OY.editors.openUrl(row.dataset.url); }
        else if (e.target.closest('.copy-url')) copyText(row.dataset.url);
      });
      q('.chat-head').addEventListener('dragstart', (e) => {
        const row = e.target.closest('.prow[data-path]');
        if (!row) return;
        e.dataTransfer.setData('text/oy-open', JSON.stringify(OY.editors.fileDesc(row.dataset.path, this.session?.repo?.root)));
      });
      q('.load-more button').addEventListener('click', () => this.loadMore());
      q('.jump-latest').addEventListener('click', () => { this.follow = true; this.scrollToBottom(); q('.jump-latest').hidden = true; });
      this.scroller.addEventListener('scroll', () => {
        const sc = this.scroller;
        const near = sc.scrollHeight - sc.scrollTop - sc.clientHeight < 150;
        this.follow = near;
        if (near) q('.jump-latest').hidden = true;
      }, { passive: true });
      new ResizeObserver(() => { if (this.follow) this.scrollToBottom(); }).observe(this.transcript);
      q('.b-send').addEventListener('click', () => this.sendPrompt());
      this.ta.addEventListener('keydown', (e) => {
        if (e.key === 'Enter' && !e.shiftKey && !e.isComposing && !e.altKey) { e.preventDefault(); this.sendPrompt(); }
      });
      q('.b-interrupt').addEventListener('click', async () => {
        const kind = this.session?.owner === 'terminal' ? 'terminal' : 'run';
        try { await api.post(`/api/${kind}/${encodeURIComponent(this.id)}/interrupt`); toast('中断を要求しました'); } catch (e) { toast(e.message); }
      });
      q('.b-end').addEventListener('click', async () => {
        if (!(await OY.confirmDialog('セッションを終了', 'OYAKATA 側の Claude プロセスを終了します。会話は残るので、あとで引き継いだり <code>claude --resume</code> で再開できます。', { label: '終了', danger: true }))) return;
        try { await api.post(`/api/run/${encodeURIComponent(this.id)}/stop`); } catch (e) { toast(e.message); }
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

  function open(id, opts = {}) {
    const s = state.byId.get(id);
    return OY.wb.open({ kind: 'chat', key: 'chat:' + id, title: s ? sessionTitle(s) : '新しいセッション', icon: '💬', data: { id } }, { where: 'bottom', ...opts });
  }
  function desc(id) {
    const s = state.byId.get(id);
    return { kind: 'chat', key: 'chat:' + id, title: s ? sessionTitle(s) : 'セッション', icon: '💬', data: { id } };
  }

  OY.wb.registerKind('chat', (d) => {
    const c = new Chat(d.data.id, d.key);
    return { el: c.el, onShow: () => c.onShow(), dispose: () => c.dispose(), focus: () => c.ta.focus() };
  });

  OY.chat = { open, desc, instances };
})();
