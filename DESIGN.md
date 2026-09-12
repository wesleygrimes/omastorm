# Omastorm design

How a change, feature, or fix should behave.

[README.md](README.md) is install and use. [docs/protocol.md](docs/protocol.md)
is the wire. [docs/configuration.md](docs/configuration.md) is the config keys.
[CONTRIBUTING.md](CONTRIBUTING.md) is the contribution workflow. Honor these; ask before
violating them.

## Picture

Weather occupies the view. Geography stays quiet: thin lines, sparse labels,
rings, a crosshair. Chrome follows the Omarchy theme; radar color comes only
from `frame.palette`, shared by the legend and the shader.

Pixels, Glyphs, and Stipple all stay. They sample the same cell and palette
and differ only in how the 3 px cell is painted. Glyphs is the default. A
shader or sampling change updates all three and is shown with a capture, not
described.

Show actual scan times. Label archived data. Missing, range-folded, and
below-threshold stay distinct from measured values. Whatever a change hides
is named in the legend. Keep OSM (ODbL) and Natural Earth attribution with
the data and on screen.

## Window chrome

Use these names when discussing or changing the expanded window. They are
the ids and comments in `ui/RadarWindow.qml`.

| Name | What it is |
|---|---|
| **brand row** | Mark, OMASTORM, status light, LIVE / ARCHIVED |
| **site row** | Station title, radar lock (yellow when the camera is outside that radar's rings) |
| **product stack** | Right column: product line + meta line |
| **product line** | Product name / tilt and NOAA NEXRAD |
| **meta line** | Age, right-aligned under the product line |
| **map stage** | Radar map frame |
| **follow chip** | Crosshair on the map (place follow); hidden until GPS is wired |
| **help chip** | Keys / `?` on the map |
| **scale bar** | Ground distance under the map, left; locale picks km or mi; label updates with zoom |
| **legend** | dBZ scale directly under the map |
| **transport** | Playback buttons |
| **tick strip** | Frame ticks on the timeline |
| **break marker** | Dashed yellow mark between two frames a hole in the scans separates |
| **break note** | Card over a hovered or focused break marker: the bounding times and the length of the hole |
| **strip stamp** | Date / time / zone above the tick strip |
| **break notice** | `Skipped 26h · no scans available` beside the stamp after the playhead crosses a hole |
| **frame index** | `N / total` above the strip, right-aligned; counts available frames only |

**Bottom chrome order.** Map stage, then legend, then transport + tick
strip, with the strip stamp left-aligned and frame index right-aligned
on one row above the ticks. Playback buttons align with the track at the bottom.

**Product stack.** Compact product line (name, then NOAA NEXRAD). The meta
line is the age only, right-aligned under that row.

**Time.** Age on the meta line is how stale the frame on screen is. The
strip stamp is the absolute observation time (date, time, zone). Locale
picks date order and 12/24h only; dates stay numeric. Locale also picks
kilometres or miles for the scale bar and picker distances. The tick strip is
position in the loop, not a second clock. It has 60 positions when the
window is wide enough; compact widths show one tick per available frame
only (empty pads need room or they read as a dotted cliff). An extra live
sweep beyond 60 completed scans adds a selectable tick and is included in
the frame count. Available frames fill from the left; unused positions are
faint, short, and cannot be sought. Each available tick represents one
frame, with no baseline and no proportional spacing.

**Breaks.** Neighbouring ticks are equal steps, so a hole in the catalog
(an outage, sparse history) would otherwise read as even time. A break
marker sits between two frames whose interval is longer than thirty
minutes (the silence that shows UNAVAILABLE while live) and longer than
three usual intervals, the usual interval being the median of those under
thirty minutes. Trailing pads are room to fill, never missing time. On the
catalogs of nineteen stations (September 2026) ordinary cadence ran 3.3 to
8.8 minutes, a VCP change moved one station from 4.5 to 7.1 minutes inside
one history, and single missed volumes left 17 to 18 minute intervals; none
of those is a break. The holes were 35 minutes to 26 hours, and all are.
The marker is a position in the strip, not a frame: it is not counted, not
sought by a scrub, and not a stop for stepping. Hovering or focusing it
(Tab) shows the break note: `No scans available between these times`, the
two stamps, and the length. When the frame on screen moves across a hole by
a step, a scrub, or playback, the break notice names the jump beside the
stamp for four seconds: `Skipped 26h · no scans available`, `Back 26h · no
scans available`, shortened to `Skipped 26h` where the row is narrow. The
loop wrapping to the oldest frame, the oldest and newest keys, and a new
sweep arriving after a silence are not crossings and show no notice.
Wording stays neutral: the scans are unavailable, and the app does not say
why. The popover strip carries the same markers and note and the short
notice.

## Location, onboarding, and map

Map center and radar source are independent. The center is the place the
user wants to see; the radar supplies one station's exact sweep. Loading a
frame or handing off to another station never moves the camera.

After the plugin ensures the engine is available and starts it, resolve the
map center in this order:

1. Explicit `center_lat` and `center_lon` in `config.toml`.
2. The last center remembered in `state.json`.
3. Valid coordinates from Omarchy's weather location (`weather.json`).
4. The location prompt: use approximate location or choose manually.

When the first three sources have no valid center, show two actions in the
existing prompt, with no additional dialog. "Use approximate location" runs
one bounded `curl` to `wttr.in/?format=j2` only on click (UI-side; not an
engine command); the provider and public-IP use are documented, and a
successful view is labeled `IP NEAR …`. "Choose
manually" opens the existing search and coordinates.
Remember a successful estimate like any chosen view. While it is pending,
manual selection remains available and takes priority over a late reply.
Failures show a short error and allow an explicit retry. There is no IP
configuration knob; a click is the opt-in. IP never enables GPS or tracking.
Offer place search and "Enter coordinates", which reveals
labeled latitude and longitude fields with validation. Place search is an
engine `search_places` reply over GeoNames cities with population ≥ 5000
in the network envelope (state/region and country so two Jacksonvilles are
distinct); map labels stay Natural Earth. "Show radar" accepts the location.
No separate setup wizard or settings window is required. Keep the picker
reachable after onboarding (`Shift+H` and LOCATION). Coordinate entry
chooses a view; it does not create a permanent config override or lock a
radar. Choosing a location writes `state.json`, never `config.toml`.

Reuse Omarchy's location when available without requiring its weather plugin.
Read weather settings only; never write them. Location search is an explicit
user action handled through the engine. Approximate IP lookup is an explicit
UI action via wttr.in (`format=j2`, smaller than Omarchy weather's `j1`); the
launcher and engine perform no IP lookup. Do not use GeoClue. Archived views
never locate, and checks require the same explicit action as users.
Resolve the radar separately: an explicit `locked_radar` in config wins,
otherwise restore a remembered radar lock, otherwise choose the station
nearest the map center. A radar lock alone does not supply a map center or
bypass location onboarding. Newly chosen locations start unlocked unless a
configured radar override applies.

Remember center and zoom after movement settles, and remember changes to the
UI radar lock. Unlocked radar selection follows the center using the protocol's
nearest-station hysteresis; do not wait until the center leaves the radar's
rings. Lock pins the source; `n` releases it and selects the nearest station
without moving the camera. Choosing a station in search locks it and centres
the map on that site. Automatic hand-off and loading a frame never move the
camera. Do not persist the automatically selected station.

Closing preserves the view. Reopening restores it, with explicit config
values taking precedence. Expanding the popover preserves its center, zoom,
station, frame, and playback. An engine reconnect restores the necessary
commands without resetting the user's camera. Weather location supplies an
initial view; subsequent weather changes do not overwrite a remembered view.

Explicit coordinates are honored on every launch and do not imply a radar
lock. A configured center far from a locked radar is valid: preserve both,
show the station and lock clearly (yellow lock when coverage is outside the
view), and offer "Use nearest radar" and "Go to selected radar" when that
radar's coverage is outside the view. UI navigation
and unlocking can change the active session; explicit config applies again
on launch.

A station with no frame yet is the map without radar. Show no loading animation.
Display one radar station’s sweep at a time.

## Split

The engine fetches, decodes, caches, and rasterizes. The UI is small state
plus GPU textures. Radar values do not enter JSON or QML. Pan and zoom are
uniforms. The engine reads neither `config.toml` nor `state.json`; the UI
resolves preferences and remembered state, then sends commands.

New settings are optional, omit means default, and a bad value is named in
the status slot. Keep deliberate settings in `config.toml` and session restore in `state.json`.
The app never rewrites config because the user pans, zooms, or changes a lock.
Write `state.json` atomically. See [configuration](docs/configuration.md) for
file ownership and precedence. Do not write Omarchy, Hyprland, or system
configuration.

A product is a texture, legend, units, timestamp, and source from the engine.
Level II is what is drawn.

The live poller follows the latest volume. `try_next` returning no chunk is
normal between chunks, but 90 seconds with no chunk at all means the
iterator is stuck on a volume that will never grow; restart discovery.
Independently, if the poller task has exited, or the newest radial is thirty
minutes old and discovery has not been tried since, spawn a new poller.
Reselecting the current station is a no-op while the poller is running; if
the task has ended, start it again. Cached frames stay on screen through a
rediscovery. A rediscovery that finds only a sweep already in the catalog
leaves the frame and connection chrome alone; a newer volume still clears
UNAVAILABLE / OFFLINE.

## Scope

Keep the feature set small. Prefer the weather panel, the theme, and the
engine's state over a parallel mechanism in this app. If a visual call is
open, change the running picture and look at it.
