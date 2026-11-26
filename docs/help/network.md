# Network

Grid starts out answering only the computer it runs on. The Network page lets phones,
tablets and other computers open Grid too: to look at the race, or to run it.

The Network page is under **Setup**.

## Two switches

Grid has two ways in for other devices. Each has its own switch, its own address and its
own port.

- **Public** is for spectators, pilots and kiosks. They can look, and a pilot can sign up.
  Nobody can run the race from the Public address, whatever they type.
- **Remote RD** is for you, when you want to run Grid from another device. It asks for an
  **access token**. See **Running Grid from another device**, further down.

Both are off until you switch them on. You can have one on without the other.

## Turning Public on

Flip the **Public** switch. The card shows an address, like `192.168.1.20`. Type that
address into the browser on the other device.

The other device must be on the same Wi-Fi or network as the Grid computer. Internet is
not needed.

The first time, Windows may ask whether Grid may use the network. Say yes for private
networks. If you say no, other devices cannot connect.

Flip the switch again to stop. Devices that were looking are cut off at once.

Grid remembers both switches. If one was on when you closed Grid, it is on when you open
it again.

## What people see at the Public address

On a phone, Grid shows three things:

- **On now:** the race that is running, with a clock and each pilot's laps as they fly
  them. With no race on, it shows how the last one ended.
- **Up next:** the heats coming up and who is in each.
- **Results:** the ranking of each phase.

A pilot who has signed in (see **Pilot sign-in** on the
[Details](#/help/event) help page) also gets a **Me** tab:

- their own heats, in the order they will fly, and how many heats away each is;
- the seat for each, and the channel when the heat is on a timer;
- their own races, lap by lap, with the best lap marked.

A box at the top tells the pilot when they are two heats away or closer, when they are
next, and when they are flying. The phone buzzes each time they get closer. This only
works while the page is open on the phone: Grid cannot wake a phone that is asleep.

Each seat's channel is shown to everyone in **Up next**.

The page updates by itself. Nobody needs to reload it. If the director opens another event,
the phones follow.

Some things are never shown to other devices: your notes on a race, protests, the video
files, a pilot's real name, and where your timers are on the network.

They cannot start or stop a race, change a lap, or change any setting. This is always
true at the Public address. No token and no setting changes it.

Anyone on the same network can open the address. Turn Public on only on a network you are
happy to share the race with.

## More than one address

A computer on two networks, such as Wi-Fi and a cable, has two addresses. The page lists
both. The first one is the one that usually works. If it does not open on the other device,
try the next.

Grid leaves out addresses that no phone could use, such as the ones Docker or a virtual
machine makes.

If the page shows no address, the Grid computer is not on a network.

## A public address

By default Grid gives out its own number on your network, like `192.168.1.20`. You can
give it a better address for people to use. Type it in the **Public address** box on the
Public card and press **Save**.

Two cases:

- **Your race network has names.** Type the name, such as `http://grid.race.lan`.
- **You put Grid on the internet with a tunnel.** Type the internet address, such as
  `https://race.example.org`. People then do not need to be on your network.

Every address Grid shows for the public pages, and every QR code (a pilot's, the printed
sheet, the kiosk's), then uses it.

Grid does not make the name or the tunnel work. It only uses the address you type. Only
ever point a tunnel at the **Public** port, never at Remote RD.

Clear the box and press **Save** to go back to Grid's own number.

## The port

The number after the colon in an address is the port. Each switch has its own.

- **Public** uses port **80** when it can. That is the port a browser uses by itself, so
  the address has no number on the end and is short to type. Some computers do not let a
  program use port 80. Then Grid uses **8081** and tells you so on the card.
- **Remote RD** uses port **8082**.

To pick your own, type a number in the **Port** box and press **Change port**. Press
**Use the usual port** to let Grid pick again.

The two switches cannot share a port. Neither can use the port Grid itself is on.

If Grid says a port is in use, another program on the computer has it. Pick another.

## Running Grid from another device

The Grid computer is the director's. It needs no password.

To run Grid from a tablet or a second laptop:

1. On the Grid computer, flip the **Remote RD** switch.
2. The card shows an address and an **access token**, like `K7QM-2XWD-9HRA`.
3. On the other device, open that address. It asks for the token. Type it and press
   **Sign in**.

The device now has the whole of Grid, the same as the Grid computer. It stays signed in
for half a day, or until Grid is closed.

**The token.** Grid makes it the first time you turn Remote RD on, and keeps it. You can
read it on the Grid computer at any time. A device that signed in with it is never shown
it. To change it, press **Change…**. Type your own, or leave the box empty and Grid makes
a new one. Changing it signs every other device out. Only the Grid computer can change it.

**Signing out.** On the other device, open the Network page and press **Sign out on this
device**. Turning Remote RD off signs every device out.

**Keep the two addresses apart.** The Remote RD address shows nothing about the race until
the token is typed. The Public address never asks for the token and never takes it. Give
spectators the Public address only.

After ten wrong tokens in a minute, a device has to wait a minute before it can try again.

With Remote RD off, nobody can run Grid from another device.

## Grid itself stays on this computer

Grid's own address, the one the Grid computer uses, only ever answers the Grid computer.
Nothing on this page changes that, and there is no setting that does. Other devices always
come in by Public or Remote RD.

## Kiosks

Some pilots have no phone with them, or a flat battery. A **kiosk** is a tablet at your
stand where they type their code and sign up.

Only you can make a tablet a kiosk. A pilot cannot turn a phone into one.

### Setting one up

1. Turn **Public** on. A kiosk comes in that way.
2. On the tablet, open the kiosk address from the Network page. It ends in `#/kiosk`.
3. On the Network page, press **New kiosk code**.
4. Type the code on the tablet and press **Set up**.

A code works **once**. For a second tablet, make a second code. A code you made and did
not use can be cancelled.

The tablet stays a kiosk, even if it is switched off and on. It is listed under **Kiosks
set up**. Press **Remove** to stop a tablet being a kiosk.

With more than one, press **Name it** and say where each one is ("By the gate"). The
tablet shows its name in the corner while it waits for a pilot.

### Using it

While it waits for a pilot, the kiosk shows a **QR code**. A pilot with a phone can scan
it to open Grid on the phone, and need not queue at the tablet.

At the tablet:

- With nobody at it, the kiosk shows a box for a code and nothing else.
- A pilot types their **own code** and presses **Go**. The codes are on the event's Details
  page.
- They get the sign-up sheet, large: heats down, seats across. It lists the heats that
  are still open. A full heat is listed too, in case someone drops out.
- When they are finished they press **Done**.
- If they walk away, the tablet signs them out by itself after 45 seconds. It counts down
  first. Touching the screen stops the count.
- Using the tablet does **not** sign a pilot out of their own phone.

**Scanning instead of typing.** A handheld barcode scanner that plugs into the tablet can
read a pilot's QR code, from their phone or from paper. It types the code for them. The
tablet's own camera cannot be used for this.

If you give a pilot a new code, or sign them out, they are signed out of the tablet too.
