/* OYAKATA small pleasures: the seal stamped on a card when you approve it (判子) and the
   wooden clappers (拍子木) that sound when Claude finishes a turn. The sound is synthesized
   with Web Audio, so nothing is downloaded. Both can be turned off in the settings. */
(() => {
  'use strict';
  const { state, LS } = OY;

  state.fxStamp = LS.get('fxStamp', true);
  state.fxSound = LS.get('fxSound', true);

  // ------------------------------------------------------------------ 判子
  const STAMPS = {
    approve: { top: '親方', main: '承認', cls: 'shu' },
    answer: { top: '親方', main: '回答', cls: 'shu' },
    deny: { top: '', main: '差戻', cls: 'ai square' },
  };
  /// Press a seal over `el` (or at its centre if `el` is about to disappear).
  function stamp(el, kind = 'approve') {
    if (!state.fxStamp || !el) return;
    const s = STAMPS[kind] || STAMPS.approve;
    const r = el.getBoundingClientRect();
    const d = document.createElement('div');
    d.className = `hanko ${s.cls}`;
    d.innerHTML = `${s.top ? `<span class="hk-top">${s.top}</span>` : ''}<span class="hk-main">${s.main}</span>`;
    d.style.left = `${Math.round(r.left + Math.min(r.width * 0.78, r.width - 60))}px`;
    d.style.top = `${Math.round(r.top + Math.min(r.height / 2, 70))}px`;
    document.body.appendChild(d);
    setTimeout(() => d.remove(), 1900);
  }

  // ---------------------------------------------------------------- 拍子木
  let ctx = null;
  let noise = null;
  let lastPlay = 0;
  function audio() {
    if (!ctx) {
      const AC = window.AudioContext || window.webkitAudioContext;
      if (!AC) return null;
      ctx = new AC();
    }
    if (ctx.state === 'suspended') ctx.resume().catch(() => {});
    return ctx;
  }
  // Browsers only let a page start audio after the user interacted with it.
  const unlock = () => { if (state.fxSound) audio(); window.removeEventListener('pointerdown', unlock); window.removeEventListener('keydown', unlock); };
  window.addEventListener('pointerdown', unlock);
  window.addEventListener('keydown', unlock);

  function noiseBuffer(c) {
    if (noise) return noise;
    noise = c.createBuffer(1, Math.floor(c.sampleRate * 0.09), c.sampleRate);
    const d = noise.getChannelData(0);
    for (let i = 0; i < d.length; i++) d[i] = (Math.random() * 2 - 1) * Math.pow(1 - i / d.length, 3);
    return noise;
  }
  /// One strike of two hardwood blocks: a short filtered click plus a few fast-decaying
  /// resonances of the wood.
  function clack(c, out, t, vol) {
    const n = c.createBufferSource();
    n.buffer = noiseBuffer(c);
    const bp = c.createBiquadFilter();
    bp.type = 'bandpass';
    bp.frequency.value = 2900;
    bp.Q.value = 3.5;
    const ng = c.createGain();
    ng.gain.setValueAtTime(vol * 0.8, t);
    ng.gain.exponentialRampToValueAtTime(0.0008, t + 0.06);
    n.connect(bp).connect(ng).connect(out);
    n.start(t);
    n.stop(t + 0.09);
    for (const [f, a, dur] of [[1240, 0.55, 0.12], [2470, 0.32, 0.08], [3900, 0.16, 0.045]]) {
      const o = c.createOscillator();
      o.type = 'sine';
      o.frequency.setValueAtTime(f * 1.03, t);
      o.frequency.exponentialRampToValueAtTime(f, t + 0.015);
      const g = c.createGain();
      g.gain.setValueAtTime(0.0001, t);
      g.gain.exponentialRampToValueAtTime(vol * a, t + 0.002);
      g.gain.exponentialRampToValueAtTime(0.0001, t + dur);
      o.connect(g).connect(out);
      o.start(t);
      o.stop(t + dur + 0.02);
    }
  }
  /// カン、カン — two strikes with a little room echo. `force` plays even when turned off
  /// (the settings' "試しに鳴らす").
  function hyoshigi({ force = false } = {}) {
    if (!state.fxSound && !force) return;
    const now = Date.now();
    if (now - lastPlay < 1500) return; // several sessions finishing together: one clap
    lastPlay = now;
    const c = audio();
    if (!c || c.state !== 'running') return;
    const out = c.createGain();
    out.gain.value = 0.55;
    const delay = c.createDelay(0.5);
    delay.delayTime.value = 0.09;
    const fb = c.createGain();
    fb.gain.value = 0.25;
    const wet = c.createGain();
    wet.gain.value = 0.3;
    out.connect(c.destination);
    out.connect(delay);
    delay.connect(fb).connect(delay);
    delay.connect(wet).connect(c.destination);
    const t = c.currentTime + 0.03;
    clack(c, out, t, 0.85);
    clack(c, out, t + 0.26, 1.0);
    setTimeout(() => { try { out.disconnect(); delay.disconnect(); wet.disconnect(); fb.disconnect(); } catch { /* already gone */ } }, 2500);
  }

  function setStamp(v) { state.fxStamp = v; LS.set('fxStamp', v); }
  function setSound(v) { state.fxSound = v; LS.set('fxSound', v); if (v) { audio(); hyoshigi({ force: true }); } }

  OY.fx = { stamp, hyoshigi, setStamp, setSound };
})();
