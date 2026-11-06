/*
 * Page behaviour for a murmuration site: the theme toggle (with a view
 * transition), moments that play once as a section arrives ([data-play] gets
 * .play; [data-count-to] counts up), the statement that fills in as you read
 * it ([data-fill]), --p (0 to 1) on [data-progress] elements as they cross the
 * screen, pointer light on cards (.card and [data-spot]), the footer wordmark
 * glow ([data-glow] in .site-footer), phone tilt (.phone in a .story .stage or
 * .duo), and hero parallax ([data-parallax] sets --mx/--my). Everything only
 * adds to a page that is already complete; the particles live in
 * murmuration.js. The inline boot script in <head> must set data-theme from
 * storage and add the .js class (see starter.html).
 */
(() => {
  'use strict';

  const root = document.documentElement;
  const reduce = window.matchMedia('(prefers-reduced-motion: reduce)');
  const darkQuery = window.matchMedia('(prefers-color-scheme: dark)');
  const hasObserver = 'IntersectionObserver' in window;
  const THEME_KEY = 'site-theme'; // match the boot script in <head>

  /* ---------- Theme ---------- */
  const themeNow = () => root.dataset.theme || (darkQuery.matches ? 'dark' : 'light');
  const themeMeta = document.querySelector('meta[name="theme-color"]');
  function syncTheme() {
    const mode = themeNow();
    document.querySelectorAll('[data-theme-toggle]').forEach((button) => {
      button.dataset.mode = mode;
      button.setAttribute('aria-label', mode === 'dark' ? 'Switch to light theme' : 'Switch to dark theme');
    });
    if (themeMeta) themeMeta.setAttribute('content', mode === 'dark' ? '#060d1b' : '#8fb8e6');
  }
  function setTheme(next) {
    const apply = () => { root.dataset.theme = next; syncTheme(); };
    if (document.startViewTransition && !reduce.matches) document.startViewTransition(apply);
    else apply();
    try { localStorage.setItem(THEME_KEY, next); } catch (error) { /* storage can be blocked; the choice lasts for this visit */ }
  }
  document.addEventListener('click', (event) => {
    const button = event.target.closest && event.target.closest('[data-theme-toggle]');
    if (button) setTheme(themeNow() === 'dark' ? 'light' : 'dark');
  });
  if (darkQuery.addEventListener) darkQuery.addEventListener('change', syncTheme);
  new MutationObserver(syncTheme).observe(root, { attributes: true, attributeFilter: ['data-theme'] });
  syncTheme();

  /* ---------- Counters: the resting number is the real one; on entry it counts up to it ---------- */
  document.querySelectorAll('[data-count-to]').forEach((el) => {
    const host = el.closest('[data-play]');
    if (!host || !hasObserver || reduce.matches) return;
    const from = Number(el.dataset.countFrom) || 0;
    const to = Number(el.dataset.countTo);
    const final = el.textContent;
    host.addEventListener('scene:play', () => {
      const start = performance.now();
      const tick = (now) => {
        const k = Math.min(1, (now - start) / 1500);
        el.textContent = k < 1 ? String(Math.round(from + (to - from) * (1 - Math.pow(1 - k, 3)))) : final;
        if (k < 1) requestAnimationFrame(tick);
      };
      requestAnimationFrame(tick);
    }, { once: true });
  });

  /* ---------- Moments that play once, when their section arrives ---------- */
  function onEnter(selector, className, options) {
    const els = document.querySelectorAll(selector);
    const fire = (el) => { el.classList.add(className); el.dispatchEvent(new CustomEvent('scene:play')); };
    if (!hasObserver) { els.forEach(fire); return; }
    const observer = new IntersectionObserver((entries) => {
      entries.forEach((entry) => {
        if (!entry.isIntersecting) return;
        observer.unobserve(entry.target);
        fire(entry.target);
      });
    }, options);
    els.forEach((el) => observer.observe(el));
  }
  onEnter('[data-play]', 'play', { rootMargin: '0px 0px -12% 0px', threshold: 0.2 });
  if (!reduce.matches) onEnter('.reveal', 'in', { rootMargin: '0px 0px -4% 0px', threshold: 0.05 });

  /* ---------- The statement fills in as you read it ---------- */
  const fills = Array.from(document.querySelectorAll('[data-fill]'));
  fills.forEach((el) => {
    const walker = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
    const texts = [];
    while (walker.nextNode()) texts.push(walker.currentNode);
    texts.forEach((node) => {
      const parts = node.textContent.split(/(\s+)/);
      const frag = document.createDocumentFragment();
      parts.forEach((part) => {
        if (part === '' ) return;
        if (/^\s+$/.test(part)) { frag.appendChild(document.createTextNode(part)); return; }
        const span = document.createElement('span');
        span.className = 'word';
        span.textContent = part;
        frag.appendChild(span);
      });
      node.parentNode.replaceChild(frag, node);
    });
    el.words = Array.from(el.querySelectorAll('.word'));
    el.lit = 0;
  });

  /* ---------- Scroll-linked: the statement, and --p on [data-progress] ---------- */
  const tracked = Array.from(document.querySelectorAll('[data-progress]'));
  let queued = false;
  function onScroll() {
    queued = false;
    const vh = window.innerHeight || 1;
    fills.forEach((el) => {
      const r = el.getBoundingClientRect();
      const progress = Math.min(1, Math.max(0, (vh * 0.86 - r.top) / (r.height + vh * 0.36)));
      const count = reduce.matches ? el.words.length : Math.round(progress * el.words.length);
      for (; el.lit < count; el.lit++) el.words[el.lit].classList.add('on');
      for (; el.lit > count; el.lit--) el.words[el.lit - 1].classList.remove('on');
    });
    tracked.forEach((el) => {
      const r = el.getBoundingClientRect();
      el.style.setProperty('--p', Math.min(1, Math.max(0, (vh - r.top) / (vh + r.height))).toFixed(3));
    });
  }
  const queue = () => { if (!queued) { queued = true; requestAnimationFrame(onScroll); } };
  window.addEventListener('scroll', queue, { passive: true });
  window.addEventListener('resize', queue);
  onScroll();

  /* ---------- Light that follows the pointer: card spotlights, the footer mark, phone tilt ---------- */
  if (!reduce.matches && window.matchMedia('(pointer: fine)').matches) {
    document.querySelectorAll('.card, [data-spot]').forEach((el) => el.classList.add('spot'));
    let pending = null;
    let frame = 0;
    let tilted = null;
    const level = (el, x, y) => {
      el.querySelectorAll('.phone').forEach((phone) => {
        phone.style.setProperty('--tx', x.toFixed(3));
        phone.style.setProperty('--ty', y.toFixed(3));
      });
    };
    const paint = () => {
      frame = 0;
      const e = pending;
      const target = e.target instanceof Element ? e.target : null;
      if (!target) return;
      const card = target.closest('.spot');
      if (card) {
        const r = card.getBoundingClientRect();
        card.style.setProperty('--sx', `${e.clientX - r.left}px`);
        card.style.setProperty('--sy', `${e.clientY - r.top}px`);
      }
      const footer = target.closest('.site-footer');
      const glow = footer && footer.querySelector('[data-glow]');
      if (glow) {
        const r = glow.getBoundingClientRect();
        glow.style.setProperty('--fx', `${e.clientX - r.left}px`);
        glow.style.setProperty('--fy', `${e.clientY - r.top}px`);
      }
      const stage = target.closest('.story .stage, .duo');
      if (tilted && tilted !== stage) level(tilted, 0, 0);
      tilted = stage;
      if (stage) {
        const r = stage.getBoundingClientRect();
        level(stage, ((e.clientX - r.left) / r.width) * 2 - 1, ((e.clientY - r.top) / r.height) * 2 - 1);
      }
    };
    document.addEventListener('pointermove', (event) => {
      pending = event;
      if (!frame) frame = requestAnimationFrame(paint);
    }, { passive: true });
  }

  /* ---------- The hero leans toward the pointer ---------- */
  const stage = document.querySelector('[data-parallax]');
  if (stage && !reduce.matches && window.matchMedia('(pointer: fine)').matches) {
    const area = stage.closest('section') || document.body;
    let frame = 0;
    let mx = 0;
    let my = 0;
    const paint = () => {
      frame = 0;
      stage.style.setProperty('--mx', mx.toFixed(3));
      stage.style.setProperty('--my', my.toFixed(3));
    };
    area.addEventListener('pointermove', (event) => {
      const r = area.getBoundingClientRect();
      mx = ((event.clientX - r.left) / r.width) * 2 - 1;
      my = ((event.clientY - r.top) / r.height) * 2 - 1;
      if (!frame) frame = requestAnimationFrame(paint);
    });
    area.addEventListener('pointerleave', () => {
      mx = 0;
      my = 0;
      if (!frame) frame = requestAnimationFrame(paint);
    });
  }
})();
