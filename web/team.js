/* OYAKATA 体制図 (team view): who is working on a session — you (親方), the main Claude
   (棟梁) and its subagents (職人) — as an org chart. Subagents launched together form a wave
   (陣) so parallel work reads at a glance; each card shows the role, the task, the status and
   what the agent is doing right now. Updates live. */
(() => {
  'use strict';
  const { $, esc, api, state, bus, sessionTitle, fmtDuration, fmtTokens, shortModel, stripCwd, statusLabel, contextWindow } = OY;

  const ROLES = {
    Explore: { icon: '🔍', label: t('調べ役') },
    Plan: { icon: '📐', label: t('段取り役') },
    'general-purpose': { icon: '🛠', label: t('何でも屋') },
    claude: { icon: '🤖', label: t('職人') },
    'statusline-setup': { icon: '⚙', label: t('設定係') },
  };
  const role = (t) => ROLES[t] || { icon: '🧑‍🔧', label: t('職人') };
  const STATUS = {
    running: { label: t('作業中'), cls: 'running' },
    pending: { label: t('準備中'), cls: 'pending' },
    done: { label: t('完了'), cls: 'done' },
    error: { label: t('失敗'), cls: 'error' },
    stopped: { label: t('中断'), cls: 'stopped' },
  };
  /// Agents started within this many seconds of each other count as one wave (parallel).
  const WAVE_GAP_S = 20;

  const since = (ts) => (ts ? (Date.now() - new Date(ts).getTime()) / 1000 : null);
  const oneLine = (s, n) => (s || '').replace(/\s+/g, ' ').trim().slice(0, n);

  function cardHtml(a, cwd, all, depth) {
    const r = role(a.agent_type);
    const st = STATUS[a.status] || STATUS.stopped;
    const active = a.status === 'running' || a.status === 'pending';
    const elapsed = active ? since(a.started_at) : (a.started_at && a.last_at ? (new Date(a.last_at) - new Date(a.started_at)) / 1000 : null);
    let now = '';
    if (a.status === 'running') now = `<div class="tc-now"><span class="lbl">${t("いま")}</span>${esc(a.last_tool ? stripCwd(a.last_tool, cwd) : t('考え中…'))}</div>`;
    else if (a.status === 'pending') now = t('<div class="tc-now"><span class="lbl">いま</span>起動しています…</div>');
    else {
      const report = a.status === 'error' ? a.result : (a.last_text || a.result);
      if (report) now = `<div class="tc-res"><span class="lbl">${a.status === 'error' ? t('エラー') : t('報告')}</span>${esc(oneLine(report, 200))}</div>`;
    }
    const meta = [
      a.agent_type ? `<span class="tc-type">${esc(a.agent_type)}</span>` : '',
      a.tool_calls ? `🔧 ${a.tool_calls}` : '',
      a.context_tokens ? `ctx ${fmtTokens(a.context_tokens)}` : '',
      a.model ? esc(shortModel(a.model)) : '',
      a.background ? t('裏で実行') : '',
    ].filter(Boolean).join(' · ');
    const kids = depth < 3 ? all.filter((c) => c.parent === a.agent_id && a.agent_id) : [];
    return `<div class="tcard ${st.cls}" data-agent="${esc(a.agent_id || '')}" title="${esc(a.prompt || '')}">
      <div class="tc-top"><span class="tc-icon">${r.icon}</span><span class="tc-role">${esc(r.label)}</span><span class="spacer"></span><span class="tc-st">${a.status === 'running' ? '<span class="spinner"></span>' : ''}${st.label}${elapsed != null ? ` · ${fmtDuration(elapsed)}` : ''}</span></div>
      <div class="tc-desc">${esc(a.description || t('（役割の説明なし）'))}</div>
      ${now}
      <div class="tc-meta"><span>${meta}</span>${a.agent_id ? t('<span class="spacer"></span><span class="tc-open">会話 →</span>') : ''}</div>
      ${kids.length ? `<div class="tc-kids"><div class="tk-label">${t("この職人が振った仕事")}</div>${kids.map((k) => cardHtml(k, cwd, all, depth + 1)).join('')}</div>` : ''}
    </div>`;
  }

  /// Group the main agent's subagents into waves by start time.
  function waves(children) {
    const sorted = children.slice().sort((a, b) => (a.started_at || '').localeCompare(b.started_at || '') || (a.index ?? 0) - (b.index ?? 0));
    const out = [];
    for (const a of sorted) {
      const w = out[out.length - 1];
      const t = a.started_at ? new Date(a.started_at).getTime() : null;
      if (w && t != null && w.last != null && t - w.last <= WAVE_GAP_S * 1000) { w.agents.push(a); w.last = t; }
      else out.push({ t, last: t, agents: [a] });
    }
    return out;
  }

  function teamView(desc) {
    const sid = desc.data.session;
    const el = document.createElement('div');
    el.className = 'ev team-view';
    el.innerHTML = t('<div class="loading">読み込み中…</div>');
    let agents = [];
    let visible = true;
    let timer = null;
    let loading = false;
    let debounce = null;

    /// Agent calls that have no subagent transcript yet ("準備中"), from the open chat.
    function pendingCalls() {
      const chat = OY.chat.instances.get(sid);
      if (!chat) return [];
      const known = new Set(agents.map((a) => a.tool_use_id).filter(Boolean));
      return chat.items
        .map((it, index) => ({ it, index }))
        .filter(({ it }) => it.t === 'tool' && (it.name === 'Agent' || it.name === 'Task') && !it.result && !known.has(it.id))
        .map(({ it, index }) => ({ agent_id: null, agent_type: it.input?.subagent_type, description: it.input?.description, prompt: it.input?.prompt, parent: 'main', index, status: 'pending', started_at: it.ts, tool_calls: 0 }));
    }
    function render() {
      const s = state.byId.get(sid);
      const cwd = s?.cwd;
      const all = agents.concat(pendingCalls());
      const running = all.filter((a) => a.status === 'running' || a.status === 'pending').length;
      const done = all.filter((a) => a.status === 'done').length;
      const failed = all.filter((a) => a.status === 'error').length;
      const used = s?.context_tokens || 0;
      const win = contextWindow(s?.model, s?.context_window, used);
      const leadNow = s?.status === 'busy' && s.last_tool ? `<div class="tc-now"><span class="lbl">${t("いま")}</span>${esc(stripCwd(s.last_tool, cwd))}</div>`
        : s?.status === 'waiting' ? t('<div class="tc-now"><span class="lbl">いま</span>親方の判断を待っています</div>') : '';
      const lead = `<div class="tcard lead ${s?.status || 'ended'}" data-lead="1">
        <div class="tc-top"><span class="tc-icon">🏯</span><span class="tc-role">${t("棟梁")}</span><span class="tc-sub">${t("メインの")} ${esc(OY.agentLabel(s?.agent))}</span><span class="spacer"></span><span class="tc-st">${s?.status === 'busy' ? '<span class="spinner"></span>' : ''}${esc(statusLabel(s?.status))}</span></div>
        <div class="tc-desc">${esc(s ? sessionTitle(s) : '')}</div>
        ${leadNow}
        <div class="tc-meta"><span>${esc([s?.model ? shortModel(s.model) : '', used && win ? `ctx ${Math.round((used / win) * 100)}%` : '', s?.tool_calls ? `🔧 ${s.tool_calls}` : ''].filter(Boolean).join(' · '))}</span><span class="spacer"></span><span class="tc-open">${t("会話 →")}</span></div>
      </div>`;
      const top = all.filter((a) => a.parent === 'main' || !all.some((p) => p.agent_id && p.agent_id === a.parent));
      const ws = waves(top);
      const lanes = ws.map((w, i) => {
        const wr = w.agents.filter((a) => a.status === 'running' || a.status === 'pending').length;
        const when = w.t ? new Date(w.t).toLocaleTimeString(OY_I18N.locale(), { hour: '2-digit', minute: '2-digit' }) : '';
        const label = `${t("第")}${i + 1}${t("陣")}${w.agents.length > 1 ? ` · ${w.agents.length} ${t("人で並行")}` : ''}`;
        return `<section class="lane${wr ? ' running' : ''}"><div class="lane-head"><span class="lane-title">${esc(label)}</span>${wr ? `<span class="lane-run"><span class="spinner"></span>${wr} ${t("人作業中")}</span>` : ''}<span class="spacer"></span><span class="lane-time">${when}</span></div>
          <div class="lane-cards">${w.agents.map((a) => cardHtml(a, cwd, all, 0)).join('')}</div></section>`;
      }).join('');
      el.innerHTML = `
        <div class="ev-bar"><span class="path">${t("体制図 —")} ${esc(s ? sessionTitle(s) : sid)}</span>
          <span class="team-counts">${all.length ? `${t("職人")} ${all.length} ${t("人")}` : ''}${running ? ` · <b class="c-run">${t("作業中")} ${running}</b>` : ''}${done ? ` ${t("· 完了")} ${done}` : ''}${failed ? ` · <b class="c-err">${t("失敗")} ${failed}</b>` : ''}</span>
          <button type="button" class="btn small act" data-act="chat">${t("会話へ")}</button><button type="button" class="btn small act" data-act="reload">${t("更新")}</button></div>
        <div class="ev-content"><div class="org">
          <div class="org-top"><div class="tcard boss"><div class="tc-top"><span class="tc-icon">👤</span><span class="tc-role">${t("親方")}</span><span class="tc-sub">${t("あなた")}</span></div></div></div>
          <div class="org-link"></div>
          <div class="org-top">${lead}</div>
          ${all.length ? `<div class="org-link"></div><div class="lanes">${lanes}</div>` : `<div class="org-link"></div><div class="tempty">${t("まだ職人（サブエージェント）はいません。")}${esc(OY.agentLabel(s?.agent))}${t(" がサブエージェントに仕事を振ると、ここに並びます。")}</div>`}
          <div class="team-legend">${t("カードをクリックすると、その職人の会話を開きます。同じ頃に振られた仕事は「陣」にまとめています（並行作業）。カードにマウスを置くと依頼内容が見られます。")}</div>
        </div></div>`;
    }
    async function load() {
      if (loading) return;
      loading = true;
      try {
        const r = await api.get(`/api/sessions/${encodeURIComponent(sid)}/team`);
        agents = r.agents || [];
        const sc = $('.ev-content', el);
        const pos = sc ? sc.scrollTop : null;
        render();
        if (pos != null) $('.ev-content', el).scrollTop = pos;
      } catch (e) {
        el.innerHTML = `<div class="loading err">${esc(e.message)}</div>`;
      } finally { loading = false; }
      schedule();
    }
    function schedule() {
      clearTimeout(timer);
      if (!visible) return;
      const s = state.byId.get(sid);
      const live = s && s.status !== 'ended';
      const working = agents.some((a) => a.status === 'running') || pendingCalls().length;
      timer = setTimeout(load, live && working ? 2000 : live ? 5000 : 30000);
    }
    const soon = () => { clearTimeout(debounce); debounce = setTimeout(load, 400); };
    el.addEventListener('click', (e) => {
      const act = e.target.closest('.act');
      if (act) {
        if (act.dataset.act === 'reload') load();
        else if (act.dataset.act === 'chat') OY.chat.open(sid);
        return;
      }
      const card = e.target.closest('.tcard[data-agent]');
      if (card?.dataset.agent) OY.editors.openAgent(sid, card.dataset.agent);
      else if (e.target.closest('.tcard.lead')) OY.chat.open(sid);
    });
    const unsubs = [
      bus.on('team-changed', (d) => { if (d.session === sid) soon(); }),
      bus.on('sessions', () => { if (visible && !loading) render(); }),
    ];
    load();
    return {
      el,
      onShow: () => { visible = true; soon(); },
      onHide: () => { visible = false; clearTimeout(timer); },
      dispose: () => { clearTimeout(timer); clearTimeout(debounce); unsubs.forEach((u) => u()); },
    };
  }

  function open(sessionId, opts) {
    const s = state.byId.get(sessionId);
    return OY.wb.open({ kind: 'team', key: 'team:' + sessionId, title: `${t("体制図 ·")} ${s ? sessionTitle(s).slice(0, 24) : ''}`, icon: '👥', data: { session: sessionId } }, opts);
  }

  OY.wb.registerKind('team', teamView);
  OY.team = { open };
})();
