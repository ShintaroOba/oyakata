/* OYAKATA code navigation: go to definition (F12 / Ctrl+click), find references
   (Shift+F12), back / forward (Alt+← / Alt+→), and jumping from file references in the
   conversation (`src/main.rs:42`) to the code. Definitions are found with `git grep -P` and
   a set of declaration patterns that cover the usual languages, so no language server is
   needed. */
(() => {
  'use strict';
  const { api, state, bus, toast, norm, joinPath, relTo, basename } = OY;

  // ------------------------------------------------------------ file lists
  const fileCache = new Map(); // norm(root) → { files, at, pending }
  const FILE_TTL = 30_000;
  /// Files of a repository (relative paths), cached briefly.
  async function files(root, { fresh = false } = {}) {
    if (!root) return [];
    const k = norm(root);
    const c = fileCache.get(k);
    if (c && !fresh && c.files && Date.now() - c.at < FILE_TTL) return c.files;
    if (c?.pending) return c.pending;
    const pending = api.get(`/api/git/tree?root=${encodeURIComponent(root)}`)
      .then((r) => { fileCache.set(k, { files: r.files || [], at: Date.now() }); return r.files || []; })
      .catch((e) => { fileCache.delete(k); throw e; });
    fileCache.set(k, { ...(c || {}), pending });
    return pending;
  }
  function prime(root, list) { if (root && list) fileCache.set(norm(root), { files: list, at: Date.now() }); }
  bus.on('files-changed', (d) => { if (d.root) fileCache.delete(norm(d.root)); });

  async function grep(root, q, { regex = false, pcre = false, word = false, cs = false, glob = '', max = 2000 } = {}) {
    const p = new URLSearchParams({ root, q, max: String(max) });
    if (regex) p.set('regex', '1');
    if (pcre) p.set('pcre', '1');
    if (word) p.set('word', '1');
    if (cs) p.set('case', '1');
    if (glob) p.set('glob', glob);
    return api.get(`/api/git/grep?${p}`);
  }

  // ------------------------------------------------------------ navigation
  const nav = { back: [], fwd: [] };
  function here() {
    const t = OY.wb.activeTab();
    return t?.desc?.kind === 'file' ? t.inst?.location?.() || null : null;
  }
  /// Open `loc` = { path (absolute), root, line, col, select } and remember where we were.
  function jump(loc, { record = true } = {}) {
    if (record) {
      const cur = here();
      if (cur && !(norm(cur.path) === norm(loc.path) && cur.line === loc.line)) {
        nav.back.push(cur);
        if (nav.back.length > 60) nav.back.shift();
        nav.fwd = [];
      }
    }
    OY.editors.openFile(loc.path, loc.root, { line: loc.line, col: loc.col, select: loc.select });
  }
  function back() {
    const loc = nav.back.pop();
    if (!loc) { toast('戻る場所はありません'); return; }
    const cur = here();
    if (cur) nav.fwd.push(cur);
    jump(loc, { record: false });
  }
  function forward() {
    const loc = nav.fwd.pop();
    if (!loc) return;
    const cur = here();
    if (cur) nav.back.push(cur);
    jump(loc, { record: false });
  }

  // ----------------------------------------------------------- definitions
  const reEsc = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const DECL = 'fn|struct|enum|trait|type|mod|union|def|class|interface|record|object|function\\*?|func|fun|const|let|var|val|static|typedef|namespace|module|macro_rules!';
  const MODS = 'public|private|protected|internal|static|final|abstract|synchronized|native|default|override|open|suspend|inline|virtual|async|export|pub(?:\\([^)]*\\))?|unsafe|extern|readonly';
  /// One PCRE alternation of declaration shapes for `sym`.
  function defPattern(sym) {
    const s = reEsc(sym);
    return [
      `\\b(?:${DECL})\\s+(?:\\([^)]*\\)\\s*)?${s}\\b`,                                         // fn x / class X / func (r T) X / const x
      `#\\s*define\\s+${s}\\b`,                                                                // C macros
      `\\b${s}\\s*[:=]\\s*(?:async\\s*)?(?:function\\b|\\([^)]*\\)\\s*(?::[^=]+)?=>|[A-Za-z_$][\\w$]*\\s*=>)`, // x = () => / x: function
      `^\\s*(?:@[\\w.]+(?:\\([^)]*\\))?\\s+)*(?:(?:${MODS})\\s+)+[^=;(]*?\\b${s}\\s*(?:<[^>]*>)?\\s*\\(`, // public static Foo x(
      `^\\s*(?:(?:${MODS})\\s+)+[\\w<>\\[\\]?.,\\s]*\\b${s}\\s*(?:[;=]|$)`,                     // private final Foo x;
      `^\\s*(?!(?:return|new|else|throw|await|yield|case|if|while|for|switch|catch|do)\\b)[\\w<>\\[\\]?.,*&:]+(?:\\s*<[^>]*>)?\\s+\\**${s}\\s*\\([^;]*$`, // Foo x(...) {  (Java/C/C++)
      `^\\s*(?:(?:async|static|get|set)\\s+)*${s}\\s*\\([^)]*\\)\\s*(?::\\s*[^{]+)?\\{`,      // JS/TS class method
    ].join('|');
  }
  function rank(matches, sym, fromRel) {
    const kw = new RegExp(`\\b(?:${DECL})\\s+(?:\\([^)]*\\)\\s*)?${reEsc(sym)}\\b`);
    const ext = (fromRel || '').split('.').pop();
    const dir = (fromRel || '').split('/').slice(0, -1).join('/');
    const seen = new Set();
    return matches
      .filter((m) => { const k = m.path + ':' + m.line; if (seen.has(k)) return false; seen.add(k); return true; })
      .map((m) => {
        let s = 0;
        if (kw.test(m.text)) s += 50;
        if (m.path === fromRel) s += 40;
        if (ext && m.path.endsWith('.' + ext)) s += 25;
        if (dir && m.path.startsWith(dir + '/')) s += 10;
        if (/(^|\/)(tests?|spec|__tests__|__mocks__)\/|\.(test|spec)\./i.test(m.path)) s -= 15;
        if (/^\s*(\/\/|#|\*|\/\*|--)/.test(m.text)) s -= 60;
        s -= m.path.split('/').length;
        return { ...m, score: s };
      })
      .sort((a, b) => b.score - a.score);
  }
  /// Go to the definition of `sym`. `from` = { root, path (absolute), line }.
  async function definition(sym, from) {
    const root = from?.root || state.activeRepo;
    if (!root) { toast('リポジトリが選ばれていません'); return; }
    if (!/^[A-Za-z_$\u00C0-\uFFFF][\w$\u00C0-\uFFFF]*$/.test(sym || '')) { toast('定義を探せる名前ではありません'); return; }
    const fromRel = from?.path ? relTo(root, from.path) : null;
    let res;
    try { res = await grep(root, defPattern(sym), { pcre: true, cs: true, max: 300 }); }
    catch (e) { toast(`定義の検索に失敗: ${e.message}`); return; }
    let hits = rank(res.matches || [], sym, fromRel);
    // Being on the definition already means "show me who uses it", as in VS Code.
    hits = hits.filter((m) => !(m.path === fromRel && m.line === from?.line));
    if (!hits.length) {
      toast(`「${sym}」の定義が見つからないため、参照を表示します`);
      references(sym, root);
      return;
    }
    const toLoc = (m) => ({ path: joinPath(root, m.path), root, line: m.line, select: sym });
    if (hits.length === 1 || hits[0].score - hits[1].score >= 30) { jump(toLoc(hits[0])); return; }
    OY.palette.list({
      title: `「${sym}」の定義候補 ${hits.length} 件`,
      items: hits.slice(0, 80).map((m) => ({ icon: '◆', label: `${basename(m.path)}:${m.line}`, detail: m.path, hint: m.text.trim(), run: () => jump(toLoc(m)) })),
    });
  }
  /// Every whole-word occurrence of `sym`, in the search view.
  function references(sym, root) {
    if (!sym) return;
    if (root && norm(root) !== norm(state.activeRepo)) OY.setActiveRepo(root);
    OY.search.run({ q: sym, word: true, cs: true, regex: false, scope: 'files' });
  }

  // ---------------------------------------------- file references in text
  const EXT = new Set('rs js mjs cjs ts tsx jsx vue svelte java kt kts scala groovy gradle py rb go php cs fs c h cc cpp hpp m mm swift dart lua pl r sql md mdx txt json jsonl yml yaml toml xml html htm css scss sass less sh bash zsh ps1 psm1 bat cmd properties ini cfg conf env lock csv tsv proto graphql gql tf hcl ipynb svg'.split(' '));
  const NAMES = new Set(['Dockerfile', 'Makefile', 'Rakefile', 'Gemfile', 'Procfile', 'Jenkinsfile', 'LICENSE', 'README', 'CLAUDE.md']);
  /// `src/main.rs`, `main.rs:42`, `src/a.ts:10:5`, `docs/x.md#L7`, `C:\x\y.java` → { path, line, col }
  function parseRef(text) {
    const t = (text || '').trim();
    if (!t || t.length > 260 || /\s/.test(t) || /:\/\//.test(t)) return null;
    const m = /^(.+?)(?::(\d+)(?::(\d+))?|#L(\d+)(?:-L?\d+)?)?$/.exec(t);
    if (!m) return null;
    const path = m[1].replace(/^\.\//, '');
    const base = path.split(/[\\/]/).pop();
    const ext = base.includes('.') ? base.split('.').pop().toLowerCase() : '';
    if (!EXT.has(ext) && !NAMES.has(base)) return null;
    if (!/^[\w@.\-~$+/\\:]+$/.test(path) || /^\d+(\.\d+)+$/.test(path)) return null;
    return { path, line: +(m[2] || m[4]) || null, col: +m[3] || null };
  }
  /// Open a reference written in the conversation, resolving it against the repository.
  async function openRef(text, cwd) {
    const ref = parseRef(text);
    if (!ref) return false;
    const repo = OY.repoOfCwd(cwd) || (cwd ? { root: cwd } : null);
    const root = repo?.root || state.activeRepo;
    const p = ref.path.replace(/\\/g, '/');
    if (/^([A-Za-z]:\/|\/)/.test(p)) {
      const rel = relTo(root, ref.path);
      jump({ path: ref.path, root: rel != null ? root : null, line: ref.line, col: ref.col });
      return true;
    }
    if (!root) { toast('リポジトリが分からないため開けません'); return true; }
    let list;
    try { list = await files(root); } catch (e) { toast(e.message); return true; }
    const lower = p.toLowerCase();
    let hits = list.filter((f) => f === p);
    if (!hits.length) hits = list.filter((f) => f.toLowerCase() === lower);
    if (!hits.length) hits = list.filter((f) => f.toLowerCase().endsWith('/' + lower));
    if (!hits.length) { toast(`${p} はこのリポジトリに見つかりません`); return true; }
    const loc = (f) => ({ path: joinPath(root, f), root, line: ref.line, col: ref.col });
    if (hits.length === 1) { jump(loc(hits[0])); return true; }
    OY.palette.list({ title: `${p} の候補`, items: hits.slice(0, 60).map((f) => ({ icon: '📄', label: basename(f), detail: f, run: () => jump(loc(f)) })) });
    return true;
  }

  OY.code = { files, prime, grep, jump, back, forward, definition, references, parseRef, openRef, here, defPattern, rank };
})();
