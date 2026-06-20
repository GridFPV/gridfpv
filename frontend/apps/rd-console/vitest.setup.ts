// Extends Vitest's `expect` with jest-dom matchers (toBeInTheDocument, etc.) and
// auto-cleans the rendered DOM between tests (via @testing-library/svelte/vite).
import '@testing-library/jest-dom/vitest';

// jsdom doesn't implement the native <dialog> modal API, so screens that use our Dialog
// primitive (Timers, EventTimers, …) can't render under test without this shim. Drive the
// `open` reflected attribute the way a real <dialog> would, so `el.open` and our toggle
// effect behave; we don't need true top-layer/backdrop semantics in unit tests.
if (typeof HTMLDialogElement !== 'undefined') {
  if (!HTMLDialogElement.prototype.showModal) {
    HTMLDialogElement.prototype.showModal = function showModal(this: HTMLDialogElement) {
      this.setAttribute('open', '');
    };
  }
  if (!HTMLDialogElement.prototype.show) {
    HTMLDialogElement.prototype.show = function show(this: HTMLDialogElement) {
      this.setAttribute('open', '');
    };
  }
  if (!HTMLDialogElement.prototype.close) {
    HTMLDialogElement.prototype.close = function close(this: HTMLDialogElement) {
      this.removeAttribute('open');
      this.dispatchEvent(new Event('close'));
    };
  }
}
