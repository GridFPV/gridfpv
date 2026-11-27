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

**If the tunnel runs on another computer,** the server has to answer your home network,
not only itself. Without Docker, make a file
`/etc/systemd/system/gridfpv-server.service.d/listen.conf`:

```
[Service]
Environment=GRIDFPV_SERVER_ADDR=0.0.0.0:8470
```

Then run `systemctl daemon-reload` and `systemctl restart gridfpv-server`, and point the
tunnel at `http://<this computer's address>:8470`. To use port 80 instead, write `0.0.0.0:80`
and add these two lines to the same file, since a low port needs leave:

```
AmbientCapabilities=CAP_NET_BIND_SERVICE
CapabilityBoundingSet=CAP_NET_BIND_SERVICE
```

Only do this on a network you trust. Anyone on it reaches the server over plain HTTP.

Set `GRIDFPV_SERVER_PUBLIC_URL` to the tunnel's public `https://` name (see **Signing in**),
so links the server prints are ones people can open.

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

People can have an account on your server. There are two ways to let them sign in. You can
use either, or both.

- **Discord or Google.** People use an account they already have. Grid never sees a
  password. You register a free app with Discord or Google once. Steps 1 to 3 below.
- **A username and a password.** The server keeps them itself. It needs nothing outside
  your server. See **Usernames and passwords** just below.

A server starts with invites only: nobody can make an account without one. You can change
that. See **Let anyone make an account** further down.

The server's page has **Sign in** at the top once one of the two ways is set up.

### Usernames and passwords

This is off on a new server. Switch it on:

```
gridfpv-serverctl password on
```

That is all the setup there is. You can skip steps 1 to 3 if this is the only way you
want. Then make yourself the owner (step 4): open the setup link and press **Make one with
a username and password**.

Once you are an owner you can also switch it on and off on your account page, under
**Signing in with a password**.

- A username is 3 to 32 letters and digits. A password is at least 10 characters.
- The server keeps a scrambled form of each password, never the password itself. Nobody
  can read a password off the server, you included.
- After five wrong passwords in a row, a username waits a minute.
- Someone who signs in with Discord or Google can add a username and password too, on
  their account page.
- **If someone forgets their password,** send them a re-connect link. See **Help someone
  who cannot sign in**. They open it and type a new password.
- If you switch passwords off, anyone whose only way in is a password is locked out
  until you switch it on again. Their passwords are kept.

Use HTTPS. A password typed over plain `http://` can be read on the way.

### Email

A server can send email, if you give it an email account to send from. It is optional. It
is used for two things only:

- **To confirm an address.** A person can give their email address on their account page.
  The server sends a link to it, and the address counts once they open the link.
- **To set a forgotten password.** On the sign-in page, **Forgot your password?** sends a
  link to the person's confirmed address. Without email, you send them a re-connect link
  yourself.

Put the settings of your email account in `/etc/gridfpv-server.env` (or
`docker/server/.env`), then restart the server:

```
GRIDFPV_SERVER_SMTP_HOST=smtp.example.org
GRIDFPV_SERVER_SMTP_USER=grid@example.org
GRIDFPV_SERVER_SMTP_PASSWORD=...
GRIDFPV_SERVER_SMTP_FROM=Grid <grid@example.org>
```

Your email provider gives you these. Look for "SMTP" in its help pages. Many want a
special password made for apps, not the one you sign in with.

- The port is 587 unless you set `GRIDFPV_SERVER_SMTP_PORT`.
- For a provider that wants port 465, add `GRIDFPV_SERVER_SMTP_TLS=tls`.
- The connection is always encrypted. `GRIDFPV_SERVER_SMTP_TLS=none` is taken only for an
  email program on this same computer.
- The server must know its own address (step 1 below), so a link in a mail can be opened.

Check it works:

```
gridfpv-serverctl email test you@example.org
```

If a setting is wrong, the server says which one and does not start.

What to know:

- A person's address is shown to nobody else, not even to you.
- A link to confirm an address works once, for one day.
- A link to set a password works once, for one hour. The same person is not sent another
  for five minutes.
- The sign-in page says the same thing whether or not the username exists, so it cannot be
  used to find out who has an account.
- Email does not join accounts. Two accounts with one address stay two accounts.

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

**Let anyone make an account.** This is your choice as the owner, and it starts off.

- **Off (invite only):** someone needs an invite to make an account.
- **On:** anyone who can sign in with Discord or Google gets an account. A new account
  is in no club. It sees what any visitor sees until it joins a club.

Invites work either way. You can always send one, and so can a club's director.

To change it, open your account page and find **Who may make an account**. Or on the
command line:

```
gridfpv-serverctl sign-up
gridfpv-serverctl sign-up open
gridfpv-serverctl sign-up invite
```

The first one says how it is set now. If you turn it off again, accounts already made
stay.

**Public and private clubs.** A club's director chooses, on the club's page under **Who
can join**. A club starts private.

