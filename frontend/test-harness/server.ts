/**
 * Boot a real Grid remote server for a test (docs/primitives.md §13).
 *
 * `startServer()` spawns the built `gridfpv-server` binary on a free port of this computer,
 * with its data in a folder of its own, and makes one link token for it. It is the real
 * program, so what a spec sees pushed to it is what a director would see.
 *
 * The binary is built on demand (`cargo build -p gridfpv-serverd`) if it is missing.
 */
import { type ChildProcess, spawn, spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, rmSync } from 'node:fs';
import { createServer } from 'node:net';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..');

/** Absolute path to the built server binary. */
export const SERVER_BIN = resolve(repoRoot, 'target', 'debug', 'gridfpv-server');

export interface RemoteServer {
  /** Where it answers, such as `http://127.0.0.1:41234`. */
  baseUrl: string;
  /** A link token for its one club. */
  token: string;
  /** What its club is called. */
  club: string;
  /** Everything it has printed. */
  readLogs(): string;
  /** Run one of the server's commands on its data (`token new x`), and give what it printed. */
  run(args: string[]): string;
  /** Stop it and remove its data. */
  stop(): Promise<void>;
}

function freePort(): Promise<number> {
  return new Promise((done, reject) => {
    const srv = createServer();
    srv.on('error', reject);
    srv.listen(0, '127.0.0.1', () => {
      const address = srv.address();
      const found = typeof address === 'object' && address ? address.port : 0;
      srv.close(() => done(found));
    });
  });
}

/**
 * `assets` is the folder with the built console, which the server shows as its own page.
 * Left out, the server takes pushes and answers the API and has no page.
 */
export async function startServer(
  club = 'E2E Club',
  assets?: string,
  /** More settings for the server, as its environment: sign-in, say. */
  settings: (baseUrl: string) => Record<string, string> = () => ({})
): Promise<RemoteServer> {
  if (!existsSync(SERVER_BIN)) {
    const built = spawnSync('cargo', ['build', '-p', 'gridfpv-serverd'], {
      cwd: repoRoot,
      stdio: 'inherit'
    });
    if (built.status !== 0 || !existsSync(SERVER_BIN)) {
      throw new Error('failed to build the server binary (cargo build -p gridfpv-serverd)');
    }
  }
  const data = mkdtempSync(join(tmpdir(), 'gridfpv-server-e2e-'));
  const port = await freePort();
  const env: Record<string, string | undefined> = {
    ...process.env,
    ...settings(`http://127.0.0.1:${port}`),
    GRIDFPV_SERVER_DATA_DIR: data,
    GRIDFPV_SERVER_ADDR: `127.0.0.1:${port}`,
    // A folder with no page in it, when none is given: never the checkout's own by chance.
    GRIDFPV_SERVER_ASSETS: assets ?? join(data, 'no-page')
  };
  const cli = (args: string[]): string => {
    const ran = spawnSync(SERVER_BIN, args, { env, encoding: 'utf8' });
    if (ran.status !== 0) {
      throw new Error(`gridfpv-server ${args.join(' ')} failed: ${ran.stderr}`);
    }
    return ran.stdout;
  };
  cli(['club', 'name', club]);
  const token = /gfs_[A-Za-z0-9_-]+/.exec(cli(['token', 'new', 'e2e']))?.[0];
  if (!token) throw new Error('gridfpv-server made no token');

  let logs = '';
  const child: ChildProcess = spawn(SERVER_BIN, [], { env, stdio: ['ignore', 'pipe', 'pipe'] });
  child.stdout?.on('data', (chunk: Buffer) => (logs += chunk.toString('utf8')));
  child.stderr?.on('data', (chunk: Buffer) => (logs += chunk.toString('utf8')));
  let exited = false;
  child.on('exit', () => (exited = true));

  const baseUrl = `http://127.0.0.1:${port}`;
  const stop = async (): Promise<void> => {
    if (!exited) {
      await new Promise<void>((done) => {
        child.once('exit', () => done());
        child.kill('SIGTERM');
        setTimeout(() => !exited && child.kill('SIGKILL'), 5_000).unref?.();
      });
    }
    rmSync(data, { recursive: true, force: true });
  };

  const deadline = Date.now() + 30_000;
  for (;;) {
    if (exited) {
      rmSync(data, { recursive: true, force: true });
      throw new Error(`the server exited before it was ready.\n--- server log ---\n${logs}`);
    }
    try {
      const res = await fetch(`${baseUrl}/health`, { signal: AbortSignal.timeout(2_000) });
      if (res.ok) break;
    } catch {
      // not up yet
    }
    if (Date.now() > deadline) {
      await stop();
      throw new Error(`the server never became ready.\n--- server log ---\n${logs}`);
    }
    await new Promise((wait) => setTimeout(wait, 100));
  }
  return { baseUrl, token, club, readLogs: () => logs, stop, run: cli };
}
