/**
 * The privileged control path — the one place the RD console writes (clients.html
 * §1: the RD console is "the ONLY surface with the privileged control path").
 *
 * Commands go up, acknowledgements come down (protocol.html §5): every privileged
 * action is a {@link Command} POSTed to the server, which replies with a single
 * {@link CommandAck} (`ok` + an optional shared {@link ProtocolError}). The console's
 * *reads* still flow through `@gridfpv/protocol-client` (snapshot + WS stream); the
 * resulting projection state comes back on that read stream, never in the ack.
 *
 * ── CONTRACT TO RECONCILE WITH #45 (the server control path) ────────────────────
 * The control endpoint/transport is being built in parallel (#45). This client
 * assumes the simplest shape the contract implies:
 *
 *   • transport: HTTP `POST {baseUrl}/control`
 *   • request body: a JSON-serialized `Command` (the externally-tagged enum verbatim,
 *     e.g. `{"Stage":{"heat":"heat-1"}}`)
 *   • auth: `Authorization: Bearer <token>` (protocol.html §9.4), same token the
 *     read client uses
 *   • response body: a JSON-serialized `CommandAck` (`{ok, error?}`)
 *   • a non-2xx HTTP status is normalized into a failed `CommandAck` whose `error`
 *     is the server's `ProtocolError` body when present, else a synthetic one.
 *
 * The exact path (`/control` vs `/command`), method, and whether commands ride a
 * dedicated control WS instead of POST MUST be reconciled with #45 on merge. The
 * lead will align them. The `Command`/`CommandAck` *types* are taken verbatim from
 * `@gridfpv/types`, so only the transport seam below moves — every screen builds a
 * typed `Command` and calls `sendCommand`, agnostic to how it ships.
 *
 * `Command`'s numeric fields (`SourceTime`, `LogRef`, `Penalty.TimeAdded`) are plain
 * `number`s — bounded far below 2^53 in our domain and rendered as JSON numbers, which
 * is exactly serde's u64/i64 default. {@link stringifyCommand} is a plain
 * `JSON.stringify`; the legacy bigint replacer is kept as a defensive no-op.
 */

import type { Command, CommandAck, ProtocolError } from '@gridfpv/types';

/** Minimal `fetch` surface this client needs (injectable for tests / Node). */
export type FetchLike = (input: string, init?: RequestInit) => Promise<Response>;

/** A control client: turns a typed {@link Command} into a {@link CommandAck}. */
export interface ControlClient {
  /** The base URL commands are POSTed to. */
  readonly baseUrl: string;
  /**
   * Send a privileged command and resolve with its acknowledgement. Never rejects
   * for a server-side / transport failure — those resolve as a failed `CommandAck`
   * (`ok: false`) so callers branch on one shape. Only a programmer error throws.
   */
  sendCommand(command: Command): Promise<CommandAck>;
}

const trimSlash = (s: string): string => (s.endsWith('/') ? s.slice(0, -1) : s);

/**
 * `JSON.stringify` for a `Command`. Its numeric fields are plain `number`s, so this is
 * a straight serialize; the bigint replacer is a defensive no-op for any stray value.
 */
export function stringifyCommand(command: Command): string {
  return JSON.stringify(command, (_k, v) => (typeof v === 'bigint' ? Number(v) : v));
}

const isProtocolError = (v: unknown): v is ProtocolError =>
  typeof v === 'object' &&
  v !== null &&
  'code' in v &&
  'message' in v &&
  typeof (v as ProtocolError).message === 'string';

const isCommandAck = (v: unknown): v is CommandAck =>
  typeof v === 'object' && v !== null && 'ok' in v && typeof (v as CommandAck).ok === 'boolean';

/** Build a failed ack carrying a synthetic protocol error. */
function failedAck(code: ProtocolError['code'], message: string): CommandAck {
  return { ok: false, error: { code, message } };
}

/** Options for {@link createControlClient}. */
export interface ControlClientOptions {
  /** Inject a `fetch` (defaults to the global). Used by tests and Node. */
  fetch?: FetchLike;
}

/**
 * Create a {@link ControlClient} bound to a base URL and optional bearer token.
 *
 * @param baseUrl  Director protocol server base URL (same one the read client uses).
 * @param token    Optional bearer token, sent as `Authorization: Bearer <token>`.
 */
export function createControlClient(
  baseUrl: string,
  token?: string,
  options: ControlClientOptions = {}
): ControlClient {
  const fetchImpl: FetchLike = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  const base = trimSlash(baseUrl);

  return {
    baseUrl: base,
    async sendCommand(command: Command): Promise<CommandAck> {
      const headers: Record<string, string> = {
        'Content-Type': 'application/json',
        Accept: 'application/json'
      };
      if (token) headers.Authorization = `Bearer ${token}`;

      let resp: Response;
      try {
        resp = await fetchImpl(`${base}/control`, {
          method: 'POST',
          headers,
          body: stringifyCommand(command)
        });
      } catch (e) {
        return failedAck('Internal', `control request failed: ${String(e)}`);
      }

      // Parse the body once; reuse it whether the status is ok or not.
      let data: unknown;
      try {
        data = await resp.json();
      } catch {
        data = undefined;
      }

      if (resp.ok) {
        if (isCommandAck(data)) return data;
        // A 2xx with a body that is not a CommandAck is a contract violation; treat
        // it as success only if the server clearly signals it, else flag it.
        return failedAck('Internal', `malformed CommandAck (HTTP ${resp.status})`);
      }

      // Non-2xx: prefer the server's ProtocolError body, else a CommandAck body,
      // else synthesize one from the HTTP status (protocol.html §9.8).
      if (isCommandAck(data)) return data;
      if (isProtocolError(data)) return { ok: false, error: data };
      return failedAck('Internal', `control HTTP ${resp.status}`);
    }
  };
}
