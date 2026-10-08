/* OYAKATA worktrees: the linked git worktrees of every known repository, with what removing
   one would lose (uncommitted files, commits the main checkout's branch does not have), and
   removal one at a time or in bulk. The list lives in the sidebar's Worktree view. */
(() => {
  'use strict';
  const { $, esc, api, state, bus, toast, ago, sessionTitle, basename, norm, modal, showOutput } = OY;

  const wv = { repos: null, dir: '', error: '', loading: null, at: 0, timer: null };

  const visible = () => !!$('#sb-worktrees')?.classList.contains('visible');
  function load() {
    if (wv.loading) return wv.loading;
    if (!wv.repos) render();
    wv.loading = api.get('/api/worktrees')
      .then((r) => { wv.repos = r.repos || []; wv.dir = r.dir || ''; wv.error = ''; })
      .catch((e) => { wv.error = e.message; })
      .finally(() => { wv.loading = null; wv.at = Date.now(); render(); });
    return wv.loading;
  }
  function loadSoon(ms = 1500) {
    clearTimeout(wv.timer);
    wv.timer = setTimeout(() => { if (visible()) load(); }, ms);
  }

  /// Sessions that worked in the worktree at `path`, newest first. A worktree whose folder is
  /// gone no longer resolves as one, so the cwd counts too.
  function sessionsIn(path) {
    const k = norm(path);
    return state.sessions
      .filter((s) => (s.repo?.worktree && norm(s.repo.worktree) === k) || (s.cwd && (norm(s.cwd) === k || norm(s.cwd).startsWith(k + '/'))))
      .sort((a, b) => (b.last_at || '').localeCompare(a.last_at || ''));
  }
  const running = (ss) => ss.some((s) => s.status !== 'ended');
  function label(w) {
    if (w.branch) return w.branch;
    if (w.head) return `HEAD ${w.head.slice(0, 7)}`;
    return basename(w.path);
  }
  /// Nothing would be lost: no uncommitted files, no commits outside the main branch (or the
  /// folder is already gone, which only drops git's record and keeps an unmerged branch).
  function clean(w, ss) {
    if (w.locked || running(ss)) return false;
    return w.prunable || (w.changes === 0 && w.ahead === 0);
  }
  function all() {
    const out = [];
    for (const repo of wv.repos || []) for (const w of repo.worktrees || []) out.push({ repo, w });
    return out;
  }
  function find(path) { return all().find((x) => norm(x.w.path) === norm(path)) || null; }
  const cleanable = () => all().filter(({ w }) => clean(w, sessionsIn(w.path)));

  // ------------------------------------------------------------- render
  function rowHtml(repo, w) {
    const ss = sessionsIn(w.path);
    const run = running(ss);
    const base = repo.base || 'HEAD';
    const chips = [];
    if (run) chips.push(`<span class="chip tiny wt-run">${t('稼働中')}</span>`);
    if (w.prunable) chips.push(`<span class="chip tiny wt-warn" title="${t('フォルダが見つかりません')}">${t('フォルダなし')}</span>`);
    if (w.locked) chips.push(`<span class="chip tiny" title="git worktree lock">🔒</span>`);
    if (w.changes) chips.push(`<span class="chip tiny wt-warn" title="${t('未コミットの変更')}">${t('変更 {n}', { n: w.changes })}</span>`);
    if (w.ahead) chips.push(`<span class="chip tiny wt-ahead" title="${esc(t('{base} にないコミット', { base }))}">${t('未マージ {n}', { n: w.ahead })}</span>`);
    else if (w.ahead === 0 && w.changes === 0) chips.push(`<span class="chip tiny wt-ok" title="${esc(t('未コミットの変更も、{base} にないコミットもありません', { base }))}">${t('変更なし')}</span>`);
    // The dot sums it up: green nothing to lose, amber uncommitted files, accent unmerged
    // commits, red folder gone.
    const st = run ? '' : w.prunable ? 'missing' : w.changes ? 'dirty' : w.ahead ? 'ahead' : w.changes === 0 && w.ahead === 0 ? 'clean' : '';
    const meta = [w.date ? esc(ago(w.date)) : '', w.subject ? esc(w.subject) : ''].filter(Boolean).join(' · ');
    const talk = ss.length ? `<button type="button" class="wt-btn wt-chat" title="${esc(t('最後のセッションを開く: {title}', { title: sessionTitle(ss[0]) }))}">💬${ss.length > 1 ? ss.length : ''}</button>` : '';
    return `<div class="row wt-row${run ? ' busy' : ''}${st ? ' st-' + st : ''}" data-path="${esc(w.path)}" data-root="${esc(repo.root)}" title="${esc(w.path)}">
      <span class="dot"></span>
      <span class="row-title">${esc(label(w))}</span>
      <span class="row-meta">${chips.join('')}${chips.length && meta ? ' ' : ''}${meta}</span>
      <span class="wt-acts">${talk}<button type="button" class="wt-btn wt-open" title="${t('ツリーと Git を開く')}">⊞</button><button type="button" class="wt-btn wt-del" title="${t('worktree を削除')}"${run ? ' disabled' : ''}>🗑</button></span>
    </div>`;
  }
  function render() {
    const el = $('#wt-body');
    if (!el) return;
    const on = state.config.worktree !== false;
    const note = `<p class="wt-note">${on ? t('新しいセッションは worktree を作って作業します（設定で変更できます）。') : t('新しいセッションは元のフォルダで作業します（設定で worktree を使うようにできます）。')}${wv.dir ? ` ${t('作成先:')} <code>${esc(wv.dir)}</code>` : ''}</p>`;
    if (!wv.repos) {
      el.innerHTML = wv.error ? `${note}<div class="loading err">${esc(wv.error)}</div>` : `${note}<div class="loading">${t('読み込み中…')}</div>`;
      return;
    }
    const n = all().length;
    const c = cleanable().length;
    const head = `<div class="wt-head"><span class="wt-sum">${n ? t('{n} 件', { n }) : ''}</span><span class="spacer"></span>
      <button type="button" class="btn small" data-wt="refresh"${wv.loading ? ' disabled' : ''}>${wv.loading ? t('読み込み中…') : t('更新')}</button>
      <button type="button" class="btn small" data-wt="clean"${c ? '' : ' disabled'} title="${t('未コミットの変更も未マージのコミットもない worktree をまとめて削除')}">${t('片付ける')}${c ? ` (${c})` : ''}</button></div>`;
    let body = '';
    for (const repo of wv.repos) {
      if (!repo.worktrees?.length && !repo.error) continue;
      const key = 'wt:' + norm(repo.root);
      body += `<details class="group" data-key="${esc(key)}" open>
        <summary><span class="caret">▶</span><span class="g-name" title="${esc(repo.root)}">${esc(repo.name)}</span><span class="g-count">${repo.worktrees.length || ''}</span></summary>
        ${repo.error ? `<div class="g-empty err">${esc(repo.error)}</div>` : repo.worktrees.map((w) => rowHtml(repo, w)).join('')}
      </details>`;
    }
    if (!body) body = `<div class="sb-empty">${t('worktree はありません。')}${on ? `<br>${t('Git リポジトリで新しいセッションを始めると、ここに並びます。')}` : ''}</div>`;
    el.innerHTML = note + head + body;
    if (wv.error) el.insertAdjacentHTML('afterbegin', `<div class="loading err">${esc(wv.error)}</div>`);
  }

  // ------------------------------------------------------------- remove
  async function post(repo, w, { force = false, deleteBranch = false } = {}) {
    return api.post('/api/worktree/remove', { path: w.path, root: repo.root, force, delete_branch: deleteBranch });
  }
  function doneToast(r) {
    toast(r.branch_deleted ? t('worktree とブランチを削除しました') : r.branch ? `${t('worktree を削除しました。ブランチは残しています:')} ${r.branch}` : t('worktree を削除しました'));
  }
  /// Ask, then remove one worktree. The dialog says what would be lost, from a recent look at
  /// it; git itself still refuses to drop uncommitted changes unless the dialog warned of them.
  async function remove(path, { fresh = false } = {}) {
    if (fresh || !find(path) || Date.now() - wv.at > 15000) await load();
    const hit = find(path);
    if (!hit) { toast(t('worktree が見つかりません（すでに削除されています）')); return; }
    const { repo, w } = hit;
    if (running(sessionsIn(w.path))) { toast(t('この worktree ではセッションが動いています。終わってから削除してください。')); return; }
    const base = repo.base || 'HEAD';
    const p = [`<p><code>${esc(w.path)}</code></p>`];
    if (w.prunable) p.push(`<p>${t('フォルダはすでにありません。git に残っている記録を消します。')}</p>`);
    if (w.changes) p.push(`<p class="wt-warn-text">${t('未コミットの変更が {n} 件あります。削除すると失われます。', { n: w.changes })}</p>`);
    if (w.branch) {
      if (w.ahead) {
        p.push(`<label class="wt-check"><input type="checkbox" id="wt-del-branch"> ${t('ブランチ {branch} も削除する', { branch: `<code>${esc(w.branch)}</code>` })}</label>`);
        p.push(`<p class="note">${t('{base} にないコミットが {n} 件あります。チェックしなければブランチは残り、あとから git で取り出せます。', { base: esc(base), n: w.ahead })}</p>`);
      } else p.push(`<p>${t('ブランチ {branch} は、マージ済みなら一緒に削除し、そうでなければ残します。', { branch: `<code>${esc(w.branch)}</code>` })}</p>`);
    }
    p.push(`<p class="note">${t('会話の記録は残ります。')}</p>`);
    modal({
      title: t('worktree を削除'),
      body: p.join(''),
      actions: [
        { label: t('キャンセル') },
        {
          label: w.changes ? t('変更を捨てて削除') : t('削除'), danger: true,
          onClick: async (card) => {
            const deleteBranch = !!$('#wt-del-branch', card)?.checked;
            try { doneToast(await post(repo, w, { force: !!w.changes, deleteBranch })); }
            catch (e) {
              // Files changed since the list was read: ask again with what is there now.
              if (e.status !== 409 || !/modified or untracked|contains modified|is dirty/.test(e.message)) throw e;
              toast(t('未コミットの変更が増えています。内容を確認してください。'));
              setTimeout(() => remove(w.path, { fresh: true }));
              return;
            }
            load();
          },
        },
      ],
    });
  }
  /// Remove every worktree that holds nothing to lose.
  function cleanup() {
    const list = cleanable();
    if (!list.length) return;
    const items = list.map(({ repo, w }) => `<li><code>${esc(label(w))}</code> <span class="muted">${esc(repo.name)}</span></li>`).join('');
    modal({
      title: t('worktree を片付ける'),
      body: `<p>${t('未コミットの変更も、マージされていないコミットもない worktree を削除します。フォルダが消えているものは、git の記録を消します。マージされていないブランチは残します。')}</p><ul class="wt-list">${items}</ul>`,
      actions: [
        { label: t('キャンセル') },
        {
          label: t('{n} 件を削除', { n: list.length }), danger: true,
          onClick: async (card) => {
            const failed = [];
            let ok = 0;
            const btn = $('.modal-actions .btn.danger', card);
            // One at a time: git locks the repository's worktree list while it changes.
            for (const [i, { repo, w }] of list.entries()) {
              if (btn) btn.textContent = t('削除中… {i}/{n}', { i: i + 1, n: list.length });
              try { await post(repo, w); ok++; } catch (e) { failed.push(`${label(w)}: ${e.message}`); }
            }
            toast(t('{n} 件の worktree を削除しました', { n: ok }));
            await load();
            // The report replaces this dialog in the same modal: keep modal() from closing it.
            if (failed.length) { showOutput(t('削除できなかった worktree'), failed.join('\n')); return true; }
          },
        },
      ],
    });
  }

  // --------------------------------------------------------------- bind
  function bind() {
    const el = $('#wt-body');
    el.addEventListener('click', (e) => {
      const act = e.target.closest('[data-wt]');
      if (act) { if (act.dataset.wt === 'refresh') load(); else if (act.dataset.wt === 'clean') cleanup(); return; }
      const row = e.target.closest('.wt-row');
      if (!row) return;
      const path = row.dataset.path;
      if (e.target.closest('.wt-del')) { remove(path); return; }
      if (e.target.closest('.wt-chat')) { const s = sessionsIn(path)[0]; if (s) OY.chat.open(s.id); return; }
      if (find(path)?.w.prunable) { toast(t('フォルダが見つかりません')); return; }
      OY.setActiveRepo(path);
      OY.sidebar.show(e.target.closest('.wt-open') ? 'explorer' : 'git');
    });
    bus.on('sb-view', () => { if (visible() && Date.now() - wv.at > 3000) load(); });
    bus.on('sessions', () => {
      if (!visible() || !wv.repos) return;
      // A session that started in a new worktree: fetch, the list does not have it yet.
      const known = new Set(all().map(({ w }) => norm(w.path)));
      if (state.sessions.some((s) => s.repo?.worktree && !known.has(norm(s.repo.worktree)))) loadSoon(300);
      else render();
    });
    bus.on('files-changed', () => { if (visible() && wv.repos) loadSoon(); });
    bus.on('tick', () => { if (visible()) load(); });
  }
  function init() {
    bind();
    if (visible()) load();
  }

  OY.worktrees = { init, load, remove };
})();
