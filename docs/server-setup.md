# Setting up a Grid server

A Grid server is a computer that is always on. It keeps your club's events and lets anyone
see them from anywhere. Directors send their events to it from Grid.

You do not need one to race. Race day works the same with or without it.

This page is for the person who runs the server. A director only needs
[the Server help page](help/server.md).

## Two ways to install

- **Without Docker:** one program file and a service. Good for an LXC container or any
  Linux computer. Start at **Without Docker**.
- **With Docker:** one command builds and runs it. Start at **With Docker**.

Both need a way for people to reach the server safely. Grid only talks to a server over
HTTPS. You need one of these:

- a **domain name** pointed at the computer, with ports 80 and 443 open; or
- a **tunnel**, if the computer is at home and you do not want to open ports. See
  **At home, with no open ports**.

## Without Docker

The server is one file, `gridfpv-server`. The web page is inside it. It runs on any Linux,
and needs nothing else installed.

### 1. Get the files

Download `gridfpv-server-linux-x86_64.tar.gz` (or `aarch64` for an ARM computer) and unpack
it:

```
tar -xzf gridfpv-server-linux-x86_64.tar.gz
cd gridfpv-server-linux-x86_64
```

Until Grid has a release with the server in it, the file comes from the **Server builds**
run on GitHub, or you build it yourself. See **Building it yourself** at the end.

### 2. Install it

Run these as root (or put `sudo` in front):

```
useradd --system --home-dir /var/lib/gridfpv-server --shell /usr/sbin/nologin gridfpv
install -m 755 gridfpv-server gridfpv-serverctl /usr/local/bin/
install -m 644 gridfpv-server.service /etc/systemd/system/
systemctl daemon-reload
systemctl enable --now gridfpv-server
```

Check that it is running:

```
systemctl status gridfpv-server
curl http://127.0.0.1:8470/health
```

The server answers only this computer, on port 8470. The next step lets everyone else in,
over HTTPS.

### 3. Give it HTTPS with Caddy

