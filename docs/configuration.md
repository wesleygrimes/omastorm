# Configuration contract

The user-facing explanation is in [README.md](../README.md). This file is
the complete setting contract for checks, captures, and contributors.

## Preferences and remembered state

The UI reads two files with different responsibilities. Explicit configuration
wins; remembered state fills in values the user has not configured. Neither
file contains radar frames or downloaded map data, and neither is read by the
engine. The UI sends the commands described in [protocol.md](protocol.md).

| File | Owner and purpose | Contents |
| --- | --- | --- |
| `~/.config/omastorm/config.toml` | User-managed, deliberate preferences | Optional fixed launch center, radar override, treatment, weak-return floor, keybindings |
| `$XDG_STATE_HOME/omastorm/state.json` | App-managed, remembered session | Last map center, zoom, and optional radar lock chosen in the UI |

When `XDG_STATE_HOME` is unset, state lives at
`~/.local/state/omastorm/state.json`. Onboarding, panning, zooming, and UI lock
changes write state, not config. Deleting state resets the remembered session
without removing deliberate preferences. Missing or invalid state falls back
to the remaining location sources; it must not prevent startup. Keep state
small and write it atomically after movement settles or the lock changes.
Do not store credentials, radar data, or copies of all config preferences there.

## Launch and onboarding

Resolve the map center from the first valid source:

1. The complete configured `center_lat` / `center_lon` pair.
2. The remembered map center in state.
3. Omarchy's weather coordinates in
   `~/.local/state/omarchy/settings/weather.json` (`name`, `latitude`,
   `longitude`). File existence alone is insufficient; coordinates must be valid.
4. The location prompt: choose manually or use approximate location.

**Use approximate location** runs one `curl` to wttr.in (`?format=j2`) from
the UI to estimate your city from your public IP. No request happens until
you click; there is no configuration switch and no engine command. The
provider receives the connection's public IP. Omastorm never writes Omarchy's
weather settings. `format=j2` is used because Omarchy weather's `j1` response
is much larger; both expose `nearest_area`.

The UI remembers a successful view in `state.json`. A reopened app uses that
view without another lookup. The initial view is labeled `IP NEAR …`;
coordinates may reflect a VPN or ISP location. Choose manually to correct an
estimate.

While locating, manual selection remains available. A failed onboarding
request shows a short error on the prompt and can be retried. A failed
locate flashes an overlay on the map. Requests time out after ten seconds.
Manual selection or navigation takes priority over a late response. Archived
sessions never locate. `OMASTORM_LOCATION_URL` lets tests substitute a local
URL; isolated checks still need to invoke the explicit action.

IP lookup is an explicit click: onboarding or the locate chip (`m`). It never
enables GPS or continuous camera tracking.

A missing location opens the same choice in the popover and expanded window.
Choose manually opens search: a city, a site, or pasted coordinates
(latitude, then longitude). Accepting a location saves the
view in state. Weather-derived initial coordinates are also saved in state.
Neither route adds coordinate overrides to config. `/` opens that search
after onboarding. `m` jumps to the approximate location without typing.

Resolve the radar independently: configured `locked_radar`, then a remembered
UI lock, then a covering source from the resolved center. A configured radar
alone does not resolve a location. Coordinates never imply a lock. Choosing a
station or mosaic source in search locks it and centres the map on it. Choosing
a city or coordinates unlocks and selects a covering source. `n` resumes
automatic covering-source selection without moving the camera. A configured
override still applies.

Restore remembered zoom, or the default zoom when none is valid. Keep the
camera at the resolved location when frames arrive. Close and reopen preserve
the view, subject to config overrides. Engine reconnects preserve the active
view. Weather changes do not reset a remembered location.

## Explicit configuration

