/**
 * A stand-in email server for the browser specs (docs/primitives.md §13.8).
 *
 * It speaks just enough SMTP for the real `gridfpv-server` to hand it a mail over a real
 * connection, and keeps each mail so a spec can read the link in it. No TLS: it listens
 * on this computer only, which is the one case the server allows without it.
 */
import { createServer, type Server, type Socket } from 'node:net';

/** One mail the server was handed. */
export interface CaughtMail {
  /** Who it is to, as the `RCPT TO` said. */
  to: string;
  subject: string;
  /** The whole mail: headers, an empty line, and the words. */
  text: string;
}

export interface FakeMail {
  port: number;
  /** Every mail caught so far, oldest first. */
  mails: CaughtMail[];
  /** The settings that point a `gridfpv-server` at this. */
  settings(): Record<string, string>;
  /** Wait for a mail to `to` that comes after the first `after` mails. */
  next(to: string, after?: number): Promise<CaughtMail>;
  stop(): Promise<void>;
}

function serve(socket: Socket, mails: CaughtMail[]): void {
  let buffer = '';
  let to = '';
  let data: string[] | undefined;
  const say = (line: string) => socket.write(`${line}\r\n`);
  say('220 test ESMTP');
  socket.on('data', (chunk) => {
    buffer += chunk.toString('utf8');
    for (;;) {
      const end = buffer.indexOf('\r\n');
      if (end < 0) break;
      const line = buffer.slice(0, end);
      buffer = buffer.slice(end + 2);
      if (data) {
        if (line === '.') {
          const text = data.join('\n');
          const subject = /^Subject: (.*)$/m.exec(text)?.[1] ?? '';
          mails.push({ to, subject, text });
          data = undefined;
          say('250 taken');
        } else {
          // A line that starts with a dot is sent with one more dot in front.
          data.push(line.startsWith('..') ? line.slice(1) : line);
        }
        continue;
      }
      const upper = line.toUpperCase();
      if (upper.startsWith('RCPT TO')) {
        to = /<([^>]*)>/.exec(line)?.[1] ?? line;
        say('250 OK');
      } else if (upper === 'DATA') {
        data = [];
        say('354 go on');
      } else if (upper === 'QUIT') {
        say('221 bye');
        socket.end();
      } else {
        say('250 OK');
      }
    }
  });
  socket.on('error', () => undefined);
}

export async function startMail(): Promise<FakeMail> {
  const mails: CaughtMail[] = [];
  // Connections are kept track of, so stopping does not wait for one left open.
  const open = new Set<Socket>();
  const server: Server = createServer((socket) => {
    open.add(socket);
    socket.on('close', () => open.delete(socket));
    serve(socket, mails);
  });
  await new Promise<void>((done) => server.listen(0, '127.0.0.1', done));
  const address = server.address();
  if (address === null || typeof address === 'string') throw new Error('no port');
  const port = address.port;
  return {
    port,
    mails,
    settings: () => ({
      GRIDFPV_SERVER_SMTP_HOST: '127.0.0.1',
      GRIDFPV_SERVER_SMTP_PORT: String(port),
      GRIDFPV_SERVER_SMTP_FROM: 'Grid Test <grid@example.org>',
      GRIDFPV_SERVER_SMTP_TLS: 'none'
    }),
    next: async (to, after = 0) => {
      const until = Date.now() + 15_000;
      for (;;) {
        const found = mails.slice(after).find((mail) => mail.to === to);
        if (found) return found;
        if (Date.now() > until) throw new Error(`no mail to ${to} arrived`);
        await new Promise((done) => setTimeout(done, 50));
      }
    },
    stop: () =>
      new Promise((done) => {
        for (const socket of open) socket.destroy();
        server.close(() => done());
      })
  };
}

/** The link in a mail: the first line that is a web address. */
export function linkIn(mail: CaughtMail): string {
  const link = mail.text.split('\n').find((line) => /^https?:\/\//.test(line.trim()));
  if (!link) throw new Error(`no link in the mail:\n${mail.text}`);
  return link.trim();
}
