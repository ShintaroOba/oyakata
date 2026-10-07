/* OYAKATA workbench: a tree of split panes holding tabs (chat, file, diff, url…), with
   drag-and-drop to move tabs between panes or split a pane on any edge. */
(() => {
  'use strict';
  const { $, $$, esc, LS, toast } = OY;

  const kinds = new Map();   // kind → factory(desc, tab) → { el, onShow?, onHide?, dispose?, isDirty?, focus? }
  const tabs = new Map();    // key → { desc, el, inst, dirty }
  let layout = null;
  let paneSeq = 0;
  let activePaneId = null;
  let rootEl = null;
  const hint = document.createElement('div');
  hint.className = 'drop-hint';

  // ------------------------------------------------------------ model
  function newPane(role) { return { type: 'pane', id: 'p' + (++paneSeq), role: role || null, tabs: [], active: null }; }
  function walk(node, fn, parent = null, index = -1) {
    fn(node, parent, index);
    if (node.type === 'split') node.children.forEach((c, i) => walk(c, fn, node, i));
  }
  function panes() { const out = []; walk(layout, (n) => { if (n.type === 'pane') out.push(n); }); return out; }
  function findPane(id) { return panes().find((p) => p.id === id) || null; }
  function parentOf(node) { let r = null; walk(layout, (n, p, i) => { if (n === node) r = { parent: p, index: i }; }); return r; }
  function replaceNode(oldNode, newNode) {
    if (layout === oldNode) { layout = newNode; return; }
    const { parent, index } = parentOf(oldNode);
    parent.children[index] = newNode;
  }
  function removePane(pane) {
    if (layout === pane) { pane.tabs = []; pane.active = null; return; }
    const { parent, index } = parentOf(pane);
    parent.children.splice(index, 1);
    parent.sizes.splice(index, 1);
    const sum = parent.sizes.reduce((a, b) => a + b, 0) || 1;
    parent.sizes = parent.sizes.map((s) => s / sum);
    if (parent.children.length === 1) replaceNode(parent, parent.children[0]);
  }
  function splitPane(pane, zone, fresh) {
    const dir = zone === 'left' || zone === 'right' ? 'row' : 'col';
    const first = zone === 'left' || zone === 'top';
    const info = parentOf(pane);
    if (info?.parent && info.parent.dir === dir) {
      const i = info.index;
      const at = first ? i : i + 1;
      const share = info.parent.sizes[i] / 2;
      info.parent.sizes[i] = share;
      info.parent.children.splice(at, 0, fresh);
      info.parent.sizes.splice(at, 0, share);
    } else {
      const split = { type: 'split', dir, children: first ? [fresh, pane] : [pane, fresh], sizes: [0.5, 0.5] };
      replaceNode(pane, split);
    }
  }
  function paneOf(key) { return panes().find((p) => p.tabs.includes(key)) || null; }
  function editorPane() {
    const a = activePaneId && findPane(activePaneId);
    if (a && a.role !== 'bottom') return a;
    const list = panes();
    const found = list.find((p) => p.role !== 'bottom');
    if (found) return found;
    // Only a chat pane exists: give editors their own pane above it, like VS Code's terminal.
    const fresh = newPane(null);
    layout = { type: 'split', dir: 'col', children: [fresh, layout], sizes: [0.55, 0.45] };
    return fresh;
  }
  function bottomPane() {
    const existing = panes().find((p) => p.role === 'bottom');
    if (existing) return existing;
    const fresh = newPane('bottom');
    if (layout.type === 'pane' && layout.tabs.length === 0) {
      // Empty workbench: the chat is the whole screen until an editor opens.
      layout.role = 'bottom';
      return layout;
    }
    layout = { type: 'split', dir: 'col', children: [layout, fresh], sizes: [0.55, 0.45] };
    return fresh;
  }

  // -------------------------------------------------------------- api
  function registerKind(kind, factory) { kinds.set(kind, factory); }
  function open(desc, opts = {}) {
    const existing = tabs.get(desc.key);
    if (existing) { activate(desc.key); return existing; }
    const pane = opts.where === 'bottom' ? bottomPane() : opts.paneId ? (findPane(opts.paneId) || editorPane()) : editorPane();
    const tab = { desc, el: null, inst: null, dirty: false };
    tabs.set(desc.key, tab);
    pane.tabs.push(desc.key);
    pane.active = desc.key;
    activePaneId = pane.id;
    render();
    save();
    return tab;
  }
  function openAt(desc, paneId, zone) {
    const existing = tabs.get(desc.key);
    if (existing) { moveTab(desc.key, paneId, zone); return; }
    tabs.set(desc.key, { desc, el: null, inst: null, dirty: false });
    placeKey(desc.key, paneId, zone);
  }
  function placeKey(key, paneId, zone) {
    const target = findPane(paneId);
    if (!target) return;
    const src = paneOf(key);
    if (src) {
      if (src === target && zone === 'center') { activate(key); return; }
      src.tabs = src.tabs.filter((k) => k !== key);
      if (src.active === key) src.active = src.tabs[src.tabs.length - 1] || null;
    }
    let dest = target;
    if (zone !== 'center') {
      dest = newPane(target.role === 'bottom' && (zone === 'left' || zone === 'right') ? 'bottom' : null);
      splitPane(target, zone, dest);
    }
    dest.tabs.push(key);
    dest.active = key;
    activePaneId = dest.id;
    if (src && src.tabs.length === 0 && src !== dest) removePane(src);
    render();
    save();
  }
  function moveTab(key, paneId, zone) { placeKey(key, paneId, zone); }
  function activate(key) {
    const pane = paneOf(key);
    if (!pane) return;
    pane.active = key;
    activePaneId = pane.id;
    const paneEl = $(`.pane[data-pane="${pane.id}"]`, rootEl);
    if (paneEl) refreshPane(pane, paneEl); else render();
    $$('.pane', rootEl).forEach((p) => p.classList.toggle('focused', p.dataset.pane === pane.id));
    tabs.get(key)?.inst?.onShow?.();
    save();
  }
  async function close(key, { force = false } = {}) {
    const tab = tabs.get(key);
    if (!tab) return;
    if (tab.dirty && !force) {
      const ok = await OY.confirmDialog('保存していない変更があります', `${esc(tab.desc.title)} を閉じると変更は失われます。`, { label: '閉じる', danger: true });
      if (!ok) return;
    }
    const pane = paneOf(key);
    tab.inst?.dispose?.();
    tab.el?.remove();
    tabs.delete(key);
    if (pane) {
      pane.tabs = pane.tabs.filter((k) => k !== key);
      if (pane.active === key) pane.active = pane.tabs[pane.tabs.length - 1] || null;
      if (pane.tabs.length === 0 && panes().length > 1) removePane(pane);
    }
    render();
    save();
  }
  function setTitle(key, title) {
    const tab = tabs.get(key);
    if (!tab) return;
    tab.desc.title = title;
    const el = $(`.ptab[data-key="${CSS.escape(key)}"] .pt-title`, rootEl);
    if (el) el.textContent = title;
    save();
  }
  function setDirty(key, dirty) {
    const tab = tabs.get(key);
    if (!tab || tab.dirty === dirty) return;
    tab.dirty = dirty;
    const el = $(`.ptab[data-key="${CSS.escape(key)}"]`, rootEl);
    if (el) el.classList.toggle('dirty', dirty);
    const d = $('.pt-dirty', el || document.createElement('div'));
    if (d) d.hidden = !dirty;
  }
  function has(key) { return tabs.has(key); }
  function get(key) { return tabs.get(key); }
  function reset() {
    for (const [k, t] of tabs) { t.inst?.dispose?.(); t.el?.remove(); tabs.delete(k); }
    layout = newPane();
    activePaneId = layout.id;
    render();
    save();
  }

  // ------------------------------------------------------------ render
  function ensureInstance(key) {
    const tab = tabs.get(key);
    if (!tab || tab.el) return tab;
    const factory = kinds.get(tab.desc.kind);
    const wrap = document.createElement('div');
    wrap.className = 'tab-content';
    wrap.dataset.key = key;
    if (!factory) {
      wrap.innerHTML = `<div class="loading err">不明な種類: ${esc(tab.desc.kind)}</div>`;
    } else {
      try {
        tab.inst = factory(tab.desc, tab);
        wrap.appendChild(tab.inst.el);
      } catch (e) {
        wrap.innerHTML = `<div class="loading err">${esc(e.message || e)}</div>`;
      }
    }
    tab.el = wrap;
    return tab;
  }
  function tabHtml(key, active) {
    const tab = tabs.get(key);
    if (!tab) return '';
    const d = tab.desc;
    return `<div class="ptab${active ? ' active' : ''}${tab.dirty ? ' dirty' : ''}" draggable="true" data-key="${esc(key)}" title="${esc(d.title)}"><span class="pt-icon">${d.icon || ''}</span><span class="pt-title">${esc(d.title)}</span><span class="pt-dirty"${tab.dirty ? '' : ' hidden'}>●</span><button type="button" class="pt-close" title="閉じる">✕</button></div>`;
  }
  function refreshPane(pane, paneEl) {
    const bar = $('.pane-tabs', paneEl);
    bar.innerHTML = pane.tabs.map((k) => tabHtml(k, k === pane.active)).join('')
      + '<span class="pt-spacer"></span><div class="pt-actions"><button type="button" class="icon-btn small split-right" title="右に分割">◫</button><button type="button" class="icon-btn small split-down" title="下に分割">⬓</button></div>';
    const body = $('.pane-body', paneEl);
    for (const k of pane.tabs) {
      const tab = ensureInstance(k);
      if (tab.el.parentElement !== body) body.appendChild(tab.el);
      const show = k === pane.active;
      if (tab.el.hidden !== !show) {
        tab.el.hidden = !show;
        if (show) tab.inst?.onShow?.(); else tab.inst?.onHide?.();
      }
    }
    let empty = $('.pane-empty', body);
    if (!pane.tabs.length) {
      if (!empty) {
        empty = document.createElement('div');
        empty.className = 'pane-empty';
        empty.innerHTML = `<img class="brand-mark big" src="/assets/icon.svg" alt=""><div>左の一覧からセッションやファイルを開くか、ここにドラッグしてください。</div><div class="hint"><kbd>/</kbd> 検索 &nbsp; <kbd>t</kbd> テーマ &nbsp; タブをドラッグして分割</div>`;
        body.appendChild(empty);
      }
    } else if (empty) empty.remove();
    const active = $(`.ptab[data-key="${CSS.escape(pane.active || '')}"]`, bar);
    active?.scrollIntoView({ inline: 'nearest', block: 'nearest' });
  }
  function render() {
    if (!rootEl) return;
    const keep = new Map($$('.pane', rootEl).map((el) => [el.dataset.pane, el]));
    const build = (node) => {
      if (node.type === 'pane') {
        let el = keep.get(node.id);
        if (!el) {
          el = document.createElement('div');
          el.className = 'pane';
          el.dataset.pane = node.id;
          el.innerHTML = '<div class="pane-tabs"></div><div class="pane-body"></div>';
        }
        el.classList.toggle('focused', node.id === activePaneId);
        refreshPane(node, el);
        return el;
      }
      const el = document.createElement('div');
      el.className = `wb-split ${node.dir}`;
      node.children.forEach((c, i) => {
        if (i > 0) {
          const sp = document.createElement('div');
          sp.className = 'wb-splitter';
          sp.dataset.index = i;
          el.appendChild(sp);
        }
        const child = document.createElement('div');
        child.className = 'wb-child';
        child.style.flex = `0 1 ${(node.sizes[i] * 100).toFixed(3)}%`;
        child.appendChild(build(c));
        el.appendChild(child);
      });
      el._node = node;
      return el;
    };
    const tree = build(layout);
    rootEl.replaceChildren(tree, hint);
    for (const p of panes()) for (const k of p.tabs) if (k === p.active) tabs.get(k)?.inst?.onShow?.();
    OY.bus.emit('layout');
  }

  // --------------------------------------------------------- persistence
  function serialize(node) {
    if (node.type === 'pane') return { type: 'pane', id: node.id, role: node.role, active: node.active, tabs: node.tabs.map((k) => tabs.get(k)?.desc).filter(Boolean) };
    return { type: 'split', dir: node.dir, sizes: node.sizes, children: node.children.map(serialize) };
  }
  function save() { LS.set('wb.layout', serialize(layout)); LS.set('wb.active', activePaneId); }
  function restore() {
    const saved = LS.get('wb.layout', null);
    if (saved) {
      try {
        const build = (n) => {
          if (n.type === 'pane') {
            const num = parseInt(String(n.id || '').replace(/^p/, ''), 10);
            if (num > paneSeq) paneSeq = num;
            const pane = { type: 'pane', id: n.id || ('p' + (++paneSeq)), role: n.role || null, tabs: [], active: null };
            for (const d of n.tabs || []) {
              if (!d?.key || !d?.kind || tabs.has(d.key)) continue;
              tabs.set(d.key, { desc: d, el: null, inst: null, dirty: false });
              pane.tabs.push(d.key);
            }
            pane.active = pane.tabs.includes(n.active) ? n.active : (pane.tabs[pane.tabs.length - 1] || null);
            return pane;
          }
          const children = (n.children || []).map(build).filter(Boolean);
          if (!children.length) return null;
          if (children.length === 1) return children[0];
          const sizes = Array.isArray(n.sizes) && n.sizes.length === children.length ? n.sizes : children.map(() => 1 / children.length);
          return { type: 'split', dir: n.dir === 'row' ? 'row' : 'col', children, sizes };
        };
        layout = build(saved) || newPane();
      } catch (e) {
        console.warn('layout restore failed', e);
        layout = newPane();
      }
    } else layout = newPane();
    activePaneId = LS.get('wb.active', null);
    if (!findPane(activePaneId)) activePaneId = panes()[0].id;
    render();
  }

  // ---------------------------------------------------------------- DnD
  function zoneFor(el, e) {
    const r = el.getBoundingClientRect();
    const x = (e.clientX - r.left) / r.width;
    const y = (e.clientY - r.top) / r.height;
    if (x < 0.22) return 'left';
    if (x > 0.78) return 'right';
    if (y < 0.22) return 'top';
    if (y > 0.78) return 'bottom';
    return 'center';
  }
  function showHint(bodyEl, zone) {
    const r = bodyEl.getBoundingClientRect();
    const root = rootEl.getBoundingClientRect();
    let x = r.left - root.left, y = r.top - root.top, w = r.width, h = r.height;
    if (zone === 'left') w /= 2;
    else if (zone === 'right') { x += w / 2; w /= 2; }
    else if (zone === 'top') h /= 2;
    else if (zone === 'bottom') { y += h / 2; h /= 2; }
    Object.assign(hint.style, { left: x + 'px', top: y + 'px', width: w + 'px', height: h + 'px' });
    hint.classList.add('show');
  }
  function hideHint() { hint.classList.remove('show'); }
  function dragPayload(e) {
    const key = e.dataTransfer.getData('text/oy-tab');
    if (key) return { key };
    const open = e.dataTransfer.getData('text/oy-open');
    if (open) { try { return { desc: JSON.parse(open) }; } catch { return null; } }
    return null;
  }
  function hasPayload(e) { return e.dataTransfer.types.includes('text/oy-tab') || e.dataTransfer.types.includes('text/oy-open'); }

  function bind() {
    rootEl.addEventListener('mousedown', (e) => {
      const paneEl = e.target.closest('.pane');
      if (paneEl && paneEl.dataset.pane !== activePaneId) {
        activePaneId = paneEl.dataset.pane;
        $$('.pane', rootEl).forEach((p) => p.classList.toggle('focused', p === paneEl));
        save();
      }
      const sp = e.target.closest('.wb-splitter');
      if (sp) startResize(sp, e);
    });
    rootEl.addEventListener('click', (e) => {
      const paneEl = e.target.closest('.pane');
      if (!paneEl) return;
      const pane = findPane(paneEl.dataset.pane);
      if (e.target.closest('.split-right') || e.target.closest('.split-down')) {
        const fresh = newPane(pane.role === 'bottom' && e.target.closest('.split-right') ? 'bottom' : null);
        splitPane(pane, e.target.closest('.split-right') ? 'right' : 'bottom', fresh);
        activePaneId = fresh.id;
        render();
        save();
        return;
      }
      const t = e.target.closest('.ptab');
      if (!t) return;
      if (e.target.closest('.pt-close')) close(t.dataset.key);
      else activate(t.dataset.key);
    });
    rootEl.addEventListener('auxclick', (e) => {
      const t = e.target.closest('.ptab');
      if (t && e.button === 1) { e.preventDefault(); close(t.dataset.key); }
    });
    rootEl.addEventListener('dragstart', (e) => {
      const t = e.target.closest('.ptab');
      if (!t) return;
      e.dataTransfer.setData('text/oy-tab', t.dataset.key);
      e.dataTransfer.effectAllowed = 'move';
      t.classList.add('dragging');
    });
    rootEl.addEventListener('dragend', () => { hideHint(); $$('.ptab.dragging', rootEl).forEach((t) => t.classList.remove('dragging')); });
    rootEl.addEventListener('dragover', (e) => {
      if (!hasPayload(e)) return;
      const body = e.target.closest('.pane-body');
      const bar = e.target.closest('.pane-tabs');
      if (!body && !bar) { hideHint(); return; }
      e.preventDefault();
      e.dataTransfer.dropEffect = 'move';
      if (body) showHint(body, zoneFor(body, e));
      else showHint($('.pane-body', bar.parentElement), 'center');
    });
    rootEl.addEventListener('dragleave', (e) => { if (!rootEl.contains(e.relatedTarget)) hideHint(); });
    rootEl.addEventListener('drop', (e) => {
      hideHint();
      const payload = dragPayload(e);
      if (!payload) return;
      const paneEl = e.target.closest('.pane');
      if (!paneEl) return;
      e.preventDefault();
      const body = e.target.closest('.pane-body');
      const zone = body ? zoneFor(body, e) : 'center';
      if (payload.key) moveTab(payload.key, paneEl.dataset.pane, zone);
      else if (payload.desc) openAt(payload.desc, paneEl.dataset.pane, zone);
    });
    window.addEventListener('resize', () => { for (const p of panes()) tabs.get(p.active)?.inst?.onShow?.(); });
  }
  function startResize(sp, e) {
    e.preventDefault();
    const splitEl = sp.parentElement;
    const node = splitEl._node;
    const i = +sp.dataset.index;
    const row = node.dir === 'row';
    const rect = splitEl.getBoundingClientRect();
    const total = row ? rect.width : rect.height;
    const start = row ? e.clientX : e.clientY;
    const a0 = node.sizes[i - 1], b0 = node.sizes[i];
    const children = $$(':scope > .wb-child', splitEl);
    sp.classList.add('dragging');
    const move = (ev) => {
      const delta = ((row ? ev.clientX : ev.clientY) - start) / total;
      const min = 0.08;
      let a = a0 + delta, b = b0 - delta;
      if (a < min) { a = min; b = a0 + b0 - min; }
      if (b < min) { b = min; a = a0 + b0 - min; }
      node.sizes[i - 1] = a;
      node.sizes[i] = b;
      children[i - 1].style.flex = `0 1 ${(a * 100).toFixed(3)}%`;
      children[i].style.flex = `0 1 ${(b * 100).toFixed(3)}%`;
    };
    const up = () => {
      window.removeEventListener('mousemove', move);
      window.removeEventListener('mouseup', up);
      sp.classList.remove('dragging');
      for (const p of panes()) tabs.get(p.active)?.inst?.onShow?.();
      save();
    };
    window.addEventListener('mousemove', move);
    window.addEventListener('mouseup', up);
  }

  function init(el) {
    rootEl = el;
    bind();
  }

  OY.wb = { init, restore, reset, registerKind, open, openAt, close, activate, setTitle, setDirty, has, get, panes: () => panes(), activePane: () => activePaneId };
})();