[Install Caddy](https://caddyserver.com/docs/install). On Debian and Ubuntu it is a
package. Then:

1. Point your domain name at this computer, and open ports **80** and **443**.
2. Copy the `Caddyfile` from the download to `/etc/caddy/Caddyfile`.
3. Change `grid.example.org` in it to your own name.
4. Run `systemctl reload caddy`.

Caddy gets an HTTPS certificate by itself and keeps it fresh. Open your name in a browser.
You should see **No events yet**.

If another web server already runs here, such as nginx, point it at
`http://127.0.0.1:8470` in the same way. Make sure it does not hold back (buffer) what the
server sends: the page that follows a race needs each update at once.

### 4. Run commands with `gridfpv-serverctl`

The steps under **Name your club** and **Make a link token** use `gridfpv-serverctl`. It
runs the command as the server's own user, on the server's data.

### Where things are

- The program: `/usr/local/bin/gridfpv-server`
- The data: `/var/lib/gridfpv-server`. Back this folder up to keep your events.
- The log: `journalctl -u gridfpv-server`

### Updating it

Put the new `gridfpv-server` file in place and start it again:

```
install -m 755 gridfpv-server /usr/local/bin/
systemctl restart gridfpv-server
```

## With Docker

You need [Docker](https://docs.docker.com/engine/install/) with Docker Compose, and a copy
of this repo on the computer.

1. Point your domain name at the server. For example `grid.example.org`.
2. Open ports **80** and **443** to the server.
3. In the `docker/server` folder, make a file called `.env` with your name in it:

   ```
   GRID_DOMAIN=grid.example.org
   ```

4. Start it:

   ```
   cd docker/server
   docker compose up -d
   ```

The first start takes a few minutes, because it builds the server. After that, Caddy gets
an HTTPS certificate by itself and keeps it fresh.

Open `https://grid.example.org` in a browser. You should see **No events yet**.

In the steps further down, put `docker compose exec server gridfpv-server` where they say
`gridfpv-serverctl`.

Your data is in the `grid-data` volume. Back that volume up to keep your events.

To update:

```
cd docker/server
git pull
docker compose up -d --build
```

## At home, with no open ports

A tunnel lets people reach a home server without opening your router.
[Cloudflare Tunnel](https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/)
is free and gives you HTTPS.

Install the server one of the two ways above, but skip Caddy. Then set up a tunnel that
sends your public name to `http://localhost:8470`. Follow Cloudflare's own steps for that.

With Docker, start only the server, on this computer:

```
docker build -f docker/server/Dockerfile -t gridfpv-server .
docker run -d --name grid --restart unless-stopped \
  -p 127.0.0.1:8470:8470 -v grid-data:/data gridfpv-server
```

The server answers only this computer (`127.0.0.1`). The tunnel is the one way in.

## Name your club

```
gridfpv-serverctl club name "Recon FPV"
```

## Make a link token for a director

A director needs a **link token** to send events. Make one for each computer that runs
Grid.

There are two ways. Once people can sign in (see **Signing in**), the page is the easy
one.

**On the server's page.** A director of the club, or an owner, opens the club's page and
finds **Link tokens**. Type what the token is for and press **Make a link token**. To take
one back, press **Take back** beside it.

**On the command line.**

```
gridfpv-serverctl token new "Ryan's laptop"
```

To see the tokens you have made:

```
gridfpv-serverctl token list
```

To take one back, use the short id from that list:

```
gridfpv-serverctl token revoke tN81uJZy
```

Either way, the token starts with `gfs_` and **is shown once**. Give it to the director in
a safe way. They paste it into Grid, on **Setup → Server**, with your server's address.

A token that is taken back stops working straight away.

## Signing in

People can have an account on your server. They sign in with Discord or Google. Grid never
sees a password. Nobody can make an account without an invite.

The server's page has **Sign in** at the top once a provider is set up.

### 1. Tell the server its own address

The server must know the address people open, so a provider can send them back to it.

- **Without Docker:** copy `gridfpv-server.env` from the download to
  `/etc/gridfpv-server.env`, and put your address in it:

  ```
  install -m 600 gridfpv-server.env /etc/gridfpv-server.env
  ```

  The file will hold secrets. `600` means only root can read it.

- **With Docker:** the address is taken from `GRID_DOMAIN`. There is nothing to do.

### 2. Register an app with a provider

You do this once for each provider you want. Both are free.

**Discord**

1. Open the [Discord Developer Portal](https://discord.com/developers/applications) and
   make a **New Application**.
2. Under **OAuth2**, add this **Redirect**, with your own address:
   `https://grid.example.org/auth/discord/callback`
3. Copy the **Client ID** and the **Client Secret**.

**Google**

1. Open the [Google Cloud Console](https://console.cloud.google.com/apis/credentials) and
   make an **OAuth client ID** of the type **Web application**.
2. Add this **Authorized redirect URI**, with your own address:
   `https://grid.example.org/auth/google/callback`
3. Copy the **Client ID** and the **Client secret**.

The redirect must match exactly, down to the `https` and no slash on the end.

Grid asks a provider only who the person is: an id and a name. It does not ask for their
email.

**Your own sign-in service**

A club that runs its own sign-in service (anything that speaks OpenID Connect) can use it
too. Give the server its name for the button, its three addresses, and the app's id and
secret. They are in `gridfpv-server.env`, under `OIDC`. Its redirect is
`https://grid.example.org/auth/oidc/callback`.

### 3. Give the server the two values

- **Without Docker:** put them in `/etc/gridfpv-server.env`, then run
  `systemctl restart gridfpv-server`.
- **With Docker:** put them in `docker/server/.env`, then run `docker compose up -d`:

  ```
  DISCORD_CLIENT_ID=...
  DISCORD_CLIENT_SECRET=...
  GOOGLE_CLIENT_ID=...
  GOOGLE_CLIENT_SECRET=...
  ```

The server's log says which providers are on when it starts.

### 4. Make yourself the owner

```
gridfpv-serverctl owner invite
```

It prints a link. Open it and sign in. Whoever does becomes an owner of the server. The
link works once, and for one day.

### 5. Invite your club

Open the server's page, sign in, and open your account from the top corner. An owner sees
**Members, invites and pilots** for each club. From there:

- **One person:** make an invite for someone, pick what they join as, and send them the
  link. It works once. Make your club's directors this way.
- **Anyone with the link:** one link to post where your club talks. You set how many
  people may use it and how many days it lasts. Everyone who uses it joins as a pilot.

A link is shown once, when you make it. If you lose it, take the invite back and make
another.

Directors can invite to their own club and change the roles of its members. They cannot
touch another club. A club always keeps at least one director.

### 6. Tie pilots to accounts

A club's pilots show on its page once a director has sent an event, or their pilot list.
The account that **holds** a pilot gets the results flown under that pilot's callsign.
There are three ways an account comes to hold one:

- **An invite made for the pilot.** When you invite one person, pick **Their pilot**.
  Whoever uses the link gets that pilot's results in the same step.
- **A director gives it.** On the club's page, under **Pilots**, pick a member beside the
  pilot. Or in Grid itself, on **Setup → Pilots**, press **Account** beside the pilot.
- **The person asks.** On their account page they press **Find yourself in** the club and
  pick their callsign. Nothing is theirs yet. A director, or a member with the
  **Approver** role, answers **Yes** or **No** on the club's page.

Knowing a callsign is never enough: someone the club trusts always says yes.

An approver cannot answer their own ask. A director can take a pilot back at any time.

### 7. Hide or remove an event

On the club's page, under **Events**, a director or an owner can:

- **Hide** an event. It leaves the list for everyone else. Anyone who has its link can
  still open it. **Show** puts it back.
- **Remove** an event. It is taken off the server. The director's own copy in Grid is not
  touched. Grid says the event was removed, and does not send it again by itself. The
  director can send it again from **Setup → Server**.

The club's name is changed at the bottom of the same page.

## What is kept

The clubs and tokens are in one file, and each event has a file of its own. The server
never changes an event. It only adds what a director sends.

## A server with its own certificate

If you use a certificate from your own certificate authority, or one the server made for
itself, Grid will not trust it by itself. Give the certificate to your directors. They
tick **The server uses its own certificate** on the Server page and paste it in.

## Building it yourself

You need [Node.js](https://nodejs.org) 22 and [Rust](https://rustup.rs). From the root of
this repo:

```
cd frontend && npm ci && npm run build && cd ..
cargo build --release -p gridfpv-serverd --features embed-page
```

The file is `target/release/gridfpv-server`. The service file, `gridfpv-serverctl` and the
`Caddyfile` are in `packaging/server`.

## What it does not do yet

- A signed-in pilot sees the events they flew and where they placed. While the director's
  Grid is connected they can sign up for a heat, and take an open seat with the race code
  the director shows at the race. Seasons and records come later.
- A director still sends events with a link token. Anyone with its link can look at an
  event.
- There is one club on a server.
- Videos are not stored.
