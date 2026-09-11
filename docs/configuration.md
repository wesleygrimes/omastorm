# Configuration

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

While locating, manual selection remains available. A failed request shows a
short error and can be retried. Requests time out after ten seconds. Manual
selection or navigation takes priority over a late response. Archived
sessions never locate. `OMASTORM_LOCATION_URL` lets tests substitute a local
URL; isolated checks still need to invoke the explicit action.

IP lookup is a one-time starting position. It never enables GPS or continuous
camera tracking.

A missing location opens the same choice in the popover and expanded window.
Choose manually opens the existing search and coordinate fields. Accepting a location saves the
view in state. Weather-derived initial coordinates are also saved in state.
Neither route adds coordinate overrides to config. The picker remains
available through `Shift+H` and LOCATION after onboarding.

Resolve the radar independently: configured `locked_radar`, then a remembered
UI lock, then the nearest station to the resolved center. A configured radar
alone does not resolve a location. Coordinates never imply a lock. Choosing a
station in search locks it and centres the map on that site. `n` selects the
nearest radar without moving the camera. Choosing a location through the
picker clears a remembered lock; a configured override still applies.

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

treatment = "GLYPHS" # PIXELS, GLYPHS, or STIPPLE at launch; Glyphs when omitted
weak_floor = 5       # dBZ; false draws every measured return

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
- `locked_radar`: a station id from `hello.sites`. Report an invalid id in
  the status slot; do not silently substitute another locked station.
  A valid override wins over the remembered lock on launch. It never moves
  the map. Unlocking in the UI affects the session and remembered lock;
  config applies again on launch. Remove this setting and unlock in the UI
  to keep automatic selection across launches.

A Jacksonville map center with `locked_radar = "KFCX"` is valid. Honor both
settings even when the sweep is outside the view. Show the selected station
and lock, with "Use nearest radar" and "Go to selected radar" available when
coverage is outside the view. Never relocate the camera or discard the lock
silently. The lock control uses the theme yellow when the camera sits outside
that radar's rings.

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
- `[keys]`: one entry per action, laid over the defaults in `ui/Keys.js`:
  `search` (`/ s`), `nearest` (`n`), `lock` (`Shift+L`), `home` (`Shift+H`,
  the location picker), `pan_left`
  `pan_down` `pan_up` `pan_right` (`h j k l` and the arrows), `zoom_in`
  (`+ =`), `zoom_out` (`-`), `reset` (`0`, the resolved location), `previous_frame` (`[`),
  `next_frame` (`]`), `play` (`Space`), `oldest` (`Home`), `newest` (`End`),
  `pixels` `glyphs` `stipple` (`1 2 3`), `weak` (`w`), `help` (`?`), `close`
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
the UI radar lock when one is set:

```json
{"lat":30.332,"lon":-81.656,"span":210,"lock":"KJAX","name":"Jacksonville"}
```

Invalid fields are dropped. A missing file is no remembered view.
`OMASTORM_LOCATION` names another weather.json (`name`, `latitude`,
`longitude`, written by the shell's weather panel) for checks; coordinates
outside ±90/±180 are ignored.