- **Public:** anyone with an account on your server can join. They press **Join** on
  their account page, or at the club's own address.
- **Private:** a person presses **Ask to join**. The director sees them under **Asked to
  join** and presses **Let in** or **No**.

Either way a director can still invite people. A director can also add someone who has
an account: under **Add someone**, type part of their name or callsign and press **Find**,
then **Add**. Everyone who joins these ways joins as a pilot. The director can change
their role after.

A member can leave a club from their account page. A club's only director cannot leave.

Private is about who can join. It does not hide the club or its events from visitors.

**The pilot profile.** Each person fills in their own on their account page: a callsign,
a colour, a MultiGP id and a Velocidrone id. All of it is optional. It belongs to the
account, so it is the same in every club on your server. The ids are there so a race
director can match a pilot to MultiGP and Velocidrone later.

**The account wins.** Once someone holds a pilot, the server shows that pilot's results
under the callsign and colour in their profile, whatever callsign the event was run with.
The club's Grid takes the callsign, colour and ids too, the next time it is connected. A
part of the profile left empty changes nothing.

### 6. Tie pilots to accounts

A club's pilots show on its page once a director has sent an event they fly in.
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

### 8. More than one club

A server can hold several clubs. Only an **owner** of the server makes one.

**As an owner you can see and change everything in every club on your server.** A club
you host has to be fine with that. The page tells people so where they sign in.

On your account page, find **Clubs on this server**.

- **Make a club:** type its name and press **Make the club**. Then open the club and
  invite its director. The director makes link tokens for their own Grid.
- **The club's address** is shown under its name, like `https://your-server/#/c/recon-fpv`.
  Hand it out. It shows that club's events alone. The address follows the club's name. If
  the club is renamed, the old address keeps working until another club takes that word.
- **Archive** a club you no longer run. It leaves the list of clubs, and nothing in it can
  be changed. Its director's Grid can send no more and is told why. Its invites stop
  working. Its events still open by their links. **Bring back** undoes it, and invites are
  made again. A club cannot be deleted yet.

**Primary club.** Someone in more than one club picks their primary club on their account
page. The front page opens on that club, with a link to every club. It starts as the first
club they joined. A visitor who is not signed in sees every club.

One club's director has no say in another club, and one club's link token sends only into
that club.

**A director of two clubs** saves both in Grid, each with its own link token, and picks a
club for each event. Grid talks to one club at a time: the club of the event that is open.

The same on the command line:

```
gridfpv-serverctl club list
gridfpv-serverctl club new "North Whoops"
gridfpv-serverctl club archive north-whoops
gridfpv-serverctl club restore north-whoops
```

With more than one club, say which club a token or a new name is for. Use the word from
`club list`, as the last word:

```
gridfpv-serverctl token new "North laptop" north-whoops
gridfpv-serverctl club name "Northern Whoops" north-whoops
```

### 9. Help someone who cannot sign in

Someone who gave the server their email address, and confirmed it, can set a forgotten
password by themselves (see **Email**). Everyone else needs you. If someone forgets their
password, or loses the Discord or Google account they sign in with, an **owner** of the
server gets them back in.

On the club's page, under **Members**, press **Sign-ins…** next to the person. You can:

- **Take off** a sign-in. The person is signed out everywhere at once, and that sign-in no
  longer opens their account. Do this when a Discord or Google account was stolen.
- **Make a re-connect link** and send it to the person. They open it and sign in with an
  account they can still use. Or, when passwords are on, they type their username and a
  new password. From then on, that is how they sign in. Their clubs, pilots
  and results stay as they are.

The link works once, and for seven days. Whoever opens it gets into the account, so send
it only to the person it is for. A new link stops the one before it. You can also take a
link back.

Only an owner can do this. A director cannot, because an account is for the whole server
and not for one club.

**If you are the owner and you are locked out**, use the command line on the server:

```
gridfpv-serverctl account list
gridfpv-serverctl account reconnect <id>
```

The first shows every account with its id and how it signs in. The second prints a
re-connect link for the account you name.

## What is kept

The clubs and tokens are in one file, and each event has a file of its own. The server
never changes an event. It only adds what a director sends.

## A server with its own certificate

If you use a certificate from your own certificate authority, or one the server made for
itself, Grid will not trust it by itself. Your directors tick **The server uses its own
certificate** when they save your club on Grid's Server page. Then:

- **A certificate the server made for itself:** the director presses **Look at the
  server's certificate**. Grid shows its fingerprint. Read them yours, and they accept it
  if the two match. To see yours:

  ```
  openssl x509 -in server.crt -noout -fingerprint -sha256
  ```

  The certificate must be made for the address directors type. One made for another name
  cannot be accepted.

- **A certificate from your own certificate authority:** give directors the authority's
  certificate. They paste it in.

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
- Nothing is sent to MultiGP yet. A pilot's MultiGP and Velocidrone ids are kept and
  given to their club's Grid, ready for that.
- Videos are not stored.
