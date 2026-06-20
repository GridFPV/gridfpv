/**
 * Reusable Director-boot harness (#13, v0.4 observability harness).
 *
 * `startDirector(opts?)` spawns the **real** built `gridfpv` binary — the actual Director that
 * serves the protocol API + the RD console SPA — on an ephemeral (or given) port, buffers its
 * whole stdout+stderr, waits until it is actually serving (polls `GET /health`), and hands back
 * a small handle: the `baseUrl` to point a client at, the known RD `token`, `readLogs()` to read
 * everything the Director has printed so far, and `stop()` for a clean shutdown.
 *
 * It is deliberately framework-agnostic — no Playwright, no vitest imports — so it works equally
 * from the browser e2e (the Playwright observability fixture stands one up and taps its logs into
 * the failure dump) and from the upcoming Node-only contract suite (vitest), which drives the
 * protocol directly with no browser at all.
 *
 * The binary is built on demand (`cargo build -p gridfpv-app`) if it is missing, so a fresh
 * checkout / CI run just works; pass `build: false` to require a pre-built binary.
 */
import { type ChildProcess, spawn, spawnSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { createServer } from 'node:net';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const frontendRoot = resolve(here, '..');
const repoRoot = resolve(frontendRoot, '..');

/** Absolute path to the built Director binary the harness boots. */
export const DIRECTOR_BIN = resolve(repoRoot, 'target', 'debug', 'gridfpv');

/** Options for {@link startDirector}. All optional — the defaults give a fast in-memory sim. */
export interface StartDirectorOptions {
  /** Port to bind. Default: an ephemeral free port picked by the OS. */
  port?: number;
  /**
   * The known RD control token to pin (`GRIDFPV_RD_TOKEN`). Default: a fresh per-call token.
   *
   * Pass **`false`** to start the Director with **no token configured** — `GRIDFPV_RD_TOKEN`
   * is left unset, so the control path is **open** (full-trust by default, #72 Slice 1b). The
   * returned handle's `token` is then the empty string. This is the posture the no-token e2e
   * exercises (picker → Practice → build heat → control with no token step).
   */
  token?: string | false;
  /** Built RD console `dist/` dir to serve as the SPA (`GRIDFPV_ASSETS`). Default: unset (API only). */
  assets?: string;
  /** Number of sim laps per heat (`GRIDFPV_SIM_LAPS`). Default: `4`. */
  simLaps?: number;
  /** Sim lap duration in ms (`GRIDFPV_SIM_LAP_MS`). Default: `250`. */
  simLapMs?: number;
  /** Extra env to layer onto the Director process. */
  env?: Record<string, string>;
  /** Build the binary first if it is missing. Default: `true`. */
  build?: boolean;
  /** Max ms to wait for `/health` to come up. Default: `30_000`. */
  readyTimeoutMs?: number;
}

/** A running Director plus the captured output and a clean shutdown. */
export interface Director {
  /** Base URL the protocol/SPA is served at, e.g. `http://127.0.0.1:54123`. */
  readonly baseUrl: string;
  /** The RD control token registered for this run (the one printed in the logs). */
  readonly token: string;
  /** Everything the Director has printed to stdout+stderr so far, in order. */
  readLogs(): string;
  /** Stop the Director and resolve once its process has exited. Idempotent. */
  stop(): Promise<void>;
}

/** Pick a free TCP port by binding `:0` and reading back the OS-assigned port. */
function pickFreePort(): Promise<number> {
  return new Promise((resolvePort, reject) => {
    const srv = createServer();
    srv.on('error', reject);
    srv.listen(0, '127.0.0.1', () => {
      const addr = srv.address();
      if (addr && typeof addr === 'object') {
        const { port } = addr;
        srv.close(() => resolvePort(port));
      } else {
        srv.close(() => reject(new Error('could not determine a free port')));
      }
    });
  });
}

const sleep = (ms: number): Promise<void> => new Promise((r) => setTimeout(r, ms));

/**
 * Build, spawn, and await a ready Director. Resolves once `GET /health` returns 200; rejects
 * (after dumping the captured logs into the error) if the process dies or never becomes ready.
 */
export async function startDirector(opts: StartDirectorOptions = {}): Promise<Director> {
  const {
    token = `harness-rd-${Math.random().toString(36).slice(2, 10)}`,
    assets,
    simLaps = 4,
    simLapMs = 250,
    env = {},
    build = true,
    readyTimeoutMs = 30_000
  } = opts;
  // `token: false` means "configure no token" — leave GRIDFPV_RD_TOKEN unset so control is
  // open (full-trust, #72). The handle's `token` is then the empty string.
  const tokenEnv: Record<string, string> = token === false ? {} : { GRIDFPV_RD_TOKEN: token };
  const handleToken = token === false ? '' : token;

  // Build the binary on demand so a fresh checkout / CI just works.
  if (build && !existsSync(DIRECTOR_BIN)) {
    const built = spawnSync('cargo', ['build', '-p', 'gridfpv-app'], {
      cwd: repoRoot,
      stdio: 'inherit'
    });
    if (built.status !== 0) {
      throw new Error(`failed to build the Director binary (cargo build -p gridfpv-app)`);
    }
  }
  if (!existsSync(DIRECTOR_BIN)) {
    throw new Error(
      `Director binary not found at ${DIRECTOR_BIN} — run \`cargo build -p gridfpv-app\` (or pass build: true).`
    );
  }

  const port = opts.port ?? (await pickFreePort());
  const baseUrl = `http://127.0.0.1:${port}`;

  // One growing buffer of everything the Director prints; `readLogs()` snapshots it.
  let logBuffer = '';
  const append = (chunk: Buffer): void => {
    logBuffer += chunk.toString('utf8');
  };

  const child: ChildProcess = spawn(DIRECTOR_BIN, [], {
    cwd: repoRoot,
    env: {
      ...process.env,
      GRIDFPV_ADDR: `127.0.0.1:${port}`,
      ...tokenEnv,
      GRIDFPV_SIM_LAPS: String(simLaps),
      GRIDFPV_SIM_LAP_MS: String(simLapMs),
      ...(assets ? { GRIDFPV_ASSETS: assets } : {}),
      ...env
    },
    stdio: ['ignore', 'pipe', 'pipe']
  });
  child.stdout?.on('data', append);
  child.stderr?.on('data', append);

  // If the process exits before/while we wait for ready, surface it as a rejected ready-wait.
  let exited: { code: number | null; signal: NodeJS.Signals | null } | null = null;
  child.on('exit', (code, signal) => {
    exited = { code, signal };
  });

  const readLogs = (): string => logBuffer;

  const stop = async (): Promise<void> => {
    if (exited || child.exitCode !== null || child.signalCode !== null) return;
    await new Promise<void>((resolveStop) => {
      child.once('exit', () => resolveStop());
      child.kill('SIGTERM');
      // Hard-kill if it ignores SIGTERM.
      setTimeout(() => {
        if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL');
      }, 5_000).unref?.();
    });
  };

  // Poll /health until the Director is actually serving.
  const deadline = Date.now() + readyTimeoutMs;
  for (;;) {
    if (exited) {
      throw new Error(
        `Director exited before becoming ready (code=${exited.code}, signal=${exited.signal}).\n` +
          `--- Director log ---\n${logBuffer}`
      );
    }
    try {
      const res = await fetch(`${baseUrl}/health`, { signal: AbortSignal.timeout(2_000) });
      if (res.ok) break;
    } catch {
      // not up yet
    }
    if (Date.now() > deadline) {
      await stop();
      throw new Error(
        `Director did not serve /health within ${readyTimeoutMs}ms at ${baseUrl}.\n` +
          `--- Director log ---\n${logBuffer}`
      );
    }
    await sleep(150);
  }

  return { baseUrl, token: handleToken, readLogs, stop };
}
