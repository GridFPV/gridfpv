/**
 * The running Director's version, for the line under the brand wordmark (#467).
 *
 * ## Where the number comes from
 *
 * `GET /about`, same-origin — the console is always served **by** the Director it is talking to, so
 * this is the version of the process actually answering, not the version this bundle was built
 * from. That distinction is the whole point of showing it: the failure it exists to catch is a
 * stale Director serving a new bundle (or the reverse), and a build-time constant baked into the
 * JavaScript would agree with itself in exactly that case and tell the RD nothing. There is no
 * build-time injection to reuse; this endpoint is it.
 *
 * ## Why it is a module singleton
 *
 * {@link Brand} is mounted six times over (the workspace sidebar and five directory pages), and the
 * version now rides with it rather than in a page-corner watermark App.svelte alone could own. One
 * fetch for the app, started on first read and never repeated: the Director's version cannot change
 * without the page reloading with it.
 *
 * A failed or malformed read leaves it `undefined` and the brand renders the wordmark alone — the
 * console is fully usable without a version, and a wrong one is worse than none on a field-support
 * line where the RD is being asked to read it out.
 */

let version = $state<string | undefined>(undefined);
let asked = false;

/**
 * The Director's version string — a release's standard name (`"0.4.0-alpha.1"`) or a
 * non-release build's commit stamp (`"0.4.0-dev-<short hash>"`, #513) — or `undefined` until
 * the read lands, or for good, if it never does. Reading this the first time starts the fetch.
 */
export function directorVersion(): string | undefined {
  if (!asked) {
    asked = true;
    void load();
  }
  return version;
}

async function load(): Promise<void> {
  try {
    const response = await fetch('/about');
    if (!response.ok) return;
    const about: unknown = await response.json();
    // Guarded rather than cast: `/about` answering something unexpected must leave the brand
    // clean, not print `undefined` under the wordmark.
    if (about && typeof about === 'object' && 'version' in about) {
      const value = (about as { version: unknown }).version;
      if (typeof value === 'string' && value.length > 0) version = value;
    }
  } catch {
    /* offline, blocked, or a Director that does not answer — the brand shows no version */
  }
}

/** Reset the cached read. Test-only: the singleton would otherwise leak across cases. */
export function resetDirectorVersion(): void {
  version = undefined;
  asked = false;
}
