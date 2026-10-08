// Drives headless Chrome over the DevTools protocol (no npm packages) to screenshot the
// OYAKATA demo daemon. Usage: node shoot.mjs <base-url> <ids.json> <out-dir>
import { spawn } from 'node:child_process';
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';

const [base, idsPath, outDir] = process.argv.slice(2);
const ids = JSON.parse(readFileSync(idsPath, 'utf8'));
mkdirSync(outDir, { recursive: true });
const CHROME = 'C:/Program Files/Google/Chrome/Application/chrome.exe';
const PORT = 9333;
const W = 1280, H = 800;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const chrome = spawn(CHROME, [
  '--headless=new', '--disable-gpu', `--remote-debugging-port=${PORT}`,
  '--user-data-dir=C:/Temp/oyk-shot/profile', '--no-first-run', '--hide-scrollbars',
  `--window-size=${W},${H}`, '--lang=ja-JP', '--force-device-scale-factor=1', 'about:blank',
], { stdio: 'ignore' });

let wsUrl;
for (let i = 0; i < 40 && !wsUrl; i++) {
  await sleep(250);
  try {
    const list = await (await fetch(`http://127.0.0.1:${PORT}/json`)).json();
    wsUrl = list.find((t) => t.type === 'page')?.webSocketDebuggerUrl;
  } catch {}
}
if (!wsUrl) { chrome.kill(); throw new Error('chrome did not start'); }

const ws = new WebSocket(wsUrl);
await new Promise((r, j) => { ws.onopen = r; ws.onerror = j; });
let seq = 0; const pending = new Map();
ws.onmessage = (ev) => { const m = JSON.parse(ev.data); if (m.id && pending.has(m.id)) { pending.get(m.id)(m); pending.delete(m.id); } };
const cmd = (method, params = {}) => new Promise((resolve, reject) => {
  const id = ++seq; pending.set(id, (m) => m.error ? reject(new Error(method + ': ' + JSON.stringify(m.error))) : resolve(m.result));
  ws.send(JSON.stringify({ id, method, params }));
});
const js = async (expr) => {
  const r = await cmd('Runtime.evaluate', { expression: expr, awaitPromise: true, returnByValue: true });
  if (r.exceptionDetails) throw new Error('js: ' + (r.exceptionDetails.exception?.description || JSON.stringify(r.exceptionDetails)));
  return r.result.value;
};
const shot = async (name) => {
  const r = await cmd('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false });
  writeFileSync(`${outDir}/${name}`, Buffer.from(r.data, 'base64'));
  console.log('wrote', name);
};

await cmd('Page.enable'); await cmd('Runtime.enable');
await cmd('Emulation.setDeviceMetricsOverride', { width: W, height: H, deviceScaleFactor: 1, mobile: false });
await cmd('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-color-scheme', value: 'light' }] });

const q = (s) => JSON.stringify(s);
// Open a hash with a clean slate: the workbench layout is persisted in localStorage, so
// clear it and reload before every shot.
const fresh = async (hash) => {
  await cmd('Page.navigate', { url: base + '/' + (hash || '') });
  await sleep(1500);
  await js(`localStorage.clear()`);
  await cmd('Page.reload');
  await sleep(3500);
};
const setTheme = async (theme) => js(`(() => { const s = document.querySelector('#set-theme'); s.value = ${q(theme)}; s.dispatchEvent(new Event('change')); return s.value; })()`);
const scrollToHeading = async (text) => js(`(() => {
  const h = [...document.querySelectorAll('.transcript h2')].find((e) => e.textContent.trim() === ${q(text)});
  if (!h) return 'no heading';
  const sc = h.closest('.transcript');
  h.scrollIntoView({ block: 'start' });
  if (sc) sc.scrollTop -= 56;
  return 'ok';
})()`);
const steps = (process.env.STEPS || 'hero,team,workbench,new,dark').split(',');

// 1. Hero: the busy session with its rendered answer, scrolled to the top of the answer.
if (steps.includes('hero')) {
  await fresh('#/s/' + ids.S1);
  console.log('scroll:', await scrollToHeading('方針'));
  await sleep(800);
  await shot('hero.png');
  console.log('state:', await js(`JSON.stringify({ counts: document.querySelector('#counts')?.textContent, title: document.title })`));
}

// 2. Team chart for the same session.
if (steps.includes('team')) {
  await fresh('#/s/' + ids.S1);
  await js(`OY.team.open(${q(ids.S1)})`);
  await sleep(2500);
  await shot('team.png');
}

// 3. Workbench: editor on the left, diff on the right, chat below, Git view in the sidebar.
if (steps.includes('workbench')) {
  await fresh('#/s/' + ids.S1);
  await js(`OY.editors.openFile(${q(ids.recipe + '/src/filters.ts')}, ${q(ids.recipe)})`);
  await sleep(1500);
  await js(`OY.editors.openDiff(${q(ids.recipe)}, 'src/filters.ts', false)`);
  await sleep(1200);
  console.log('split:', await js(`(() => { if (OY.wb && OY.wb.splitActive) { OY.wb.splitActive('right'); return 'ok'; } return 'no splitActive'; })()`));
  await sleep(1200);
  await js(`(() => { const b = document.querySelector('#sb-switch .sw[data-view="git"]'); b && b.click(); })()`);
  await sleep(2000);
  await shot('workbench.png');
}

// 4. New-session dialog.
if (steps.includes('new')) {
  await fresh('#/s/' + ids.S1);
  await js(`document.querySelector('#btn-new').click()`);
  await sleep(1500);
  await shot('new-session.png');
}

// 5. Dark theme hero (theme is applied through the settings select, then the page reloads
// so Mermaid re-renders with the dark palette).
if (steps.includes('dark')) {
  await fresh('#/s/' + ids.S1);
  console.log('theme:', await setTheme('dark'));
  await cmd('Page.reload');
  await sleep(3500);
  console.log('scroll:', await scrollToHeading('方針'));
  await sleep(800);
  await shot('hero-dark.png');
}

ws.close(); chrome.kill();
