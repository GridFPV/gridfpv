# Network

Grid starts out answering only the computer it runs on. The Network page lets phones,
tablets and other computers open Grid too, so people can look at the race.

The Network page is under **Setup**.

## Turning it on

Press **Let other devices open Grid**. The page shows an address, like
`http://192.168.1.20:8081/`. Type that address into the browser on the other device.

The other device must be on the same Wi-Fi or network as the Grid computer. Internet is
not needed.

The first time, Windows may ask whether Grid may use the network. Say yes for private
networks. If you say no, other devices cannot connect.

Press **Turn off** to stop. Devices that were looking are cut off at once.

Grid remembers the setting. If it was on when you closed Grid, it is on when you open it
again.

## What other devices can do

They can look. On a phone, Grid shows three things:

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

They cannot start or stop a race, change a lap, or change any setting. Only the Grid
computer can do those things. This is always true. No setting changes it.

Anyone on the same network can open the address. Turn this on only on a network you are
happy to share the race with.

## More than one address

A computer on two networks, such as Wi-Fi and a cable, has two addresses. The page lists
both. The first one is the one that usually works. If it does not open on the other device,
try the next.

Grid leaves out addresses that no phone could use, such as the ones Docker or a virtual
machine makes.

If the page shows no address, the Grid computer is not on a network.

## The port

The number at the end of the address is the port. Leave it alone unless Grid says the port
is in use. That means another program on the computer has it. Type another number, such as
`8082`, and press **Change port**.

A port must be from 1024 to 65535.

## Running Grid from another device

The Grid computer is the director's. It needs no password.

To run Grid from another device, Grid must be started with an **access token**. That is
set when Grid is started, with `GRIDFPV_RD_TOKEN`. Then open the address on the other
device with `#/director` on the end, and type the token. Nothing on the page links there,
so spectators never see it. The device is signed in for half
a day, or until Grid is closed.

To sign out, open the Network page on that device and press **Sign out on this device**.

After ten wrong tokens in a minute, a device has to wait a minute before it can try again.

With no access token set, nobody can run Grid from another device.

## Always on

If Grid was started on the network already (with `GRIDFPV_ADDR`), the page says it is
always on and has no switch.

## Kiosks

Some pilots have no phone with them, or a flat battery. A **kiosk** is a tablet at your
stand where they type their code and sign up.

Only you can make a tablet a kiosk. A pilot cannot turn a phone into one.

### Setting one up

1. On the tablet, open the kiosk address from the Network page. It ends in `#/kiosk`.
2. On the Network page, press **New kiosk code**.
3. Type the code on the tablet and press **Set up**.

A code works **once**. For a second tablet, make a second code. A code you made and did
not use can be cancelled.

The tablet stays a kiosk, even if it is switched off and on. It is listed under **Kiosks
set up**. Press **Remove** to stop a tablet being a kiosk.

With more than one, press **Name it** and say where each one is ("By the gate"). The
tablet shows its name in the corner while it waits for a pilot.

### Using it

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
