/**
 * @gridfpv/protocol-client — STUB.
 *
 * The thin, framework-agnostic protocol layer described in docs/clients.html §3:
 * connect to a base URL, fetch a projection snapshot, subscribe to the WebSocket
 * change stream, and expose typed state. It is configured only with a base URL,
 * so it cannot tell LAN from Cloud — the same client backs all three surfaces on
 * both transports.
 *
 * This package currently only nails down the public surface so apps can wire
 * against it. The real implementation (snapshot fetch, WS reconnect, typed
 * subscriptions, auth headers) is issue #49.
 */
import type { RaceSnapshot } from '@gridfpv/types';

/** Options for {@link connect}. Expanded by #49 (auth token, transports, etc.). */
export interface ConnectOptions {
  /**
   * Base URL of the Director (or Cloud) protocol server, e.g.
   * `http://director.local:8080` or `https://cloud.gridfpv.example`.
   */
  baseUrl: string;
}

/**
 * A live connection to the protocol server. The shape here is a placeholder; #49
 * defines the real snapshot/subscribe/typed-state API.
 */
export interface ProtocolClient {
  readonly baseUrl: string;
  /** Fetch the current projection snapshot. Implemented by #49. */
  snapshot(): Promise<RaceSnapshot>;
  /** Close the connection and tear down any WebSocket. Implemented by #49. */
  close(): void;
}

/**
 * Connect to a GridFPV protocol server.
 *
 * STUB: signature only. #49 implements snapshot + WS subscribe.
 *
 * @param options - connection options, or a bare base URL string for convenience.
 */
export function connect(options: ConnectOptions | string): ProtocolClient {
  const baseUrl = typeof options === 'string' ? options : options.baseUrl;
  return {
    baseUrl,
    snapshot(): Promise<RaceSnapshot> {
      return Promise.reject(new Error('protocol-client: connect() is a stub — implemented by #49'));
    },
    close(): void {
      /* no-op until #49 */
    }
  };
}
