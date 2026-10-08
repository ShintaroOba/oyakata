/* OYAKATA i18n: the UI is written in Japanese; `t()` looks each string up in the English
   dictionary (web/i18n-en.js) when English is selected. Keys are the Japanese source
   strings themselves, with `{name}` placeholders for interpolated values. Static text in
   index.html is translated in place at startup. Loaded before every other script. */
(() => {
  'use strict';
  const KEY = 'oyakata.lang';
  const read = () => { try { return JSON.parse(localStorage.getItem(KEY)); } catch { return null; } };
  const guess = () => ((navigator.language || '').toLowerCase().startsWith('ja') ? 'ja' : 'en');
  let lang = read() || guess();
  const dicts = { ja: null, en: (window.OY_I18N_EN || {}) };

  function t(key, vars) {
    let s = key;
    if (lang !== 'ja') {
      const d = dicts[lang];
      if (d && Object.prototype.hasOwnProperty.call(d, key)) s = d[key];
    }
    if (vars) s = s.replace(/\{([a-zA-Z0-9_]+)\}/g, (m, k) => (k in vars ? String(vars[k]) : m));
    return s;
  }
  /// Translate the text nodes and common attributes of a static element tree.
  function translateDom(root) {
    if (lang === 'ja') return;
    for (const el of root.querySelectorAll('[data-i18n]')) el.textContent = t(el.dataset.i18n);
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    const nodes = [];
    while (walker.nextNode()) nodes.push(walker.currentNode);
    for (const n of nodes) {
      if (n.parentElement && /^(SCRIPT|STYLE|CODE|PRE)$/.test(n.parentElement.tagName)) continue;
      const raw = n.nodeValue;
      const trimmed = raw.trim();
      if (!trimmed) continue;
      const tr = t(trimmed);
      if (tr !== trimmed) n.nodeValue = raw.replace(trimmed, tr);
    }
    for (const el of root.querySelectorAll('[title],[placeholder],[alt],[aria-label]')) {
      for (const a of ['title', 'placeholder', 'alt', 'aria-label']) {
        const v = el.getAttribute(a);
        if (v) { const tr = t(v); if (tr !== v) el.setAttribute(a, tr); }
      }
    }
  }
  window.OY_I18N = {
    lang: () => lang,
    locale: () => (lang === 'ja' ? 'ja-JP' : 'en-US'),
    /// Whether the user (or the server config) ever picked a language explicitly.
    chosen: () => read() !== null,
    setLang(l) { lang = l === 'en' ? 'en' : 'ja'; try { localStorage.setItem(KEY, JSON.stringify(lang)); } catch { /* ignore */ } },
    t,
    translateDom,
    register(l, dict) { dicts[l] = Object.assign(dicts[l] || {}, dict); },
  };
  window.t = t;
  document.addEventListener('DOMContentLoaded', () => { document.documentElement.lang = lang; translateDom(document.body); });
})();
