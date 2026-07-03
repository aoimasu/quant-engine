/**
 * Idempotently inject a component's CSS into <head>, once per id.
 *
 * The Claude Design primitives co-locate their CSS as a string and inject it on
 * first load (guarded by an element id). We preserve that pattern verbatim so
 * the ported class rules stay a single source of truth and render identically
 * in the built app and in jsdom tests (where Vite's CSS pipeline is disabled).
 */
export function injectCss(id: string, css: string): void {
  if (typeof document === 'undefined') return;
  if (document.getElementById(id)) return;
  const style = document.createElement('style');
  style.id = id;
  style.textContent = css;
  document.head.appendChild(style);
}
