# Ondar 0.1.0, test build

Thanks for trying this. Ondar is a small radio player that lives in the menu bar: pick a country, pick a station, listen. It is a project by Martín Vieira, built during an apprenticeship at CrabNebula. This is the first build outside my own Mac, so your job is to use it like a normal app and tell me what is awkward, broken or missing.

## What you need

A Mac with Apple Silicon (M1 or later) running macOS 12 or newer. Intel Macs are not supported in this build.

## Install

1. Download Ondar from [DOWNLOAD LINK, filled at release].
2. Open the DMG and drag Ondar to Applications. Do this before the first launch; it matters for updates later.
3. Open Ondar from Applications. There is no Dock icon and no window: look for the Ondar icon in the menu bar at the top right of the screen and click it. macOS should not show a security warning; if it does, stop and tell me, that is a bug on my side.

## What works

Country list with search, station list, play and pause, volume, favourites and recents, the live track title when the station sends one, and an expanded view with a map of the selected country showing where stations are. Click a dot on the map to filter the list to that place.

## What is not there yet (no need to report these)

No equalizer controls or spectrum display. No media keys or Now Playing integration. No sleep timer. The menu bar icon does not animate while playing. Some streams will not play: HE-AAC v2 and Opus streams, and some stations whose stream is simply down. When a stream fails, the app says so; if it does not, tell me.

## What I want to hear

Anything that confused you, anything that broke, anything you expected and did not find. Three lines are enough: what you did, what you expected, what happened. A screenshot helps. Send it on the same channel I sent you this on.

## The log file

When something goes wrong, attach today's log: in Finder press Cmd+Shift+G and paste `~/Library/Logs/eu.ondar.radio/`. The file is named `ondar.<date>.log`. It contains station names and stream addresses and nothing else about you.

## Updates

When a new version is ready, a line appears at the bottom of the popover: "Ondar 0.1.1 available · Update". Click Update; the app replaces itself and relaunches. This only works if Ondar is in Applications (step 2).

## Privacy

Nothing leaves your Mac except the radio streams and requests to the station directory (radio-browser.info). No analytics, no crash reports, no account.

## To quit

Right-click (or Control-click) the Ondar icon in the menu bar and choose Quit Ondar; the popover itself has no quit control.

Martín
