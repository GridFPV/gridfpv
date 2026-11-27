# Server

A Grid server is a computer that is always on. It keeps your events after the laptop is
shut, and lets people see them from anywhere.

You do not need one. Race day works the same with or without it.

The Server page is under **Setup**.

## What you need

- **The server's address.** It looks like `https://grid.example.org`.
- **A link token.** It starts with `gfs_`. Whoever runs the server makes one for you.

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

If the internet drops, keep racing. Grid sends what was missed when it is back.

## The list of events

Under the link, each event shows where it stands.

- **On the server:** all of it is there.
- **Sending:** some is still on its way.
- **Not sent:** the last try failed. Grid tries again by itself.
- **A different event is on the server:** see the next part.

## A different event is on the server

This means the server already holds an event that should be this one, but its laps are not
the same. It can happen when you bring back an old copy of your data, or run the same event
from two computers.

Grid will not write over it by itself. Press **Replace the server's copy** if this
computer's copy is the right one. The server's copy is removed and yours is sent.

## A server with its own certificate

Most servers need nothing extra. Some clubs and home servers make their own certificate.
Grid will not trust one until you say so.

Tick **The server uses its own certificate** and paste the certificate the server's owner
gave you. It starts with `-----BEGIN CERTIFICATE-----`.

## Turning it off

Flip the switch off. Nothing more is sent. What is already on the server stays there.
