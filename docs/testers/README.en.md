# Ondar, test build

Thanks for trying this. Ondar is a small radio player that lives in the menu bar: pick a country, pick a station, listen. It is a project by Martín Vieira, built during an apprenticeship at CrabNebula. This is the first build outside my own Mac, so your job is to use it like a normal app and tell me what is awkward, broken or missing.

## What you need

A Mac with Apple Silicon (M1 or later) running macOS 12 or newer. Intel Macs are not supported in this build.

## Install

1. Download Ondar from https://cdn.crabnebula.app/download/metambuy/ondar/latest/platform/dmg-aarch64.
2. Open the DMG and drag Ondar to Applications. Do this before the first launch; it matters for updates later.
3. Open Ondar from Applications. There is no Dock icon and no window: look for the Ondar icon in the menu bar at the top right of the screen and click it. macOS should not show a security warning; if it does, stop and tell me, that is a bug on my side.

## What works

Country list with search, station list, play and pause, volume, favourites and recents, the live track title when the station sends one, and an expanded view with a map of the selected country showing where stations are. Click a dot on the map to filter the list to that place. The play/pause media key and Control Centre work once you have played a station since opening Ondar.

## What is not there yet (no need to report these)

No equalizer controls or spectrum display. The next and previous media keys do nothing yet, and the volume keys change the Mac's volume, not Ondar's slider. If a Bluetooth speaker or headphones lose their connection, Ondar may keep playing through the Mac's speakers; that fix is next. No sleep timer. The menu bar icon does not animate while playing. Some streams will not play: HE-AAC v2 and Opus streams, and some stations whose stream is simply down. When a stream fails, the app says so; if it does not, tell me.

## What I want to hear

Anything that confused you, anything that broke, anything you expected and did not find. Three lines are enough: what you did, what you expected, what happened. A screenshot helps. Send it on the same channel I sent you this on.

## The log file

When something goes wrong, attach today's log: in Finder press Cmd+Shift+G and paste `~/Library/Logs/eu.ondar.radio/`. The file is named `ondar.<date>.log`. It contains station names and stream addresses and nothing else about you.

## Updates

When a new version is ready, a line appears at the bottom of the popover. On the version you are running now it is grey text, "Ondar 0.1.2 available · Update", and you click Update; from 0.1.2 on it is a blue button in the same place, "Update to" and the new version. Either way the app replaces itself and relaunches, and either way it only works if Ondar is in Applications (step 2).

## Privacy

What leaves your Mac:

- The radio streams you play. The station's server sees your IP address, as with any player.
- Requests to the station directory, radio-browser.info: country and station lists, and one "click" for each station you start, which the directory asks apps to send so it can count plays. Ondar finds a directory server through macOS's normal name lookup, as other apps do.
- One update check each time Ondar starts, to cdn.crabnebula.app (CrabNebula hosts Ondar's releases). It sends the version you run and your Mac's platform (macOS, Apple silicon).

No analytics, no crash reports, no account.

## To quit

Right-click (or Control-click) the Ondar icon in the menu bar and choose Quit Ondar; the popover itself has no quit control.

Martín
