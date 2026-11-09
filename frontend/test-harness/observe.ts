/**
 * Manual "observe" entry point (#13, v0.4 observability harness).
 *
 * Hands-on, full-stack debugging without a test: boot the real Director (serving the built RD
 * console SPA) with the same harness the e2e uses, stream its server log live to this terminal,
 * and print the console URL + RD token so you can open a browser and drive the console by hand —
 * server log here, browser console in the browser devtools, side by side.
 *
 * Run it via the frontend-root `observe` npm script:  `npm run observe`
 * (build the SPA first — `npm run build` — or the Director serves the API only).
 *
 * Env passthrough: GRIDFPV_RD_TOKEN and the port (GRIDFPV_PORT) override the defaults below.
 */
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { DIRECTOR_BIN, startDirector } from './director.js';

const here = fileURLToPath(new URL('.', import.meta.url));
const frontendRoot = resolve(here, '..');
const dist = resolve(frontendRoot, 'apps', 'rd-console', 'dist');

async function main(): Promise<void> {
  const port = process.env.GRIDFPV_PORT ? Number(process.env.GRIDFPV_PORT) : 8123;
  const token = process.env.GRIDFPV_RD_TOKEN ?? 'observe-rd-token';

  console.log('Booting the Director (this is the real `gridfpv` binary)…');
  console.log(`  binary : ${DIRECTOR_BIN}`);
  console.log(`  assets : ${dist}`);

  const director = await startDirector({
    port,
    token,
    assets: dist
  });

  console.log('');
  console.log('────────────────────────────────────────────────────────────────────');
  console.log(`  RD console : ${director.baseUrl}/`);
  console.log(`  RD token   : ${director.token}`);
  console.log('  Open the console URL; enter the token above if a write asks for one.');
  console.log('  Server log streams below; open browser devtools for the browser console.');
  console.log('  Ctrl-C to stop.');
  console.log('────────────────────────────────────────────────────────────────────');
  console.log('');

  // Stream the Director's growing log to this terminal.
  let printed = 0;
  const pump = setInterval(() => {
    const all = director.readLogs();
    if (all.length > printed) {
      process.stdout.write(all.slice(printed));
      printed = all.length;
    }
  }, 200);

  const shutdown = async (): Promise<void> => {
    clearInterval(pump);
    await director.stop();
    process.exit(0);
  };
  process.on('SIGINT', shutdown);
  process.on('SIGTERM', shutdown);
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
