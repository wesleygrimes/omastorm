# Omastorm

An Omarchy plugin that shows one NEXRAD station's lowest-cut Level II
reflectivity in the bar and in a window. Open source. Beta.

[![Omastorm playing an archived KTLX scan](https://github.com/wesleygrimes/omastorm/releases/download/v0.1.0/omastorm-preview.gif)](https://github.com/wesleygrimes/omastorm/releases/download/v0.1.0/omastorm-demo.mp4)

The demo above plays the archived KTLX scan of 2013-05-20 that the checks
use, not a live feed: playback, pan and zoom, the three treatments, weak
returns, station search, and the keys sheet. The stills below are live KTLX
with the actual scan time.

A radar that lives in your bar. The popover shows the station nearest your
location, the newest sweep, and whether that sweep is still arriving. Expand
it for the full window: the station's recent scans on a scrubbable timeline,
station and place search, all drawn in your Omarchy theme. One station at a
time, the lowest elevation cut, reflectivity only.

![The Omastorm window, live KTLX](https://github.com/wesleygrimes/omastorm/releases/download/v0.1.0/window-live.png)

![The Omastorm popover, live KTLX](https://github.com/wesleygrimes/omastorm/releases/download/v0.1.0/popover.png)

A headless Rust engine fetches and decodes NEXRAD Level II data and prepares
GPU-ready radar textures. An Omarchy plugin built with Quickshell/QML is the
client: it displays those textures in the bar popover and full window.

## What it does

- **One station, live.** The engine polls the public real-time Level II
  chunk feed for the selected station and the lowest cut paints as the
  antenna turns. When frames stop arriving, the badge stops saying LIVE.
- **Pick the station.** Pan the map and it follows the nearest station, lock
  one, or search by id, city, or state. The station table has 163 NEXRAD
  rows, including archived and test sites; a listing is not a promise that
  the station is on the air. Alaska, Hawaii, Puerto Rico, and the handful of
  overseas NEXRAD sites (Guam, Kadena, Kunsan, Camp Humphreys, Lajes) are in
  the table. TDWR and other countries' radars are not.
- **Timeline.** Up to 60 scans per station, cached locally. Play, step,
  scrub.
- **Three treatments.** Glyphs, Pixels, and Stipple sample the same gate
  and paint the 3 px cell differently.
- **Native.** Colors, font, and spacing come from the active Omarchy theme
  and change with it.
- **Keyboard first.** Everything the pointer reaches is a keystroke, and
  every key is rebindable.
- **Honest.** Actual scan times, in your local zone. Missing, range-folded,
  and below-threshold returns are drawn distinctly from measured values.
  Cached frames are called cached.

Not here: mosaics, neighbouring-station composites, velocity or other
moments, higher cuts. Reflectivity from one station's lowest cut is the
product.

## Install

Omarchy with the plugin system (`omarchy plugin add`; reports so far are
from Omarchy 4.0), x86_64 only: the pinned engine binary is built for that
architecture (aarch64 is [#4](https://github.com/wesleygrimes/omastorm/issues/4)).

```sh
omarchy plugin add https://github.com/wesleygrimes/omastorm --enable
```

This clones the plugin into `~/.config/omarchy/plugins/com.omastorm.radar`
and asks which bar section to use. As soon as the bar widget loads it
downloads the pinned engine binary from this repository's GitHub Releases,
verifies its sha256 against `engine/release.pin`, installs it under
`~/.local/share/omastorm/bin`, and starts it; until then the popover says
"Starting radar engine…". Runtime files, cached data, remembered view state,
and configuration stay inside Omastorm's own directories.

On first use, Omastorm uses your Omarchy weather location when available;
otherwise it prompts you to search for a place or enter coordinates. To set
a fixed launch location, including during agent-assisted installation, see
[configuration and remembered state](docs/configuration.md).

To open the window from the keyboard, add one line to
`~/.config/hypr/bindings.lua` (the file Omarchy's Lua keybindings live in;
if your Omarchy keeps them elsewhere, put the line there). Omastorm never
writes that file.

```lua
o.bind("SUPER + SHIFT + R", "Omastorm", "omarchy shell shell toggle com.omastorm.radar '{}'")
```

To list Omastorm in the app launcher:

```sh
bash ~/.config/omarchy/plugins/com.omastorm.radar/scripts/install-launcher.sh
```

Update with `omarchy plugin update com.omastorm.radar`.

## Use

Click the mark in the bar for the popover: the map at your location, the
station id, the feed condition beside it, the newest sweep's time, step and
play, and EXPAND. If no location is known, the popover offers "Choose a
location", which opens the picker in the window. Click the radar or press
Enter for the window; it opens on the same station, frame, and camera.
Closing preserves your view for the next launch.

The mark itself says how the feed is doing: full strength while sweeps are
arriving; dim with no dot while a station's first sweep loads; a little
dimmer with a yellow dot when the newest sweep is ten minutes old or more;
dim with a solid dot in the theme's urgent color when the station has gone
quiet or the feed cannot be reached; faint with a hollow dot while no engine
is running. A screen reader gets the station and the same condition.

In the window, drag to pan and scroll to zoom. The map follows the nearest
station as you pan unless you lock it; a locked radar stays put even when
the camera leaves its coverage, and the chip after the station name says
OUTSIDE COVERAGE whenever the view centre is beyond the active station's
dashed ring, locked or not. That ring is the nominal 460 km reflectivity
footprint, the same for every station; it is not measured range. A station
you arrive at fetches its last dozen scans, so there is a loop to play within
a few seconds; the cache then grows to 60 as new scans arrive.

Under the map: SEARCH (stations), the lock, LOCATION (the place picker),
the treatment chip, zoom, and RESET. Drag along the timeline to scrub;
`?` opens the keys sheet.

### Live, stale, unavailable, offline

The engine judges the feed and every surface repeats its judgement; nothing
in the UI upgrades it. The window's header badge, the popover's headline,
and the bar mark all follow the same condition:

| Condition | Meaning | What you see |
| --- | --- | --- |
| LIVE | The newest sweep for the station is under ten minutes old. | The badge says LIVE; the timeline's last tick says "now". |
| STALE | The feed is reachable but the newest sweep is ten minutes old or more. | STALE · LAST SWEEP and the time, in yellow. Cached frames stay. |
| UNAVAILABLE | The feed answered, but has nothing for the station, or nothing new for thirty minutes: maintenance, an outage, or a stalled poller ([#7](https://github.com/wesleygrimes/omastorm/issues/7)). | The window says UNAVAILABLE; the popover says NOT UPDATING · LAST SWEEP and the time, and under the map names the condition and the restart. Cached frames stay, dimmed. |
| OFFLINE | The engine could not reach the feed (twice in a row), or, in the popover and bar, no engine is running. | The window says OFFLINE · SHOWING CACHED; the popover says NOT UPDATING · LAST SWEEP. Cached frames stay, dimmed. |
| LOADING | A station was just selected and its first sweep has not arrived. | The map without radar. No spinner. |

The popover never offers a retry control, because none exists in the
engine: recovery is restarting the daemon (below). A GPU driver that refuses
the radar shader shows "Radar overlay failed to draw" in the popover
instead of an empty map under a LIVE badge; the window shows the driver's
message.

| Key | Action |
| --- | --- |
| `h` `j` `k` `l` or arrows | Pan |
| `+` or `=`, `-` | Zoom in, zoom out |
| `0` | Reset to the configured or weather location |
| `/` or `s` | Search sites (center and lock) |
| `n` | Nearest site |
| `Shift+L` | Lock the station |
| `Shift+H` | Choose a location |
| `Space` | Loop the frames |
| `[` `]` | Step a frame |
| `Home` `End` | Oldest or newest frame |
| `1` `2` `3` | Pixels, Glyphs, Stipple |
| `w` | Show weak returns |
| `?` | Keys sheet |
| `Esc` | Close |

Measured returns under 5 dBZ (insects, birds, ground clutter on a clear day)
are hidden by default and the legend says so; `w` shows them. The popover
carries the same legend in one line.

Place search covers GeoNames towns of 5,000 or more people inside the
network's envelope (roughly 5–75° N and west of 20° W or east of 120° E). Outside
it the picker says so; latitude and longitude work anywhere.

## Configuration

`~/.config/omastorm/config.toml` holds deliberate preferences. The app saves
last map center, zoom, and UI radar lock separately in
`$XDG_STATE_HOME/omastorm/state.json` (default
`~/.local/state/omastorm/state.json`). Navigation never rewrites your config.
`Shift+H`, or LOCATION, opens the location picker; it writes state, not config.

Explicit center coordinates win on every launch. Without them, Omastorm
restores your last view, then falls back to the weather location or location
picker. Radar selection is independent: a configured lock wins, otherwise a
remembered lock is restored, otherwise the nearest radar follows the map.

```toml
# Optional: always open here. Omit both to remember the last map position.
center_lat = 36.23708
center_lon = -79.97948
# locked_radar = "KFCX" # optional radar override; coordinates do not imply a lock

treatment = "GLYPHS" # PIXELS, GLYPHS, or STIPPLE at launch
weak_floor = 5       # dBZ; false draws every measured return

[keys]
pan_left = "h Left"
zoom_in = "+ ="
```

A bad value is named in the status slot and that setting stays on its
default; the feed condition stays visible beside it. Every action name, the
key syntax, and what each setting does are in
[docs/configuration.md](docs/configuration.md).

## Troubleshooting

If expand or the keybind does nothing after `omarchy plugin update`, the
shell still has the previous QML types. Restart it:

```sh
omarchy restart shell
```

The engine runs as one shared daemon per login. Its log is
`$XDG_RUNTIME_DIR/omastorm/engine.log` (usually `/run/user/<uid>/omastorm/`).
If the popover says the engine could not be installed, the download or its
sha256 check failed; the reason is in `bootstrap.log` in the same directory,
and the bar retries the install every 20 seconds while no engine answers.

A station that sits on UNAVAILABLE or OFFLINE while the feed is up is a
stalled poller ([#7](https://github.com/wesleygrimes/omastorm/issues/7),
[#19](https://github.com/wesleygrimes/omastorm/issues/19),
[#20](https://github.com/wesleygrimes/omastorm/issues/20)); the last lines of
`engine.log` name the chunk it was waiting for. There is no restart command
for the poller. Stop the engine:

```sh
~/.local/share/omastorm/bin/omastorm-engine stop
```

The bar widget starts it again within 20 seconds, and the next window launch
from a checkout does so at once. Please attach both logs to a
[bug report](https://github.com/wesleygrimes/omastorm/issues).

## Remove

```sh
omarchy plugin remove com.omastorm.radar
~/.local/share/omastorm/bin/omastorm-engine stop
rm -rf ~/.local/share/omastorm ~/.cache/omastorm ~/.local/state/omastorm
rm -rf ~/.config/omastorm                            # your config.toml; keep it to reinstall later
rm -f ~/.local/share/applications/omastorm.desktop   # if you added the launcher entry
```

Then delete the `o.bind` line if you added one.

## Feedback

This is a beta. Bugs, rough edges, and ideas go to
[GitHub issues](https://github.com/wesleygrimes/omastorm/issues).

## Data and licenses

Radar: NOAA NEXRAD Level II. Live sweeps come from Unidata's public
real-time chunk bucket on AWS (`unidata-nexrad-level2-chunks`, part of the
[NOAA Open Data](https://registry.opendata.aws/noaa-nexrad/) program); no
credentials are needed and none are stored. Basemap: ©
OpenStreetMap contributors, [ODbL](https://opendatacommons.org/licenses/odbl/1-0/),
tiles by [OpenFreeMap](https://openfreemap.org); Natural Earth, public domain.
Location search: [GeoNames](https://www.geonames.org/),
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).
Code: MIT, see [LICENSE](LICENSE).

## Contributing

Start with [CONTRIBUTING.md](CONTRIBUTING.md) for local setup, checks, and the
pull request workflow. [DESIGN.md](DESIGN.md) defines the app's visual and
interaction rules; the [engine guide](engine/README.md) and
[wire protocol](docs/protocol.md) explain the backend/client boundary.
Maintainers can follow the [release guide](docs/RELEASING.md).
