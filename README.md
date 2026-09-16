# Omastorm

Live NEXRAD radar in your Omarchy bar. Beta.

<p align="center">
  <img src="docs/media/readme/hero.png" alt="Omastorm window and bar popover">
</p>

A radar that lives next to the clock. The popover is the station nearest you
and the actual scan time. Click the map (or press Enter) for the full window:
every dish in the network, the sweep at native resolution, a two-hour loop
you can play and scrub, drawn in your Omarchy theme.

This page is the user guide: install, first run, and everyday use. How the
code is built lives in [CONTRIBUTING.md](CONTRIBUTING.md).

## Install

Omarchy 4 on x86_64 and aarch64.

```sh
omarchy plugin add https://github.com/wesleygrimes/omastorm.git --enable
```

That clones the plugin, asks which bar section to use, and on first open
downloads the pinned engine from GitHub Releases, checks its sha256, and
installs it under `~/.local/share/omastorm/bin`. Configuration, remembered
view, and cached scans stay in Omastorm's own directories.

Optional: a Hyprland key to toggle the window. Add one line to
`~/.config/hypr/bindings.lua`. Omastorm never writes that file:

```lua
o.bind("SUPER + SHIFT + R", "Omastorm", "omarchy shell shell toggle com.omastorm.radar '{}'")
```

Optional: list it in the app launcher:

```sh
bash ~/.config/omarchy/plugins/com.omastorm.radar/scripts/write-desktop-entry.sh
```

## First run

Open the popover from the bar. If Omarchy weather already has a city, the
map opens there. If not, you get a choice: pick a place, or use an
approximate location.

![Choose your location](docs/media/readme/onboard.png)

