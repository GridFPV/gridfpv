/**
 * Results-screen export helpers (#56).
 *
 * Export is a JSON download of the typed projection — good enough for v0.4 (#56) and
 * lossless, since it is the exact wire value.
 */

/** Serialize a value to pretty JSON; the bigint replacer is a defensive no-op now
 * that wire numerics are plain `number`s. */
export function toExportJson(value: unknown): string {
  return JSON.stringify(value, (_k, v) => (typeof v === 'bigint' ? Number(v) : v), 2);
}

/**
 * Trigger a browser download of `json` as `filename`. No-op outside a DOM (tests call
 * `toExportJson` directly). Returns whether a download was initiated.
 */
export function downloadJson(filename: string, json: string): boolean {
  if (typeof document === 'undefined' || typeof URL?.createObjectURL !== 'function') return false;
  const blob = new Blob([json], { type: 'application/json' });
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  URL.revokeObjectURL(url);
  return true;
}
