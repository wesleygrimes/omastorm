# Omastorm

Live NEXRAD radar for the Omarchy desktop. Beta.

[![Omastorm, one live take on the Jacksonville radar](https://github.com/wesleygrimes/omastorm/releases/download/v0.1.0/omastorm-preview.gif)](https://github.com/wesleygrimes/omastorm/releases/download/v0.1.0/omastorm-demo.mp4)

One live take, 2026-09-07, on KJAX: the loop, pan and zoom, the three
treatments, weak returns, the picker, and the keys.

A radar that lives in your bar. The popover shows the station nearest you with
the actual scan time. Expand it for the full window: every NEXRAD site in the
network, reflectivity at native resolution, a timeline you can scrub, all drawn
in your Omarchy theme.

![The Omastorm window, live](https://github.com/wesleygrimes/omastorm/releases/download/v0.1.0/window-live.png)

![The Omastorm popover, live](https://github.com/wesleygrimes/omastorm/releases/download/v0.1.0/popover.png)

## Features

- **Live.** A Rust engine polls NOAA's public Level II feed and sweeps paint as
  the antenna turns. Stale data says it is stale.
- **Every site.** Pan the map and it follows the nearest station, or search by
  id, city, or state.
- **Timeline.** The last 60 scans per station, cached locally. Play, step, scrub.
- **Three treatments.** Glyphs, Pixels, and Stipple sample the same gate and
  paint the cell differently.
- **Native.** Colors, font, and spacing come from the active Omarchy theme and
  change with it.
- **Keyboard first.** Everything the pointer reaches is a keystroke, and every
  key is rebindable.
- **Honest.** Actual scan times. Missing, range-folded, and below-threshold
  returns are drawn distinctly from measured values. No forecasts, no mosaic.

## Install

Omarchy 4 on x86_64.

ARM64 (`aarch64`) can run a native source build (see below). Prebuilt ARM64
installation requires a published engine asset pinned in
`engine/release-aarch64.pin`; until that pin ships, the installer explains
how to build locally instead of trying to run an x86_64 binary.

```sh
omarchy plugin add https://github.com/wesleygrimes/omastorm --enable
```

This clones the plugin into `~/.config/omarchy/plugins/com.omastorm.radar` and
asks which bar section to use. The first time the popover opens it downloads
the pinned engine binary from this repository's GitHub Releases, verifies its
sha256 against `engine/release.pin`, and installs it under
`~/.local/share/omastorm/bin`. Nothing else is written outside the plugin's
own cache and config directories.

To open the window from the keyboard, add one line to
`~/.config/hypr/bindings.lua`. Omastorm never writes that file.

```lua
o.bind("SUPER + SHIFT + R", "Omastorm", "omarchy shell shell toggle com.omastorm.radar '{}'")
```

To list Omastorm in the app launcher:

```sh
bash ~/.config/omarchy/plugins/com.omastorm.radar/scripts/install-launcher.sh
```

Update with `omarchy plugin update com.omastorm.radar`.

### ARM64 source installation

Install the plugin as above, then, with Rust 1.89+ and a C compiler available:

```sh
cd ~/.config/omarchy/plugins/com.omastorm.radar
bash scripts/setup-fixture.sh
bash scripts/cargo.sh build --locked
bash run.sh --ensure
```

The plugin automatically uses this native engine on subsequent launches.
Repeat the build after updating the plugin to pick up engine changes.

## Use

Click the mark in the bar for the popover: the home station, LIVE or the
connection condition, the actual scan time, step and play, and EXPAND. Click
the radar or press Enter for the window; it opens on the same station and
frame. Closing the window returns to home.

In the window, drag to pan and scroll to zoom. The map follows the nearest
station as you pan unless you lock it. A station you arrive at fetches its last
dozen scans, so there is a loop to play within a few seconds. The status slot shows the age of the
frame on screen: LIVE, STALE after ten minutes, UNAVAILABLE or OFFLINE when the
feed cannot be reached, with cached frames kept.

| Key | Action |
| --- | --- |
| `h` `j` `k` `l` or arrows | Pan |
| `+` `-` | Zoom |
| `0` | Home view |
| `/` or `s` | Search sites |
| `n` | Nearest site |
| `Shift+L` | Lock the station |
| `Shift+H` | Save the station as home |
| `Space` | Loop the frames |
| `[` `]` | Step a frame |
| `Home` `End` | Oldest or newest frame |
| `1` `2` `3` | Pixels, Glyphs, Stipple |
| `w` | Show weak returns |
| `?` | Keys sheet |
| `Esc` | Close |

Measured returns under 5 dBZ (insects, birds, ground clutter on a clear day)
are hidden by default and the legend says so; `w` shows them.

## Configuration

`~/.config/omastorm/config.toml` is optional. Without `home_site` the home is
the station nearest Omarchy's weather location. `Shift+H`, or the HOME
button, saves the station on screen as `home_site`.

```toml
home_site = "KTLX"   # a station id; omit to use Omarchy's weather location
follow = true        # follow the nearest station while panning
treatment = "GLYPHS" # PIXELS, GLYPHS, or STIPPLE at launch
weak_floor = 5       # dBZ; false draws every measured return

[keys]
pan_left = "h Left"
zoom_in = "+ ="
```

A bad value is named in the status slot and that setting stays on its default.
The action names and key syntax are in [docs/protocol.md](docs/protocol.md).

## Remove

```sh
omarchy plugin remove com.omastorm.radar
~/.local/share/omastorm/bin/omastorm-engine stop
rm -rf ~/.local/share/omastorm ~/.cache/omastorm
rm -f ~/.local/share/applications/omastorm.desktop   # if you added the launcher entry
```

Then delete the `o.bind` line if you added one.

## Feedback

This is a beta. Bugs, rough edges, and ideas go to
[GitHub issues](https://github.com/wesleygrimes/omastorm/issues).

## Data and licenses

Radar: NOAA NEXRAD Level II via the NOAA Open Data program on AWS. Basemap: ©
OpenStreetMap contributors, [ODbL](https://opendatacommons.org/licenses/odbl/1-0/),
tiles by [OpenFreeMap](https://openfreemap.org); Natural Earth, public domain.
Code: MIT, see [LICENSE](LICENSE).

## Developing

A checkout needs Rust 1.89+ and Quickshell with OpenGL. See
[AGENTS.md](AGENTS.md), [engine/README.md](engine/README.md), and
[data/README.md](data/README.md).

```sh
bash scripts/setup-fixture.sh
bash scripts/cargo.sh build --locked
bash run.sh
```