```toml
# Always open centered here. Set both; omit both to remember the last position.
center_lat = 36.23708
center_lon = -79.97948

# Optional: use this radar on launch regardless of map center.
# Omit to restore the UI lock, or select automatically when no lock is remembered.
# locked_radar = "KFCX"

# Optional: follow a GPS receiver on the local gpsd. The map centre tracks the
# fix and the radar hands off as you drive; a lock still holds.
# gpsd = true

treatment = "GLYPHS" # PIXELS, GLYPHS, or STIPPLE at launch; Glyphs when omitted
weak_floor = 5       # dBZ; false draws every measured return

[metar]
show = false                         # ICAO chips on the map; omit or false is off
pick = "nearest"                     # nearest to the radar, or "priority" (AWC tiers in view)
count = 16                           # 1–16 chips; omit is 16. 8 or 4 shrinks the pool
always_on_when_in_view = "KM19"      # quoted ICAO list; pinned first while on screen
mark = "chip"                        # chip (filled category block), ink (ICAO in category color), pin (larger category marker)

[keys]
pan_left = "h Left"
zoom_in = "+ ="
```

- `center_lat` / `center_lon`: finite numeric latitude in [-90, 90] and
  longitude in [-180, 180]. Both are required together. Report an incomplete
  or invalid pair in the status slot and fall back to the next location
  source. Valid coordinates win over remembered center on every launch and
  bypass the location prompt. Panning still works and updates state; reopening
  returns to the configured center. Removing the pair resumes the remembered
  position. Zoom remains independent.
- `locked_radar`: a polar station id from `hello.sites`. It is not a mosaic
  or provider setting. Report an invalid id in
  the status slot; do not silently substitute another locked station.
  A valid override wins over the remembered lock on launch. It never moves
  the map. Unlocking in the UI affects the session and remembered lock;
  config applies again on launch. Remove this setting and unlock in the UI
  to keep automatic selection across launches.
- `gpsd`: `true` relays the local gpsd through `gpspipe -w` (part of the
  gpsd package). While a receiver has a 2D-or-better fix, each fix that
  has moved more than 100 m becomes the map centre — the source reads
  `gps` — and the engine's ordinary hand-off picks the nearest radar,
  with its own hysteresis, exactly as a pan would. Follow starts only
  once a view exists: it never places the launch view, and an explicit
  `center_lat` / `center_lon` still wins. The crosshair on the
  map is the follow chip: filled while a fix is steering the camera,
  outlined after a pan has paused it, outlined dimmed with `NO FIX`
  while the receiver has nothing to report. Click the chip to pause and
  resume; turning `gpsd` off hides the chip and stops following. A lock
  holds the radar while the map keeps following; a user pan pauses the
  follow so the chip click resumes from where the camera was. The
  remembered view is written like any other centre, so the last fix is
  where a relaunch opens. A lost fix leaves the view where the receiver
  last was; gpsd not answering is retried every five seconds while the
  key is on. Off when omitted.

A Jacksonville map center with `locked_radar = "KFCX"` is valid. Honor both
settings even when the sweep is outside the view. Show the selected station
and lock, with "Use nearest radar" and "Go to selected radar" available when
coverage is outside the view. Never relocate the camera or discard the lock
silently. The lock control uses the theme yellow when the camera sits outside
that radar's coverage footprint, not its range rings.

For agent-assisted installation, write coordinate overrides only when the
user requests a fixed launch location. Ordinary installation leaves them
unset so weather location or onboarding establishes a remembered view.

`OMASTORM_CONFIG` names another config file for checks and captures; a missing
file is no configuration. `OMASTORM_STATE` names another state file;
`OMASTORM_LOCATION` names another weather file. When `OMASTORM_CONFIG` is set,
the machine's own state and weather files are not read unless
`OMASTORM_STATE` or `OMASTORM_LOCATION` names one.

## Display and keyboard preferences

- `treatment`: the treatment at launch and whenever the file changes; the
  keys and the chip change it afterwards without writing the file.
  `OMASTORM_STYLE`, set by the capture scripts, outranks it.
- `weak_floor`: the weak-return floor at
  launch and whenever the file changes: a number in the product's units, or
  `false` to draw every measured return; 5 when omitted. The `weak` key
  toggles between off and this floor afterwards without writing the file.
  `OMASTORM_WEAK` (`off` or a number), set by the capture scripts, outranks
  it. Anything else is reported like a bad `treatment` and leaves the default.
