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

## Linking Grid to a server

1. Type the address.
2. Paste the link token.
3. Press **Save**.
4. Flip the switch on.

The badge says **Connected to** and the name of your club when it works.

Grid never shows the token again. To change it, type a new one over it.

## What is sent

Every event is sent, and kept up to date as you race. A lap is on the server about a
second after it is flown.

- The race itself goes up whole: every lap and every ruling.
- Pilots go up by callsign and colour only. Real names stay on your computer.
- An event's name, date, place, description and organizer go up.
- Your pilot list goes up too, by callsign and colour, so the server knows a pilot before
  their first race.

## Pilots and their accounts

People in your club can have an account on the server. When a pilot is tied to their
account, they see the events they flew and where they placed.

You tie them together on the [Pilots](#/help/pilots) page, with **Account**. Or you bring
pilots in from the server with **Add from the server**.

## Signing up from anywhere

A pilot who is tied to their account can sign up for a heat on the server's page, from
home or on the way to the field. They open the event there and use the **Sign up** tab.
It is the same sheet they get on their phone at the race.

- Sign-up must be on for the phase, and open. You set that on the
  [Phases](#/help/phases) page, as always.
- Grid decides each ask on your computer, by the same rules as at the race. The server
  only passes it on.
- It works for the event that is **open** in Grid, and only while Grid is connected to the
  server. When it is not, the pilot is told to try again or sign up at the race.

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
- You can still drop anyone from a seat on the Run page.

If the internet drops, keep racing. Grid sends what was missed when it is back.

## The list of events

Under the link, each event shows where it stands.

- **On the server:** all of it is there.
- **Sending:** some is still on its way.
- **Not sent:** the last try failed. Grid tries again by itself.
- **A different event is on the server:** see the next part.
- **Removed on the server:** see the part after that.

## A different event is on the server

This means the server already holds an event that should be this one, but its laps are not
the same. It can happen when you bring back an old copy of your data, or run the same event
from two computers.

Grid will not write over it by itself. Press **Replace the server's copy** if this
computer's copy is the right one. The server's copy is removed and yours is sent.

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

Tick **The server uses its own certificate**. Then there are two ways.

**Let Grid look.** Press **Look at the server's certificate**. Grid shows the
certificate's **fingerprint**, a long row of letters and numbers. Ask whoever runs the
server to read you theirs. If every letter and number is the same, press **Accept this
certificate**. Grid then trusts that one certificate, for that server, and no other.

Do not accept a fingerprint you have not checked. Someone between you and the server could
show you a certificate of their own.

Sometimes Grid says accepting would not be enough. Then the certificate was signed by the
club's own certificate authority, or the address is not the one the certificate was made
for. Use the second way.

**Paste it.** Paste the certificate, or the certificate authority, that the server's owner
gave you. It starts with `-----BEGIN CERTIFICATE-----`.

## Turning it off

Flip the switch off. Nothing more is sent. What is already on the server stays there.
