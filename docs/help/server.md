# Server

A Grid server is a computer that is always on. It keeps your events after the laptop is
shut, and lets people see them from anywhere.

You do not need one. Race day works the same with or without it.

The Server page is under **Setup**.

## What you need

- **The server's address.** It looks like `https://grid.example.org`.
- **A link token.** It starts with `gfs_`. Whoever runs the server makes one for you. If
  you are a director of your club on the server, you can make one yourself: open your
  club's page there and look for **Link tokens**.

## Saving your club

A club on a server is saved once. Grid keeps it in a list.

1. Press **Add a club**.
2. Type the address.
3. Paste the link token.
4. Give it a name if you want one. If you leave the name empty, Grid uses the name the
   club has on the server.
5. Press **Save**.

Grid never shows the token again. To change it, press **Change…** and type a new one over
it.

Saving a club sends nothing yet. You pick the club for each event.

## Sending an event

Under **Events**, each event has a box that says where it is sent. A new event says **Not
sent**. Pick your club in the box.

The badge at the top says **Connected to** and the name of your club when it works.

You can run an event with no club. Race day works the same. Pick a club later and all of
the event goes up then.

## More than one club

You may run events for two clubs, or keep events on two servers. Save each club. Each one
needs its own link token, even when both are on the same server.

An event goes to one club, or to none.

Grid talks to one club at a time: **the club of the event that is open**. While it does,
every event of that club is kept up to date.

An event of another club says **Waiting**. Open one of that club's events, and Grid goes
over to that club and sends what is waiting.

## What is sent

An event with a club is sent, and kept up to date as you race. A lap is on the server
about a second after it is flown.

- The race itself goes up whole: every lap and every ruling.
- Pilots go up by callsign and colour only. Real names stay on your computer.
- An event's name, date, place, description and organizer go up.
- Only the pilots in an event go up with it. Your whole pilot list does not. A club
  only hears of the pilots who fly in its events.

## Pilots and their accounts

This part works with the club of the event that is open.

People in your club can have an account on the server. When a pilot is tied to their
account, they see the events they flew and where they placed.

You tie them together on the [Pilots](#/help/pilots) page, with **Account**. Or you bring
pilots in from the server with **Add from the server**.

## Someone cannot sign in

People sign in to a server with Discord or Google, or with a username and password if
the server's owner has switched that on. If someone in your club forgets their password,
or loses their Discord or Google account, ask an owner of the server. On the server's
page, the owner can send the person a **re-connect link**. The person opens it, signs in
another way or types a new password, and is back in their own account with their results.

## Signing up from anywhere

A pilot who is tied to their account can sign up for a heat on the server's page, from
home or on the way to the field. They open the event there and use the **Sign up** tab.
It is the same sheet they get on their phone at the race.

- Sign-up must be on for the phase, and open. You set that on the
  [Phases](#/help/phases) page, as always.
- Grid decides each ask on your computer, by the same rules as at the race. The server
  only passes it on.
- It works for the event that is **open** in Grid, and only while Grid is connected to
  that event's club. When it is not, the pilot is told to try again or sign up at the race.

## Taking a seat from anywhere, at the race

In open practice a pilot can take an open seat from the server's page too, on their own
phone data. You do not need to run Wi-Fi for them.

Taking a seat gives a pilot the laps flown on it, so this is only for pilots who are at
the race. The **race code** is how Grid knows.

1. Open the event's **Pilot codes** page and find **Race code**. Press **Show**.
2. Show pilots the code, or press **Show the QR code** and let them scan it.
3. A pilot opens the event on the server, goes to **Me**, and takes a seat. The first time,
   the page asks for the race code. A pilot who scanned the QR code is not asked.

- There is one code for the whole event. It is not the same as a pilot's own sign-in code.
- Grid checks the code on your computer. The server never learns it.
- Press **New code** if the code got out. The old one stops working straight away. Pilots
  who have a seat keep it.
- After five wrong codes in a row, a pilot waits a minute.
- You can still drop anyone from a seat on the Run page, and put a pilot on a seat there
  yourself if their phone will not do it.

If the internet drops, keep racing. Grid sends what was missed when it is back.

## The list of events

Each event shows where it is sent and where it stands.

- **Not sent:** the event has no club. Pick one in its box.
- **Waiting:** the event's club is not the one Grid is talking to now. It is sent when an
  event of that club is open.
- **On the server:** all of it is there.
- **Sending:** some is still on its way.
- **Could not be sent:** the last try failed. Grid tries again by itself.
- **A different event is on the server:** see the next part.
- **Removed on the server:** see the part after that.

## A different event is on the server

This means the server already holds an event that should be this one, but its laps are not
the same. It can happen when you bring back an old copy of your data, or run the same event
from two computers.

Grid will not write over it by itself. Press **Replace the server's copy** if this
computer's copy is the right one. The server's copy is removed and yours is sent.

## The club is archived

Grid says **Not connected**, and that your club is archived on the server. An owner of the
server archived it. The server keeps what it has, and takes nothing new. Your own copy is
not touched.

Ask an owner of the server to bring the club back. Grid then carries on by itself.

## Removed on the server

Someone who runs your club removed this event on the server's page. Grid sends none of it
while it says this. Your own copy is not touched.

Press **Send it again** if it should be on the server after all.

A director can also **hide** an event on the server's page. A hidden event is left out of
the list there, but anyone who has its link can still open it. Grid keeps sending a hidden
event as usual.

## A server with its own certificate

Most servers need nothing extra. Some clubs and home servers make their own certificate.
Grid will not trust one until you say so.

Press **Add a club**, or **Change…** on a club you saved. Tick **The server uses its own
certificate**. Then there are two ways.

**Let Grid look.** Press **Look at the server's certificate**. Grid shows the
certificate's **fingerprint**, a long row of letters and numbers. Ask whoever runs the
server to read you theirs. If every letter and number is the same, press **Accept this
certificate**, and then **Save**. Grid then trusts that one certificate, for that server,
and no other.

Do not accept a fingerprint you have not checked. Someone between you and the server could
show you a certificate of their own.

Sometimes Grid says accepting would not be enough. Then the certificate was signed by the
club's own certificate authority, or the address is not the one the certificate was made
for. Use the second way.

**Paste it.** Paste the certificate, or the certificate authority, that the server's owner
gave you. It starts with `-----BEGIN CERTIFICATE-----`. Then press **Save**.

## Stopping

To stop sending one event, pick **Not sent** in its box. Nothing more of it is sent. What
is already on the server stays there.

To stop using a club, press **Remove…** next to it. Grid forgets the address and the link
token. Its events are not sent any more. What is already on the server stays there.
