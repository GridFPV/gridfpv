import { describe, expect, it, vi } from 'vitest';
import type { Command } from '@gridfpv/types';
import { createControlClient, stringifyCommand, type FetchLike } from '../src/lib/control.js';
import { okAck, failAck } from './fixtures.js';

function jsonResponse(body: unknown, init?: { ok?: boolean; status?: number }): Response {
  return {
    ok: init?.ok ?? true,
    status: init?.status ?? 200,
    json: async () => body
  } as unknown as Response;
}

/** A typed fetch mock so `.mock.calls[0]` is `[url, init]`, not `[]`. */
function mockFetch(impl: FetchLike) {
  return vi.fn<FetchLike>(impl);
}

describe('createControlClient', () => {
  it('POSTs the JSON-serialized Command to {baseUrl}/control with the bearer token', async () => {
    const fetch = mockFetch(async () => jsonResponse(okAck));
    const client = createControlClient('http://d.local:8080/', 'tok-123', { fetch });
    const cmd: Command = { Stage: { heat: 'heat-1' } };

    const ack = await client.sendCommand(cmd);

    expect(ack).toEqual(okAck);
    expect(fetch).toHaveBeenCalledOnce();
    const [url, init] = fetch.mock.calls[0];
    expect(url).toBe('http://d.local:8080/control');
    expect(init?.method).toBe('POST');
    const headers = init?.headers as Record<string, string>;
    expect(headers.Authorization).toBe('Bearer tok-123');
    expect(headers['Content-Type']).toBe('application/json');
    expect(JSON.parse(init?.body as string)).toEqual({ Stage: { heat: 'heat-1' } });
  });

  it('omits the Authorization header when no token is given', async () => {
    const fetch = mockFetch(async () => jsonResponse(okAck));
    const client = createControlClient('http://d.local', undefined, { fetch });
    await client.sendCommand({ Arm: { heat: 'h' } });
    const headers = fetch.mock.calls[0][1]?.headers as Record<string, string>;
    expect(headers.Authorization).toBeUndefined();
  });

  it('passes through a failed CommandAck (ok:false + ProtocolError) verbatim', async () => {
    const fetch = mockFetch(async () => jsonResponse(failAck, { ok: false, status: 409 }));
    const client = createControlClient('http://d.local', 't', { fetch });
    const ack = await client.sendCommand({ Start: { heat: 'h' } });
    expect(ack.ok).toBe(false);
    expect(ack.error).toEqual({ code: 'BadRequest', message: 'illegal transition' });
  });

  it('synthesizes a failed ack from a bare ProtocolError body on non-2xx', async () => {
    const fetch = mockFetch(async () =>
      jsonResponse({ code: 'Unauthorized', message: 'no' }, { ok: false, status: 401 })
    );
    const client = createControlClient('http://d.local', 't', { fetch });
    const ack = await client.sendCommand({ Finish: { heat: 'h' } });
    expect(ack.ok).toBe(false);
    expect(ack.error?.code).toBe('Unauthorized');
  });

  it('never rejects on a transport failure — resolves a failed ack instead', async () => {
    const fetch = mockFetch(async () => {
      throw new Error('network down');
    });
    const client = createControlClient('http://d.local', 't', { fetch });
    const ack = await client.sendCommand({ Score: { heat: 'h' } });
    expect(ack.ok).toBe(false);
    expect(ack.error?.code).toBe('Internal');
  });

  it('renders bigint command fields as JSON numbers (serde u64 default)', () => {
    const cmd: Command = { AdjustLap: { target: 42n, at: 1_500_000n } };
    expect(JSON.parse(stringifyCommand(cmd))).toEqual({
      AdjustLap: { target: 42, at: 1_500_000 }
    });
  });
});
