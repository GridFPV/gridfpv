# Setting up a Grid server

A Grid server is a computer that is always on. It keeps your club's events and lets anyone
see them from anywhere. Directors send their events to it from Grid.

You do not need one to race. Race day works the same with or without it.

This page is for the person who runs the server. A director only needs
[the Server help page](help/server.md).

## What you need

- A computer that stays on. A small cloud machine or a home server both work.
- [Docker](https://docs.docker.com/engine/install/), with Docker Compose.
- A copy of this repo on that computer.
- One of these, so people can reach it safely:
  - a **domain name** pointed at the computer, with ports 80 and 443 open; or
  - a **tunnel**, if the computer is at home and you do not want to open ports.

Grid only talks to a server over HTTPS. The steps below set that up for you.

## With a domain name

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

## At home, with no open ports

A tunnel lets people reach a home server without opening your router.
[Cloudflare Tunnel](https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/)
is free and gives you HTTPS.

1. Start only the server, on this computer:

   ```
   docker build -f docker/server/Dockerfile -t gridfpv-server .
   docker run -d --name grid --restart unless-stopped \
     -p 127.0.0.1:8470:8470 -v grid-data:/data gridfpv-server
   ```

2. Set up a tunnel that sends your public name to `http://localhost:8470`. Follow
   Cloudflare's own steps for that.

The server answers only this computer (`127.0.0.1`). The tunnel is the one way in.

## Name your club

```
docker compose exec server gridfpv-server club name "Recon FPV"
```

With the tunnel set-up, use `docker exec grid` in place of `docker compose exec server`.

## Make a link token for a director

A director needs a **link token** to send events. Make one for each director:

```
docker compose exec server gridfpv-server token new "Ryan's laptop"
```

It prints a token that starts with `gfs_`. **It is shown once.** Give it to the director
in a safe way. They paste it into Grid, on **Setup → Server**, with your server's address.

To see the tokens you have made:

```
docker compose exec server gridfpv-server token list
```

To take one back, use the short id from that list:

```
docker compose exec server gridfpv-server token revoke tN81uJZy
```

A token that is taken back stops working straight away.

## Where the data is

Everything is in the `grid-data` volume: the clubs and tokens in one file, and one file
for each event. Back that volume up to keep your events.

The server never changes an event. It only adds what a director sends.

## A server with its own certificate

If you use a certificate from your own certificate authority, or one the server made for
itself, Grid will not trust it by itself. Give the certificate to your directors. They
tick **The server uses its own certificate** on the Server page and paste it in.

## Updating

```
cd docker/server
git pull
docker compose up -d --build
```

Your data stays in its volume.

## What it does not do yet

- There are no accounts. Anyone can look. Only a director with a token can send.
- There is one club on a server.
- Videos are not stored.
