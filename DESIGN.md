# Omastorm design

How a change, feature, or fix should behave.

[README.md](README.md) is the user guide. [docs/README.md](docs/README.md)
indexes the rest of this tree. [docs/protocol.md](docs/protocol.md) is the
wire. [docs/configuration.md](docs/configuration.md) is the config contract.
[docs/radar-fetch.md](docs/radar-fetch.md) is how to get live and archived
Level II bytes. [docs/grid-adapters.md](docs/grid-adapters.md) is how
international grid mosaics join the live picture.
[CONTRIBUTING.md](CONTRIBUTING.md) is the contribution
workflow. Honor these; ask before violating them.

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
| **site row** | Station title, radar lock (yellow when the camera is outside that source's coverage, not its rings) |
| **product stack** | Right column: product line + meta line |
| **product line** | Product name / tilt and the active source's attribution |
| **meta line** | Age, right-aligned under the product line |
| **map stage** | Radar map frame |
| **locate chip** | Map marker, top-left of the map; jump to the approximate location |
| **follow chip** | GPS crosshair beside it while `gpsd = true`; pause / resume follow, `NO FIX` when the receiver is silent |
| **help chip** | Keys / `?` on the map |
| **metar chips** | Optional. ICAO labels with FAA flight-category color in place of city names, around the selected live NEXRAD radar (US and Canada; OPERA Europe is a no-op). Off until toggled. Default is the nearest stations (at most 16). Optional AWC-priority pick uses the current map view so hubs outrank closer small fields; `count` shrinks the pool; `always_on_when_in_view` pins a home field that is on screen. A chip at the selected radar (KLIT next to KLZK) sits beside the site tag. Optional `mark`: filled category block (`chip`), ICAO letters in category color (`ink`), or a larger category-colored location (`pin`). Click shows the raw METAR on a **metar card** over the map, 80% width, bottom-right, above the OSM credit, so the scale bar stays clear. |
| **scale bar** | Ground distance under the map, left; locale picks km or mi; label updates with zoom |
| **legend** | dBZ scale directly under the map |
| **transport** | Playback buttons |
| **tick strip** | Frame ticks on the timeline |
| **strip stamp** | Date / time / zone above the tick strip |
| **frame index** | `N / total` above the strip, right-aligned; counts available frames only |

**Bottom chrome order.** Map stage, then legend, then transport + tick
strip, with the strip stamp left-aligned and frame index right-aligned
on one row above the ticks. Playback buttons align with the track at the bottom.

**Product stack.** Compact product line (name, then the active source's
attribution). The meta
line is the age only, right-aligned under that row.

**Time.** Age on the meta line is how stale the frame on screen is. The
strip stamp is the absolute observation time (date, time, zone). Locale
picks date order and 12/24h only; dates stay numeric. Locale also picks
kilometres or miles for the scale bar and picker distances. The tick strip is
position in the loop, not a second clock. One tick per timeline entry,
spread across the strip, at every width; no empty pads. NEXRAD retains the
last two hours of completed scans, 60 at most; older frames leave the
catalog, so a station watched yesterday and again tonight loops tonight
only. Its sweep in progress is an outlined tick after the complete frames
and is included in the frame count. Mosaic history follows the adapter
limit; OPERA retains up to 12 complete frames. Each tick represents one frame, without
extra gap ticks or a baseline.

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
manually" opens search.
Remember a successful estimate like any chosen view. While it is pending,
manual selection remains available and takes priority over a late reply.
Failures show a short error and allow an explicit retry. There is no IP
configuration knob; a click is the opt-in. IP never enables GPS or tracking.
`/` opens one search: a city, a site id, or pasted coordinates in the same
field. There is no lat/lon form. Rows are tagged `place`, `site`, or `source`
(at most four). Places rank first unless the query is three or four letters
(a site id or its prefix: `kfcx`, `tlx`) or matches a live mosaic id or name
(`opera`, `eumetnet`). Enter on a place centres the map there, unlocks, and
selects a covering source; Enter on a site or mosaic source locks that radar
and centres on it. Clicking the station title opens the same card listing
covering mosaics and the nearest dishes. Coordinates are decimal degrees, latitude then
longitude, separated by a comma or a space (`36.23708, -79.97948`); they
commit as a place. Invalid range is named. Do not swap a lon,lat paste.
Place names come from an engine `search_places` reply over GeoNames cities
with population ≥ 5000 in the network envelope (state/region and country so
two Jacksonvilles are distinct); map labels stay Natural Earth.
No separate setup wizard or settings window is required. Coordinate entry
chooses a view; it does not create a permanent config override or lock a
radar. Choosing a location writes `state.json`, never `config.toml`.
The locate chip (`m`) is not search: one bounded `curl` to wttr.in, the
same fetch as onboarding. It centres on the estimate, unlocks, and selects
a covering source, keeping the current zoom. Off until clicked; a successful
onboarding estimate does not enable it. Failure leaves the camera and
flashes a short overlay on the map. Archived sessions never locate.

A GPS receiver is a map centre that moves on its own. With `gpsd = true`,
each fix from the local gpsd that has moved more than 100 m becomes the
centre, and the radar follows it by the same nearest-with-hysteresis
hand-off a pan gets. The crosshair beside the locate chip is the follow
chip: filled while a fix is steering the camera, outlined while a pan has
paused it, and outlined dimmed with `NO FIX` while the receiver has
nothing to report. Click to pause and resume; a user pan pauses follow;
`gpsd = false` turns GPS follow off entirely and hides the chip. A lock
still holds the radar while the map follows. `gpsd = true` follows *after*
we already have a view — it is not a fifth launch source and does not
touch `resolvePlace()`: the launch one-shot (config centre, remembered
state, weather, prompt) owns placement, an explicit `center_lat` /
`center_lon` still wins on launch, and a last fix is remembered like any
centre, so a relaunch opens where the receiver last was. A lost fix moves
nothing. Nothing is drawn at the fix: a position marker is its own
decision, against the overlay's collision layout.

Reuse Omarchy's location when available without requiring its weather plugin.
Read weather settings only; never write them. Location search is an explicit
user action handled through the engine. Approximate IP lookup is an explicit
UI action via wttr.in (`format=j2`, smaller than Omarchy weather's `j1`); the
launcher and engine perform no IP lookup. Do not use GeoClue. Archived views
never locate, and checks require the same explicit action as users.
Resolve radar separately: an explicit polar `locked_radar` in config wins,
otherwise restore a remembered exact selection lock, otherwise choose a
covering source from the map center. A radar lock alone does not supply a map
center or bypass location onboarding. Newly chosen locations start unlocked
unless a configured radar override applies.

Remember center and zoom after movement settles, and remember changes to the
UI radar lock. Unlocked radar selection follows the center. Polar-to-polar
selection uses the protocol's nearest-station hysteresis while the held dish
covers the center; leaving its coverage bypasses hysteresis. Cross-family and
grid selection follow [docs/grid-adapters.md](docs/grid-adapters.md). Lock pins
one exact dish or mosaic; `n` releases it and selects from the center without
moving the camera. Choosing a station in search (or from the station title)
locks it and centres the map on that site. Live mosaic sources appear in that
same radar list — covering mosaics on an empty browse, and by id or name when
typed — and choosing one locks that mosaic and centres on its coverage. They
are not fake dishes and there is no separate provider picker. Automatic
hand-off and loading a frame never move the camera. Persist only an explicit
lock, never an automatically selected source.

Closing preserves the view. Reopening restores it, with explicit config
values taking precedence. Expanding the popover preserves its center, zoom,
station, frame, and playback. An engine reconnect restores the necessary
commands without resetting the user's camera; the window and the bar share
one remembered lock and follow it, so a restart brings back the station the
user last chose, not whichever client reconnects last. Weather location
supplies an initial view; subsequent weather changes do not overwrite a
remembered view.

Explicit coordinates are honored on every launch and do not imply a radar
lock. A configured center far from a locked radar is valid: preserve both,
show the station and lock clearly (yellow lock when coverage is outside the
view), and offer "Use nearest radar" and "Go to selected radar" when that
radar's coverage is outside the view. UI navigation
and unlocking can change the active session; explicit config applies again
on launch.

A station with no frame yet is the map without radar. A static
“Loading...” sits in the centre of the map until the first scan time
arrives; brief loads do not flash the notice. No spinner, and no chrome
that resizes the map. The timestamp and timeline reserve their space while
empty. Present each sweep with its own decoded texture, azimuth lookup,
palette, and geometry together; keep the previous ready frame of the same
source while its replacement loads. A source change clears that frame, and
the first ready sweep fades in briefly. Display one radar station’s sweep
at a time.

## Split

The engine fetches, decodes, caches, and rasterizes. The UI is small state
plus GPU textures. Radar values do not enter JSON or QML. Pan and zoom are
uniforms. The engine reads neither `config.toml` nor `state.json`; the UI
resolves preferences and remembered state, then sends commands.

New settings are optional, omit means default, and a bad value is named in
the status slot. A plugin update the shell has not loaded yet is named there
too, and in the popover, where the notice restarts the shell on click; the
shell's rescan keeps the loaded QML, so nothing else can apply it.
Keep deliberate settings in `config.toml` and session restore in `state.json`.
The app never rewrites config because the user pans, zooms, or changes a lock.
Write `state.json` atomically. See [configuration](docs/configuration.md) for
file ownership and precedence. Do not write Omarchy, Hyprland, or system
configuration.

A product is a texture, legend, units, timestamp, and source from the engine.
Polar Level II and compiled grid mosaics are what is drawn.

Live join is [docs/radar-fetch.md](docs/radar-fetch.md): read the last
archive volume header, then poll the slots after it in the chunk bucket.
The newest name timestamp is the live volume; leftover keys in a reused
1–999 folder are not. The join picks the volume; radial age alone says
LIVE, STALE, or UNAVAILABLE. Empty polls are normal between chunks, but
90 seconds without a recent chunk restarts discovery.
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
