/**
 * A stand-in sign-in service for a test (docs/primitives.md §13.8).
 *
 * It speaks OpenID Connect the way a real one does, closely enough to hold the server to
 * the rules: a code works once, the PKCE proof must fit, the client's secret must be
 * right. It has no page of its own: whoever is sent to it is sent straight back as the
 * person the test said is at the keyboard (`person`).
 */
import { createHash } from 'node:crypto';
import { createServer, type Server } from 'node:http';

export const CLIENT_ID = 'grid-e2e';
export const CLIENT_SECRET = 'grid-e2e-secret';

export interface Person {
  /** The service's own id for them. */
  sub: string;
  name: string;
}

export interface FakeProvider {
  baseUrl: string;
  /** Who is at the keyboard: the next person to be sent back signed in. */
  person: Person;
  /** The settings that point a server at this service. */
  settings(): Record<string, string>;
  stop(): Promise<void>;
}

export async function startProvider(): Promise<FakeProvider> {
  const codes = new Map<string, { challenge: string; person: Person }>();
  const tokens = new Map<string, Person>();
  let made = 0;
  const provider: FakeProvider = {
    baseUrl: '',
    person: { sub: 'nobody', name: 'Nobody' },
    settings: () => ({
      GRIDFPV_SERVER_OIDC_NAME: 'Test login',
      GRIDFPV_SERVER_OIDC_AUTHORIZE_URL: `${provider.baseUrl}/authorize`,
      GRIDFPV_SERVER_OIDC_TOKEN_URL: `${provider.baseUrl}/token`,
      GRIDFPV_SERVER_OIDC_USERINFO_URL: `${provider.baseUrl}/userinfo`,
      GRIDFPV_SERVER_OIDC_CLIENT_ID: CLIENT_ID,
      GRIDFPV_SERVER_OIDC_CLIENT_SECRET: CLIENT_SECRET
    }),
    stop: () => new Promise((done) => server.close(() => done()))
  };

  const json = (res: import('node:http').ServerResponse, status: number, body: unknown): void => {
    res.writeHead(status, { 'content-type': 'application/json' });
    res.end(JSON.stringify(body));
  };

  const server: Server = createServer((req, res) => {
    const url = new URL(req.url ?? '/', 'http://provider');
    if (url.pathname === '/authorize') {
      const back = url.searchParams.get('redirect_uri') ?? '';
      const challenge = url.searchParams.get('code_challenge') ?? '';
      if (url.searchParams.get('client_id') !== CLIENT_ID || !back || !challenge) {
        return json(res, 400, { error: 'invalid_request' });
      }
      made += 1;
      const code = `code-${made}`;
      codes.set(code, { challenge, person: provider.person });
      const to = new URL(back);
      to.searchParams.set('code', code);
      to.searchParams.set('state', url.searchParams.get('state') ?? '');
      res.writeHead(302, { location: to.toString() });
      return res.end();
    }
    if (url.pathname === '/token' && req.method === 'POST') {
      let body = '';
      req.on('data', (chunk: Buffer) => (body += chunk.toString('utf8')));
      req.on('end', () => {
        const form = new URLSearchParams(body);
        if (
          form.get('client_id') !== CLIENT_ID ||
          form.get('client_secret') !== CLIENT_SECRET ||
          form.get('grant_type') !== 'authorization_code'
        ) {
          return json(res, 401, { error: 'invalid_client' });
        }
        const code = form.get('code') ?? '';
        const known = codes.get(code);
        codes.delete(code);
        const proof = createHash('sha256')
          .update(form.get('code_verifier') ?? '')
          .digest('base64url');
        if (!known || proof !== known.challenge) return json(res, 400, { error: 'invalid_grant' });
        const access = `access-${code}`;
        tokens.set(access, known.person);
        json(res, 200, { access_token: access, token_type: 'Bearer', expires_in: 3600 });
      });
      return;
    }
    if (url.pathname === '/userinfo') {
      const person = tokens.get((req.headers.authorization ?? '').replace(/^Bearer /, ''));
      return person ? json(res, 200, person) : json(res, 401, { error: 'invalid_token' });
    }
    json(res, 404, { error: 'not_found' });
  });
  await new Promise<void>((done) => server.listen(0, '127.0.0.1', done));
  const address = server.address();
  provider.baseUrl = `http://127.0.0.1:${typeof address === 'object' && address ? address.port : 0}`;
  return provider;
}