**Use approximate location** is one click. It asks [wttr.in](https://wttr.in)
to guess your city from the public IP of that request. Nothing is sent until
you click; there is no GPS and no background tracking. A VPN or CGNAT may
land you at the ISP instead of your house. Choose manually if the guess is
wrong.

The view is remembered. Next time you open the popover, you are back where
you left off.

## What you are looking at

NEXRAD is NOAA's network of weather radars. Each dish spins, sends a pulse,
and measures how much bounced back. Omastorm shows **reflectivity** on the
lowest tilt: the beam that stays closest to the ground.

Color is **dBZ**, not a rain rate and not a warning. Stronger return, warmer
color. The legend under the map is that scale.

A bright blob is often rain or snow. The same beam also sees:

- insects, birds, and bats (especially on clear evenings)
- dust, smoke, and sea spray
- ground clutter and buildings near the dish
- anomalous propagation, when the beam bends and paints the ground far away
- wind farms, towers, and military chaff

Measured returns under 5 dBZ (the usual biological clutter and haze) are
hidden by default; the legend says so. Press `w` to show them.

This is not a forecast, and it is not the NWS warning stack. It is the sweep
that dish published, with the scan time on the stamp.

## The window

Drag to pan, scroll to zoom. The map and the radar are independent: panning
moves the camera; the dish is whichever station the map is following, unless
you lock it.

Click the station name for nearby dishes. The padlock pins that radar so
panning will not hand off; it turns yellow when the camera sits outside that
dish's rings. `n` picks the nearest radar and leaves the camera where it is.

The number under the product line is how stale the frame on screen is. The
stamp above the timeline is when that sweep was observed. **LIVE** is the
feed; the light beside it goes yellow when data is stale (ten minutes) and
red when the station or the bucket is unreachable. Cached frames stay.

A scale bar on the map is ground distance, in kilometres or miles from your
locale.

## Search

`/` (or `s`) is one field. Type a city, a site id, or paste coordinates.

A **city** centres the map there, unlocks, and selects the nearest radar.

![Search a city](docs/media/readme/search-city.png)

A **site** (`KTLX`, `tlx`) locks that dish and centres on it. Clicking the
station title opens the same card on the nearest dishes.

![Search a radar site](docs/media/readme/search-site.png)

**Coordinates** are latitude then longitude, decimal degrees, comma or space,
the way a maps link looks. Invalid range is named on the card; the layout
does not jump.

![Paste coordinates](docs/media/readme/search-coords.png)

![Coordinates out of range](docs/media/readme/search-error.png)

## My location

The map-marker at the top-left of the map, or `m`, jumps to the same
approximate IP location as first run. Zoom stays. The radar unlocks and
follows. Archived sessions never locate.

If the lookup fails, the camera stays put and a short overlay appears on the
map. It is not a new row of chrome.

![Approximate location failed](docs/media/readme/locate-fail.png)

## Air quality

A small chip beside **LIVE** shows the air quality where the map is centred:
an index in its category color, the scale always named, since the world's
AQI scales are not comparable. `a` turns the chip on and off for the
session; `~/.config/omastorm/config.toml` makes it a launch default. The
chip opens a card: the raw concentrations in µg/m³, the source, and how old
the reading is. Click it again to close.

- No token: the reading is [Open-Meteo](https://open-meteo.com)'s model
  analysis, global, no key. It shows the **US** (EPA) and **European** (EEA)
  indices as the source serves them, and computes the **Chinese** (HJ 633)
  and **Indian** (CPCB NAQI) indices from the raw concentrations.
- With a token from [WAQI](https://aqicn.org/data-platform/token/) in the
  `[aqi]` table, the reading is the nearest real ground station, and `d`
  shows its dots on the map: every station in view in the band palette, a
  click for its full breakdown. Station dots carry WAQI's own index — the
  China scale — and the card says so.

```
[aqi]
show = false
scale = "us"     # us | european | china | india | raw
token = ""       # a WAQI token; empty is Open-Meteo only
stations = false # the dot overlay; needs the token
```

## The loop

A station you arrive at fetches recent scans so there is something to play
within a few seconds. The cache then grows toward **60 frames / two hours**.
Older scans drop out. Space loops them; `[` `]` steps; Home and End jump.

NOAA publishes Level II via the [Open Data program on AWS](https://registry.opendata.aws/noaa-nexrad/).
A full volume takes about four to seven minutes (faster in severe weather,
slower in clear air). While a volume is in progress the engine reads live
chunks, so the sweep can paint as the antenna turns. If no new chunk arrives
for 90 seconds it rediscovers the latest volume; cached frames stay.

## Look

Three treatments sample the same gate and palette. They only change how each
3 px cell is painted. Glyphs is the default; `1` `2` `3` switch.

| Key | Treatment | Look |
| --- | --- | --- |
| `1` | Pixels | Solid blocks. The most literal picture of each gate. |
| `2` | Glyphs | A denser mark as the return strengthens. |
| `3` | Stipple | Soft squares that grow with intensity; more map shows through. |

![Pixels, Glyphs, and Stipple](docs/media/readme/treatments.png)

Chrome follows the Omarchy theme. Radar color comes only from the sweep.

<p align="center">
  <img src="docs/media/readme/themes.png" alt="Tokyo Night and Flexoki Light">
</p>

`?` lists every key. They are all rebindable.

| Key | Action |
| --- | --- |
| `h` `j` `k` `l` or arrows | Pan |
| `+` `-` | Zoom |
| `0` | Reset to the configured or weather location |
| `/` or `s` | Search |
| `n` | Nearest radar (camera stays) |
| `Shift+L` | Lock the station |
| `m` | My location |
| Space | Play / pause the loop |
| `[` `]` | Step a frame |
| Home / End | Oldest or newest frame |
| `1` `2` `3` | Pixels, Glyphs, Stipple |
| `w` | Show weak returns |
| `a` | Air quality chip off / on |
| `d` | Station dots off / on (needs a WAQI token) |
| `?` | This map |
| Esc | Close |

## Preferences

`~/.config/omastorm/config.toml` is what you mean to keep. The last map
center, zoom, and UI radar lock are saved separately in
`~/.local/state/omastorm/state.json`. Panning never rewrites config.

```toml
# Optional: always open here. Omit both to remember the last map position.
center_lat = 36.23708
center_lon = -79.97948
# locked_radar = "KFCX"  # optional; coordinates do not lock a radar

treatment = "GLYPHS"  # PIXELS, GLYPHS, or STIPPLE at launch
weak_floor = 5        # dBZ; false draws every measured return

[keys]
pan_left = "h Left"
zoom_in = "+ ="
```

A bad value is named in the status slot and that setting stays on its
default. Keys are Qt sequences; an empty string unbinds. The `1` `2` `3` and
`w` keys change treatment and the weak-return floor for the session without
writing the file.

## Update

```sh
omarchy plugin update com.omastorm.radar
omarchy restart shell
```

Until the restart, the shell keeps running what it loaded at login, old
engine pin included. Omastorm notices the new files and says so; clicking
that notice in the popover restarts the shell.

## If something is wrong

If expand or the keybind does nothing after an update, restart the shell.

The engine is one daemon per login. Its log is
`$XDG_RUNTIME_DIR/omastorm/engine.log` (usually `/run/user/<uid>/omastorm/`).
If the engine could not be installed, the reason is `bootstrap.log` in the
same directory; opening the popover again retries.

```sh
~/.local/share/omastorm/bin/omastorm-engine stop
```

The next popover or window starts it again.

This is a beta. Bugs, rough edges, and ideas go to
[GitHub issues](https://github.com/wesleygrimes/omastorm/issues). For a
failure, use the
[bug report template](https://github.com/wesleygrimes/omastorm/issues/new?template=bug-report.md)
and attach the last screenful of `engine.log` (and `bootstrap.log` if the
engine never installed). Include Omarchy version, plugin commit, and GPU as
the template asks.

## Remove

```sh
omarchy plugin remove com.omastorm.radar
~/.local/share/omastorm/bin/omastorm-engine stop
rm -rf ~/.local/share/omastorm ~/.cache/omastorm ~/.local/state/omastorm
rm -rf ~/.config/omastorm                            # keep this to reinstall later
rm -f ~/.local/share/applications/omastorm.desktop   # if you added the launcher entry
```

Then delete the `o.bind` line if you added one.

## Data

Radar: NOAA NEXRAD Level II via the NOAA Open Data program on AWS. Basemap: ©
OpenStreetMap contributors, [ODbL](https://opendatacommons.org/licenses/odbl/1-0/),
tiles by [OpenFreeMap](https://openfreemap.org); Natural Earth, public domain.
Location search: [GeoNames](https://www.geonames.org/),
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).
Approximate location: [wttr.in](https://wttr.in).
Air quality: [Open-Meteo](https://open-meteo.com), CC BY 4.0, built on the
Copernicus Atmosphere Monitoring Service (CAMS);
[WAQI](https://aqicn.org) station data under the
[data-platform terms](https://aqicn.org/data-platform/token/) with the
user's own token.
Code: MIT, see [LICENSE](LICENSE).

## Contributing

Setup, checks, and pull requests are in [CONTRIBUTING.md](CONTRIBUTING.md).
Read [DESIGN.md](DESIGN.md) before proposing a product change. Open work
that is ready for a first patch is labeled
[`good first issue`](https://github.com/wesleygrimes/omastorm/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22)
and [`help wanted`](https://github.com/wesleygrimes/omastorm/issues?q=is%3Aissue+is%3Aopen+label%3A%22help+wanted%22).

## Contributors

Thanks to these people
([emoji key](https://allcontributors.org/docs/en/emoji-key)):

<!-- ALL-CONTRIBUTORS-LIST:START - Do not remove or modify this section -->
<!-- prettier-ignore-start -->
<!-- markdownlint-disable -->
<table>
  <tbody>
    <tr>
      <td align="center" valign="top" width="14.28%"><a href="https://omastorm.com/"><img src="https://avatars.githubusercontent.com/u/324308?v=4?s=100" width="100px;" alt="Wes Grimes"/><br /><sub><b>Wes Grimes</b></sub></a><br /><a href="https://github.com/wesleygrimes/omastorm/commits?author=wesleygrimes" title="Code">💻</a> <a href="https://github.com/wesleygrimes/omastorm/commits?author=wesleygrimes" title="Documentation">📖</a> <a href="#maintenance-wesleygrimes" title="Maintenance">🚧</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/scottjones"><img src="https://avatars.githubusercontent.com/u/444693?v=4?s=100" width="100px;" alt="Scott Jones"/><br /><sub><b>Scott Jones</b></sub></a><br /><a href="https://github.com/wesleygrimes/omastorm/commits?author=scottjones" title="Code">💻</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/shieldsworks"><img src="https://avatars.githubusercontent.com/u/92273925?v=4?s=100" width="100px;" alt="Casey Shields"/><br /><sub><b>Casey Shields</b></sub></a><br /><a href="#infra-shieldsworks" title="Infrastructure (Hosting, Build-Tools, etc)">🚇</a> <a href="https://github.com/wesleygrimes/omastorm/commits?author=shieldsworks" title="Code">💻</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/airtwo"><img src="https://avatars.githubusercontent.com/u/3505656?v=4?s=100" width="100px;" alt="Chance Griffin"/><br /><sub><b>Chance Griffin</b></sub></a><br /><a href="https://github.com/wesleygrimes/omastorm/commits?author=airtwo" title="Code">💻</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/TheFifeDawg"><img src="https://avatars.githubusercontent.com/u/226545193?v=4?s=100" width="100px;" alt="Michael Pfeifer"/><br /><sub><b>Michael Pfeifer</b></sub></a><br /><a href="https://github.com/wesleygrimes/omastorm/commits?author=TheFifeDawg" title="Code">💻</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/nixfred"><img src="https://avatars.githubusercontent.com/u/15384894?v=4?s=100" width="100px;" alt="Fred Nix"/><br /><sub><b>Fred Nix</b></sub></a><br /><a href="https://github.com/wesleygrimes/omastorm/commits?author=nixfred" title="Code">💻</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/nmorton13"><img src="https://avatars.githubusercontent.com/u/16527730?v=4?s=100" width="100px;" alt="Nathan Morton"/><br /><sub><b>Nathan Morton</b></sub></a><br /><a href="https://github.com/wesleygrimes/omastorm/commits?author=nmorton13" title="Code">💻</a></td>
    </tr>
    <tr>
      <td align="center" valign="top" width="14.28%"><a href="http://mrcobas.com"><img src="https://avatars.githubusercontent.com/u/41881817?v=4?s=100" width="100px;" alt="javi"/><br /><sub><b>javi</b></sub></a><br /><a href="https://github.com/wesleygrimes/omastorm/commits?author=mrcobas" title="Tests">⚠️</a> <a href="https://github.com/wesleygrimes/omastorm/pulls?q=is%3Apr+reviewed-by%3Amrcobas" title="Reviewed Pull Requests">👀</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://ryrob.es/"><img src="https://avatars.githubusercontent.com/u/757387?v=4?s=100" width="100px;" alt="Ryan Robitaille"/><br /><sub><b>Ryan Robitaille</b></sub></a><br /><a href="https://github.com/wesleygrimes/omastorm/commits?author=ryrobes" title="Code">💻</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/gw7523"><img src="https://avatars.githubusercontent.com/u/199144018?v=4?s=100" width="100px;" alt="gw7523"/><br /><sub><b>gw7523</b></sub></a><br /><a href="https://github.com/wesleygrimes/omastorm/commits?author=gw7523" title="Code">💻</a> <a href="https://github.com/wesleygrimes/omastorm/issues?q=author%3Agw7523" title="Bug reports">🐛</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://github.com/Yani3rt"><img src="https://avatars.githubusercontent.com/u/170105839?v=4?s=100" width="100px;" alt="Yaniert Pascual"/><br /><sub><b>Yaniert Pascual</b></sub></a><br /><a href="#userTesting-Yani3rt" title="User Testing">📓</a></td>
      <td align="center" valign="top" width="14.28%"><a href="https://mikeyockey.com"><img src="https://avatars.githubusercontent.com/u/306343?v=4?s=100" width="100px;" alt="Michael Yockey"/><br /><sub><b>Michael Yockey</b></sub></a><br /><a href="https://github.com/wesleygrimes/omastorm/issues?q=author%3Ayock" title="Bug reports">🐛</a></td>
    </tr>
  </tbody>
</table>

<!-- markdownlint-restore -->
<!-- prettier-ignore-end -->

<!-- ALL-CONTRIBUTORS-LIST:END -->

This project follows the [all-contributors](https://allcontributors.org) specification.