- `[metar] show`: optional. `true` seeds the METAR overlay on (ICAO chips
  replace city names around the selected live NEXRAD radar). Omit or
  `false` is off. US and Canada only; on OPERA Europe the overlay and the
  `aviation` key are a no-op. The `aviation` key (`a`) toggles the
  session without writing the file; an edit of this value re-seeds. A
  value that is not a boolean is named in the status slot like a bad
  `treatment`.
- `[metar] pick`: optional. `"nearest"` (omit is this) is the 16 closest
  stations to the selected radar. `"priority"` takes stations in the
  current map view and ranks them by AWC stationinfo `priority` (1 is a
  hub such as KBNA) then distance to the radar, so a major airport on
  screen beats a closer small field. Anything else is named in the status
  slot. The engine never reads this file; the UI sends `pick` and the view
  box on `metar_query`.
- `[metar] count`: optional. How many chips, 1 through 16. Omit is 16.
  8 or 4 shrinks the pool. A non-integer or a number outside 1–16 is named
  in the status slot.
- `[metar] always_on_when_in_view`: optional. Quoted ICAO ids separated by
  spaces (`"KM19"` or `"KM19 KMEM"`). Those stations take the first chip
  slots whenever they are in the map view and have a current METAR, even
  when `pick` is `"priority"`. A home field stays on screen instead of
  being crowded out by hubs. Toml.js is the scalar subset, so this is a
  string, not a TOML array. Bad tokens are named in the status slot.
- `[metar] mark`: optional. `"chip"` (omit is this) is the filled
  flight-category block Wes's screenshot used. `"ink"` colors the ICAO
  letters with that category and drops the fill. `"pin"` leaves the ICAO
  as theme chrome and paints a larger location marker in the category
  color. Anything else is named in the status slot.
- `[keys]`: one entry per action, laid over the defaults in `ui/Keys.js`:
  `search` (`/ s`), `nearest` (`n`), `lock` (`Shift+L`), `locate` (`m`,
  approximate location), `pan_left`
  `pan_down` `pan_up` `pan_right` (`h j k l` and the arrows), `zoom_in`
  (`+ =`), `zoom_out` (`-`), `reset` (`0`, the resolved location), `previous_frame` (`[`),
  `next_frame` (`]`), `play` (`Space`), `oldest` (`Home`), `newest` (`End`),
  `pixels` `glyphs` `stipple` (`1 2 3`), `weak` (`w`), `aviation` (`a`), `help` (`?`), `close`
  (`Escape`).
  A value that is not a quoted string, a sequence Qt cannot parse, an
  unknown action, or a key another action already holds leaves that action
  on its default and is named in the status slot (`[KEYS] ZOOM_IN = "FOO":
  FOO IS NOT A KEY`, with a count of any further mistakes) until the file is
  fixed; a bad `treatment`, `weak_floor`, centre, or `locked_radar` is
  reported the same way. `home_site` and `follow` are unused and named if
  present.

## Remembered state

`~/.local/state/omastorm/state.json` is written atomically (a temporary file
renamed into place). It holds the last map centre, span in kilometres, and
the UI radar lock when one is set. Protocol v2 stores that lock as the exact
selection identity. A leftover string lock is read as a NEXRAD site for one
migration release and rewritten in the object form:

```json
{"lat":30.332,"lon":-81.656,"span":210,
 "lock":{"sourceId":"nexrad","target":{"kind":"site","siteId":"KJAX"}},
 "name":"Jacksonville"}
```

A mosaic lock is `{"sourceId":"fixture-mosaic","target":{"kind":"mosaic"}}`.
The previous `"lock":"KJAX"` string is accepted on read, then written back as
the object above. `locked_radar` in config.toml stays a polar site string;
there is no provider setting or mosaic picker.

Invalid fields are dropped. A missing file is no remembered view.
`OMASTORM_LOCATION` names another weather.json (`name`, `latitude`,
`longitude`, written by the shell's weather panel) for checks; coordinates
outside ±90/±180 are ignored.
