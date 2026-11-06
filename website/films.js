/*
 * The films. Without this file each is a plain <video controls>, which already works.
 * With it: a play button, one film at a time, and the first film running silently while
 * it is on screen, until someone asks for it with sound.
 */
(() => {
  'use strict';
  const films = Array.from(document.querySelectorAll('[data-film]'));
  const reduce = window.matchMedia('(prefers-reduced-motion: reduce)').matches;

  const start = (film, video) => {
    films.forEach((other) => {
      const v = other.querySelector('video');
      if (v !== video && !v.paused) v.pause();
    });
    video.controls = true;
    video.muted = false;
    video.loop = false;
    video.currentTime = 0;
    film.classList.add('is-playing');
    video.play();
  };

  films.forEach((film) => {
    const video = film.querySelector('video');
    const play = film.querySelector('.film-play');
    video.controls = false;
    play.addEventListener('click', () => start(film, video));

    if (!film.hasAttribute('data-autoplay') || reduce || !('IntersectionObserver' in window)) return;
    video.muted = true;
    video.loop = true;
    new IntersectionObserver(([entry]) => {
      if (film.classList.contains('is-playing')) return;
      if (entry.isIntersecting) video.play().catch(() => {});
      else video.pause();
    }, { threshold: 0.35 }).observe(film);
  });
})();
