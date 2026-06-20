/**
 * Shared helpers for the client↔server contract suite (#13).
 *
 * Everything here drives the **real** Director over the real wire — no mocks. The one
 * deliberate bit of configuration is {@link emptyAssetsDir}: the Director serves the RD
 * console SPA as a `fallback_service`, so an unmatched route normally returns `index.html`
 * (HTTP 200) when the console is built. Pointing `GRIDFPV_ASSETS` at an empty dir makes the
 * fallback deterministic — a `404` with a short text body — *and faithful to the contract*:
 * either way an unknown route is NOT a `Snapshot`, which is exactly what seam 1 asserts.
 */
import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import {
  type Director,
  type StartDirectorOptions,
  startDirector
} from '../test-harness/director.ts';

import type { Command } from '@gridfpv/types';

/**
 * The built-in **Practice** event id every per-event contract route is rooted under (issue
 * #72). Events are first-class containers now — `/events/{eventId}/snapshot|stream|control|…`
 * — and Practice is the always-present in-memory event the contract suite drives against.
 */
export const PRACTICE_EVENT_ID = 'practice';

/** The `/events/practice` route root the per-event contract paths hang off. */
export function eventRoot(baseUrl: string, eventId: string = PRACTICE_EVENT_ID): string {
  return `${baseUrl}/events/${encodeURIComponent(eventId)}`;
}

/** A fresh empty dir to point `GRIDFPV_ASSETS` at, so the SPA fallback is a deterministic 404. */
export function emptyAssetsDir(): string {
  return mkdtempSync(join(tmpdir(), 'gridfpv-no-assets-'));
}

/** Boot a Director with a deterministic (no-SPA) assets dir layered on. */
export function startContractDirector(opts: StartDirectorOptions = {}): Promise<Director> {
  return startDirector({
    ...opts,
    env: { GRIDFPV_ASSETS: emptyAssetsDir(), ...(opts.env ?? {}) }
  });
}

/** The raw HTTP result of a `POST /control` — the status and the parsed `CommandAck`-ish body. */
export interface ControlResult {
  status: number;
  /** The parsed JSON body (a `CommandAck` on the happy path), or `undefined` if not JSON. */
  body: unknown;
}

/** Options for {@link postControl} — override the headers to exercise the auth/Content-Type contract. */
export interface PostControlOptions {
  /** The bearer token to send (omit for no `Authorization` header). */
  token?: string;
  /** Send `Content-Type: application/json` (default `true`; set `false` to test the rejection). */
  contentType?: boolean;
}

/** `POST /control` a single command with explicit header control, returning the raw status + body. */
export async function postControl(
  baseUrl: string,
  command: Command,
  opts: PostControlOptions = {}
): Promise<ControlResult> {
  const { token, contentType = true } = opts;
  const headers: Record<string, string> = {};
  if (contentType) headers['Content-Type'] = 'application/json';
  if (token !== undefined) headers.Authorization = `Bearer ${token}`;
  // Control is rooted under the Practice event (#72): `/events/practice/control`.
  const res = await fetch(`${eventRoot(baseUrl)}/control`, {
    method: 'POST',
    headers,
    body: JSON.stringify(command)
  });
  let body: unknown;
  try {
    body = await res.json();
  } catch {
    body = undefined;
  }
  return { status: res.status, body };
}

/** A `CommandAck` as it actually arrives on the wire. */
export interface WireCommandAck {
  ok: boolean;
  error?: { code: string; message: string };
}

/** `POST /control` with a valid RD token + JSON content-type, asserting nothing — returns the ack. */
export async function rdControl(
  baseUrl: string,
  token: string,
  command: Command
): Promise<WireCommandAck> {
  const { body } = await postControl(baseUrl, command, { token });
  return body as WireCommandAck;
}

/** The `ws(s)://…` base for a Director's `http(s)://…` base URL. */
export function wsBase(baseUrl: string): string {
  return baseUrl.replace(/^http/, 'ws');
}

const sleep = (ms: number): Promise<void> => new Promise((r) => setTimeout(r, ms));

/**
 * Open a raw `WebSocket`, collecting every parsed frame and the close event. Resolves once
 * the socket is open (or rejects if it errors/closes before opening). `headers` are passed
 * to undici's `WebSocket` (Node) — the control WS reads its `Authorization` there.
 */
export async function openSocket(
  url: string,
  headers?: Record<string, string>
): Promise<{
  ws: WebSocket;
  frames: unknown[];
  closed: Promise<{ code: number; reason: string }>;
}> {
  const ws = headers ? new WebSocket(url, { headers } as unknown as string[]) : new WebSocket(url);
  const frames: unknown[] = [];
  let resolveClosed!: (v: { code: number; reason: string }) => void;
  const closed = new Promise<{ code: number; reason: string }>((res) => {
    resolveClosed = res;
  });
  ws.onmessage = (ev: MessageEvent): void => {
    frames.push(JSON.parse(ev.data as string));
  };
  ws.onclose = (ev: CloseEvent): void => resolveClosed({ code: ev.code, reason: ev.reason });
  await new Promise<void>((res, rej) => {
    ws.onopen = (): void => res();
    ws.onerror = (): void => rej(new Error(`websocket failed to open: ${url}`));
  });
  return { ws, frames, closed };
}

/**
 * Try to open a control WebSocket and report whether the upgrade succeeded. The control
 * upgrade is auth-gated, so a missing/invalid token closes the socket instead of opening it.
 */
export async function tryOpenControlWs(
  url: string,
  headers?: Record<string, string>
): Promise<boolean> {
  const ws = headers ? new WebSocket(url, { headers } as unknown as string[]) : new WebSocket(url);
  const opened = await new Promise<boolean>((res) => {
    ws.onopen = (): void => res(true);
    ws.onerror = (): void => res(false);
    ws.onclose = (): void => res(false);
  });
  try {
    ws.close();
  } catch {
    /* ignore */
  }
  return opened;
}

/** Poll `cond` against the latest stream frames until it holds or the deadline passes. */
export async function waitForFrame(
  frames: unknown[],
  cond: (frames: unknown[]) => boolean,
  timeoutMs = 5_000
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (cond(frames)) return;
    await sleep(20);
  }
  throw new Error(`condition not met within ${timeoutMs}ms; frames=${JSON.stringify(frames)}`);
}

/** Drive a heat through its legal loop to `Running` over the control path (schedule already done). */
export async function driveToRunning(baseUrl: string, token: string, heat: string): Promise<void> {
  for (const command of [
    { Stage: { heat } },
    { Arm: { heat } },
    { Start: { heat } }
  ] as Command[]) {
    const ack = await rdControl(baseUrl, token, command);
    if (!ack.ok)
      throw new Error(
        `drive to running failed at ${JSON.stringify(command)}: ${JSON.stringify(ack)}`
      );
  }
}
