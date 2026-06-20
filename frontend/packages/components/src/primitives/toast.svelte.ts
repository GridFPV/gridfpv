/**
 * Toast store — a tiny global queue of transient notifications, rendered by
 * `ToastHost`. Framework-pure (Svelte 5 runes), no protocol dependency. Any
 * surface calls `toasts.push(...)` (or the `toast.success/error/...` helpers)
 * and mounts one `<ToastHost />`; the host auto-dismisses after `duration`.
 */

export type ToastTone = 'info' | 'success' | 'warn' | 'danger';

export interface Toast {
  id: number;
  tone: ToastTone;
  title?: string;
  message: string;
  /** Auto-dismiss after this many ms; 0 keeps it until dismissed. */
  duration: number;
}

let seq = 0;

class ToastStore {
  items = $state<Toast[]>([]);

  push(t: Omit<Toast, 'id' | 'duration'> & { duration?: number }): number {
    const id = ++seq;
    this.items = [...this.items, { id, duration: t.duration ?? 4000, ...t }];
    return id;
  }

  dismiss(id: number): void {
    this.items = this.items.filter((t) => t.id !== id);
  }

  clear(): void {
    this.items = [];
  }
}

export const toasts = new ToastStore();

/** Convenience helpers for the four tones. */
export const toast = {
  info: (message: string, title?: string) => toasts.push({ tone: 'info', message, title }),
  success: (message: string, title?: string) => toasts.push({ tone: 'success', message, title }),
  warn: (message: string, title?: string) => toasts.push({ tone: 'warn', message, title }),
  error: (message: string, title?: string) =>
    toasts.push({ tone: 'danger', message, title, duration: 6000 })
};
