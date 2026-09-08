# Omastorm design notes

## Intent

An Omarchy-first radar app with the focused weather controls of RadarScope and
the monospace, ASCII/pixel visual language of the desktop. It should feel at home
beside native shell panels and remain useful as a popover or quarter, half, or
full tile.

Aesthetic references: Omarchy's native network panel, login and lock screens,
and ASCII screensaver.

## Agreed principles

- Use Quickshell as the UI direction and honor the active Omarchy system theme.
- Make monospace typography, precise spacing, restrained controls, and pixel/ASCII
  texture part of the design language.
- Render actual numerical radar measurements ourselves so the returns can share
  that visual language.
- Keep the feature set focused and simple. Fold capabilities in iteratively and
  review together at each milestone.
- Support multiple sizes through a shared radar view with controls adapted to
  available space.
- Treat mPING as optional, depending on access availability.

## Visual direction

Keep geography quiet: thin boundaries, sparse place labels, range rings, and a
precise crosshair. The weather should occupy most of the view. Geography needs
documented sources and attribution when selected.

Inherit interface background, foreground, borders, accents, and font from the
active Omarchy setup. Radar intensity uses a fixed, labeled, ordered palette
that stays readable across themes. The exact palette remains open to refinement.

Keep data meaning separate from visual treatment. Preserve original measurements
for inspection, distinguish missing values from weak returns, and validate any
sampling used to fit measurements into display cells. Show actual scan times and
label archived fixtures clearly.

### Treatments (decided 2026-09-05)

All three treatments sample the identical cell and palette; they differ only in
how each 3 px screen cell is painted. Implementation cost is a few shader lines
each, so the choice is about reading weather, not code size.

| Treatment | Coverage by intensity group | Character |
| --- | --- | --- |
| Pixels | 100% everywhere | Solid swatches; what you see equals the legend. |
| Glyphs | 22 / 44 / 78 / 100% | Fixed density masks standing in for ░▒▓█; exact palette colors, solid top band. |
| Stipple | 34 / 44 / 56 / 69% | Centered squares; quietest look, compressed intensity contrast. |

Decision: keep all three. **Glyphs is the default**, being the most literal match
for Omarchy's terminal vocabulary and the mode that gives up the least when
reading a storm. Pixels stays as the honest comparison view. Stipple stays on
probation until the milestone 2 review, after a live storm has been watched in
each mode. Its weakest bands were raised from 1 and 1.5 px squares (11% and 25%
coverage) to 1.75 and 2 px so light rain is no longer visually suppressed; the
top band is unchanged.

Range-folded cells use the same opaque 3×3 X in every treatment: five light
pixels (#f5f5f5) over four dark pixels (#181818). Both shape and neutral colors
separate status from the intensity palette, and the dark backing preserves
contrast in light themes. The legend labels it “X: folded”. Synthetic GPU
captures cover all three treatments because the archived scan has no folded gates.

Follow-ups: consider rendering legend swatches with the active treatment; consider
mirroring the monospace font's actual shade-glyph patterns in the glyph masks.

### Palette from state (decided 2026-09-06)

`frame.palette` stays in the protocol and is the only source of radar color.
The UI renders it as a `bands` × 1 strip of 1 px rectangles behind a
`ShaderEffectSource` and the shader samples texel centers by class index, so
the legend and the radar cannot disagree and a product with a different band
count (velocity, milestone 3) needs no shader change. Treatment coverage groups
are quartiles of the band count rather than fixed indices; for the current 12
bands that reproduces the previous 3/6/9 thresholds exactly. A uniform array
was rejected because Qt Quick's `ShaderEffect` maps only scalar, vector,
matrix, and texture properties into the uniform block, and a fixed-size array
would cap the band count anyway. Station, product, unit, and source text on
screen comes from socket state; QML holds no station or product names.

### Weak-return floor (built 2026-09-07; the default is for the milestone 2 review)

The first live evening showed the lowest cut full of biological scatter and
clutter under about 10 dBZ that consumer radar hides (open choices, below;
PLAN.md). RadarScope's model is the one adopted: its default palette leaves
the low end uncolored and the full scale is one setting away. Omastorm's
floor is a view setting, not a palette change: measured returns under
`weak_floor` dBZ draw nothing, the legend dims the swatches under the floor
to the background with a tick where it falls and says `measured <5 dBZ
hidden (W shows)`, and `w` toggles between the floor and every return. The
default is 5 dBZ; `weak_floor = false` in config.toml draws everything.

Mechanics: the frame carries the moment's `scale` and `offset`
(docs/protocol.md), the UI turns the floor into a code threshold, and the
shader compares the raw code in the texture's B channel, so the floor can sit
inside a palette band and no texture is re-encoded when it changes. Folded
and below-threshold codes are never weak. The rendering test replays the rule
on the default view (`render-default-glyphs-floor5`). The window and the
popover share the setting through the session, like the treatment.

Open for the review, with live evenings in both scan modes: the default
value (5 or 10 dBZ), whether it should follow the VCP once the frame carries
one, and whether a dimmed rendering beats hiding. The honesty rule stands:
whatever hides is named in the legend.

## Layout direction

Two entry points, one app. A bar icon opens the popover: a glance at the home
radar with scan age, play/pause, and expand. Clicking the popover or pressing
the global key opens the freestanding window, which behaves like any other
application window (Foot, VS Code) and carries the selected radar over. The
exact shape of both, and how the popover hands off to the window, is settled by
the phase 4 UX design pass in PLAN.md on a design canvas before any of it is
built.

| Surface | Intended controls |
| --- | --- |
| Popover | Home radar, scan age, play/pause, expand |
| Quarter tile | Product selector, compact legend, timeline |
| Half/full tile | Elevation controls, reports, detailed inspection |

Below 560 px, optional labels and zoom buttons hide; product, timestamp, badge,
legend, treatment, and reset remain. Minimum is 360×360 at a 12 px base font,
scaled with larger fonts. Radar keeps equal x/y scale and cover framing, with
bounded pan so empty space beyond coverage is never exposed.

### Phase 4 UX pass (decided 2026-09-06)

Mocked on the canvas in `docs/design/phase4-ux/` from captures of the running
app, the active theme's tokens, and the shell's own popup and bar metrics, and
reviewed with Wes the same day. Everything below is decided.

- **Popover.** A 336 px card (308 content; the shell's default is 280) with
  the shell's 14 px padding and 2 px active-border edge: a hero row with the
  site ID, city, and a status dot with `LIVE · 2 min ago`; the home radar at
  308×280 with an expand glyph and two caption chips (product and elevation,
  scan time); a step-back / play / step-forward row over the frame strip; a
  footer with the key hints and an EXPAND control. No legend and no treatment
  buttons: the popover inherits the window's last treatment.
- **Path.** Bar icon click opens the popover; click outside or Esc closes it.
  Clicking the radar, ↵, or EXPAND opens the window at quarter size carrying
  site, frame, and play state, and closes the popover. The global key toggles
  the window directly. Esc closes the window; the engine keeps polling the
  home site so the icon and popover stay current.
- **Bar icon.** The 16 px one-color mark in the theme foreground, in the
  shell's 27 px slot with its open-state fill while the popover is up. Dimmed
  to .6 for any state but LIVE; a 5 px red corner mark only when the feed or
  the station is down; never animated.
- **Window chrome.** The mark replaces ◈ in the lockup; the badge reads LIVE
  or ARCHIVED; the third header row gains the scan age, and its status slot
  carries the connection condition or the sweep in progress; a FOLLOWING /
  LOCKED chip follows the site name; a timeline row sits between the map and
  the legend; SEARCH and the lock control lead the controls row; a `?` chip
  on the map opens the keys sheet. Half and full show the same controls with
  room; the milestone 3 elevation selector takes the product slot when it
  lands.
- **Treatment control (decided 2026-09-06, first review pass).** The three
  full-width PIXELS / GLYPHS / STIPPLE buttons go. The treatment is a view
  setting, not an action: one low-emphasis chip names the current treatment,
  click opens the three in the shell's menu style (or cycles), keys `1 2 3`
  are unchanged, Glyphs stays the default, and the popover has no control and
  inherits the window's choice. The chip sits at the right end of the
  controls row before zoom and RESET: the shell's 30 px control, focus ring,
  and tab order, and nothing over the radar. A chip in the map's top-right
  corner was rejected as an opaque control on the radar.
- **Search placement (decided 2026-09-06).** SEARCH and the lock stay
  dedicated buttons leading the controls row, ahead of the treatment chip,
  zoom, and RESET. Two alternatives were mocked and rejected: a small search
  button beside the site name, and the site name itself as the control with
  the picker dropping beneath it. Wes preferred the explicit control. The
  popover's name is not a control; it opens the window.
- **Timeline.** One 2 px tick per frame in the station's ring buffer, newest
  at the right; the current frame is a taller 3 px accent tick, a missing
  frame a stub on the baseline, the sweep in progress a hollow accent tick
  past the newest frame. First-frame time and `now` label the ends. Space
  plays at one frame per 250 ms and loops; `[` `]` step and pause; Home and
  End jump; dragging scrubs. Transport sits left of the strip at every size;
  the popover has step and play only.
- **States.** LIVE under 10 min says only the age. STALE at 10 min or more
  with the feed up turns the age yellow and names itself. LOADING (first frame
  for a site, or a station switch) is accent with the loading view.
  UNAVAILABLE (feed up, station silent 30 min or more) and OFFLINE (feed
  unreachable) are the theme red with the last time; cached frames keep their
  real age and stay steppable. A per-client rejection outranks a condition in
  the status slot while it stands.
- **Loading and scanning view.** Owns the whole map area while no frame is
  drawable: opaque theme background, range rings and crosshair at the home
  span, a sweep wedge drawn on the treatment's 3 px cell grid turning once per
  six seconds (not the radar's real rotation), the site ID, `WAITING FOR FIRST
  SWEEP`, and the live radial counter. No place labels until the first frame.
- **Markers.** As drawn today, plus lock: `L` pins the station with an accent
  1 px frame on the marker and tag, LOCKED in the header, and the lock control
  selected; panning no longer hands off. Selecting a site from the picker
  locks it. `n` jumps to the nearest station and releases the lock.
- **Picker and keys.** The picker follows the launcher tokens (scrim at .5,
  card at .95, foreground border, selected row fill .08 with accent text):
  an input row, four matches with the matched letters in accent and the
  distance and bearing from the map centre, a hint footer. Keys as listed in
  "Picker and keyboard" above plus Home / End for the timeline and `?` for the
  sheet; the sheet says the bindings live in `~/.config/omastorm/config.toml`.

### Plugin surfaces as built (2026-09-07, phase 5 session 1)

- The manifest pairs `ui/RadarBar.qml` with `ui/Panel.qml`. The bar uses
  the installed shell's `BarIconButton` and `KeyboardPanel`, inheriting
  outside-click dismissal, focus acquisition, and popout coordination.
  Its 336 px card has 308 px content: the 14 px inset includes the 2 px
  border. A small attribution line follows the approved footer.
  The same themed 16 px branded mark replaces the old header diamond.
- `Popover.qml` is the production card in both the shell and the offscreen
  harness. It mounts `RadarMap` at home span, with pointer interaction
  reserved for expand. `Timeline.js` shares missing-frame stubs with the
  window. The window lives in `RadarWindow.qml`; `shell.qml` only hosts
  its standalone instance. Only standalone instances may quit Quickshell.
- `PluginSession` owns config, theme, the last treatment, and one persistent
  status socket for all bar instances. Each visible card and the window has
  its own `Engine` connection, so their tile rectangles cannot supersede
  each other. Opening the window sends no station, seek, or play command:
  the daemon already owns that state. The shell's toggle opens it; if already
  open, expand uses summon to avoid accidentally hiding it. Close returns
  to the configured/location home, only selecting when it differs; with no
  home, the current station remains. Home config edits still apply at once.
- The development bootstrap invokes `run.sh --ensure` by argv from the
  installed plugin directory, or `OMASTORM_ROOT` for a checkout harness.
  It runs `ensure` without building or launching another UI. The Process
  working directory is `$HOME` because
  Quickshell's cwd is `qrc:/qs-blackhole` and bash will not start there.
  A checkout with `target/debug/omastorm-engine` stays on that binary.
  Otherwise `--ensure` runs `scripts/install-engine.sh`.
- Cold start exposed Quickshell's failed-connect socket retention: its
  setter only connects when its underlying socket is null, but an initial
  error leaves that socket allocated. `Engine.qml` now replaces a
  disconnected Socket every second; incompatible protocol still latches.
  Checked against upstream `src/io/socket.cpp` and the installed 0.3.1
  runtime, with cold start and daemon restart in `check-popover.sh`.
- `PopoverHarness.qml` hosts the actual card and plugin window without
  Omarchy. Live captures use their own daemon/cache and a representative
  bar strip; they are not screenshots of an installed plugin. A separate
  temporary Wayland host loaded the actual installed shell components and
  opened/closed the popover. The clean source tree passes Omarchy validation.
  Installing with `omarchy plugin add` was run 2026-09-07 (user-facing
  README as built, below). Engine packaging is the following as-built note.

### Engine packaging as built (2026-09-07, phase 5 session 2)

- Two artifacts, one pin. The UI ships in the git clone (`manifest.json`
  at the repo root). The engine is `omastorm-engine-x86_64-unknown-linux-gnu`
  on a GitHub Release of `wesleygrimes/omastorm`, hashed in
  `engine/release.pin` (`tag`, `repo`, `asset`, `sha256`). The pin is the
  source of truth; a `SHA256SUMS` next to the asset is for humans. Pinning,
  not `latest`, because UI and engine share protocol v1.
- `scripts/build-engine-release.sh` builds `--release --locked --offline`
  on `x86_64-unknown-linux-gnu`, copies and strips the binary into
  `target/dist/`, writes `SHA256SUMS`, and with `--write-pin` updates the
  pin. It does not tag, publish, or push. Release order: publish that
  exact file as Release `engine-0.1.0`, then the pin may reach origin.
- `scripts/install-engine.sh` installs under
  `$XDG_DATA_HOME/omastorm/bin/omastorm-engine` (default
  `~/.local/share/omastorm/bin`). A dest whose sha256 already matches is
  left alone. Otherwise it copies `OMASTORM_ENGINE_ASSET` or curl-fetches
  the pinned URL (`OMASTORM_ENGINE_URL` overrides), verifies the committed
  hash, and temp-and-renames. A mismatch is refused. A missing GitHub asset
  names the publish step and the checkout build. `ensure` still replaces a
  stale daemon by self-hash.
  ARM64 packaging uses the same flow with `omastorm-engine-aarch64-unknown-linux-gnu`
  and a separate `engine/release-aarch64.pin`. The native release builder
  selects the host's asset and pin; the installer selects by `uname -m` and
  refuses a pin for another architecture. Until the ARM64 asset is published
  and pinned, it gives source-build instructions. No speculative hash ships.
- `run.sh --ensure` uses `target/debug/omastorm-engine` when that file is
  executable (ordinary development, no fetch). Otherwise it runs the
  installer and execs the installed binary's `ensure`. Ordinary
  `bash run.sh` still builds offline and never calls the installer.
- Removal is the installed binary, `omastorm-engine stop`, and
  `~/.local/share/omastorm` plus the cache dir; the plugin never writes
  Omarchy configuration. The opt-in launcher entry is a separate file
  (launcher entry as built, below). A real `omarchy plugin add` was run
  2026-09-07 (user-facing README as built, below).
- Published 2026-09-07 (phase 5 session 3): the repository is public and
  GitHub Release `engine-0.1.0` is that exact asset plus `SHA256SUMS`.
  The pin is on `main`. A clone with no Rust and no
  `OMASTORM_ENGINE_ASSET` fetches, verifies, and `--ensure`s.

### Launcher entry as built (2026-09-07, phase 5 session 4)

- `scripts/install-launcher.sh` writes
  `$XDG_DATA_HOME/applications/omastorm.desktop` (default
  `~/.local/share/applications/omastorm.desktop`). Exec is
  `omarchy shell shell toggle com.omastorm.radar "{}"`, matching the
  summoning command: the target is `shell`, and `omarchy-shell` supplies
  `{}` when it is omitted, but the desktop file carries it so gtk-launch
  and a shell one-liner stay identical. Icon is this tree's
  `branding/mark/omastorm-mark-app.svg` as an absolute path, the same
  pattern Omamail uses for its mark. `TryExec=omarchy` hides the entry
  where the shell command is absent. `StartupNotify=false` because the
  window already lives in the running shell.
- Ordinary `bash run.sh`, `run.sh --ensure`, and
  `scripts/install-engine.sh` never call it. Removal is deleting the
  file; `update-desktop-database` is best-effort after a write.
  `scripts/check-launcher.sh` uses a scratch `XDG_DATA_HOME` and asserts
  the Exec, the Icon, validate, a refresh, that Quickshell's
  `DesktopEntries` list includes `omastorm` after the write and drops it
  after `rm`, and that install and launch never call the script.

### Global keybinding as built (2026-09-07, phase 5 session 5)

- Documented only. The user adds one line to
  `~/.config/hypr/bindings.lua`:
  `o.bind("SUPER + SHIFT + R", "Omastorm", "omarchy shell shell toggle com.omastorm.radar '{}'")`.
  The plugin, `run.sh`, and the installers never write Hyprland, shell, or
  theme configuration. `SUPER + SHIFT + R` is absent from
  `/usr/share/omarchy/default/hypr` (`RIGHT` and `RETURN` are different
  chords). On this machine the same chord already launches the checkout
  through `run.sh`; replacing that line is the switch to the shell toggle.
  The in-app map in `KeysSheet.qml` still says the global key lives in
  `bindings.lua` and does not name the chord, so a user who picks another
  key is not contradicted. `scripts/check-bind.sh` asserts the README line,
  the free default, and that install and launch do not touch
  `bindings.lua`.

### Lean startup as built (2026-09-07, packaging)

- Nothing archived ships in the engine. The binary embeds the station
  table, the frame template (`engine/data/fixture.json`: product, palette,
  bounds), and the Natural Earth geography; the 9.5 MB KTLX volume that
  was the offline first frame is gone, which is about 45% of the release
  asset. A daemon starts `live` with no station: `site.id` empty,
  `loading`, the placeholder frame sited at the middle of the network so
  the map shows every marker until the UI selects the home station, which
  it does on its first state. `OMASTORM_ARCHIVE=<Level II volume>` keeps
  the archived start for development, `scripts/check.sh`, and the
  captures; the decoder tests read the same file from `data/raw/`.
- The loading view is gone with it. A station waiting for its first sweep
  is the map without radar: tiles, labels, markers, coverage, rings, and
  crosshair as usual, the radar layer empty, `LOADING · <site>` in the
  status slot. With no station yet the rings, crosshair, and tag stay
  away and the slot says `NO STATION · PAN OR SEARCH`. The sweep wedge
  shader and the words under the crosshair were removed.
- Clock readings are the machine's local time zone (Qt formats the UTC
  scan time; standalone readings carry the zone abbreviation). The wire
  stays UTC.

### Backfill and home as built (2026-09-07, packaging)

- On joining a station the poller, three seconds after the live frame is
  up (so a hand-off passed while panning costs nothing), fetches the lowest
  cut of the twelve previous volumes newest first, one Start chunk plus the
  few chunks that carry the cut, skipping volumes whose start time the
  catalog already holds; each lands as `Event::Backfill`, joins the catalog
  and the timeline in time order, and never takes the screen. The task dies
  with the poller. Playback paces the loop to about ten seconds whatever
  the count, 250 ms to 1 s per frame, re-judged every step.
- `Shift+H` and a HOME control save the station on screen as `home_site`
  in config.toml, replacing the top-level line or adding it above the
  first table and touching nothing else; the file watch applies it and the
  status slot confirms for three seconds. The control hides while the
  station is already the home. Without a saved home the home is still the
  station nearest Omarchy's weather location.

### User-facing README as built (2026-09-07, phase 5 session 6)

- GitHub README is the install and use document, in the Omamail shape:
  `omarchy plugin add https://github.com/wesleygrimes/omastorm --enable`,
  popover and window, keys, config, opt-in launcher, documented bind,
  removal. The north-star checkout README is replaced; contributor
  launch stays in AGENTS.md and engine/README.md.
- A real add was run on this machine (2026-09-07): that exact command,
  clone at `~/.config/omarchy/plugins/com.omastorm.radar` on origin
  `main`, enabled, bar widget on the right, pinned engine
  `6801a4ee7b83df7a9861ac92b8683969e7c44bbc0cbbc18603a35005d28f7496`
  under `~/.local/share/omastorm/bin`. Omarchy `4.0.2-1`. The add writes
  Omarchy configuration; this session did not run it. Holding page
  still has no repository link.
- Live stills (`docs/media/window-live.png`, `popover.png`) are isolated
  KTLX captures of the current UI. The 24 s demo is one continuous
  camera path per theme on the archived fixture, stills at home for the
  treatment table (`scripts/capture-demo.sh`, `scripts/capture-readme.sh`).

## Technical foundation (decided 2026-09-05)

**Architecture.** A headless engine does fetching, decoding, caching, and
rasterization. The Quickshell/QML view is thin and reactive: it receives small
JSON state over a unix socket and GPU-ready textures from
`$XDG_RUNTIME_DIR/omastorm/`. Radar values never pass through JSON or QML
JavaScript. Pan and zoom change shader uniforms only.

**Phase 1b fixture transport.** The Rust binary embeds the existing index PNG,
small scan metadata, and a dated NCEI station snapshot. The UI reads only socket
state and the published texture; basemap JSON remains until phase 3. A shared
persistent daemon uses an OS lock for single-instance startup and stale-socket
recovery. Revision paths remain unique across restarts, with a 30-second grace
period for retired files measured from when the daemon last saw a path in its
own state, never from file timestamps. The launcher builds offline and checks the existing
daemon's build fingerprint; a differing build is ended and replaced by this
build's daemon (decided 2026-09-06, reversing the phase 1 refusal: windows
reconnect within a second, while the refusal left the launch key dead after
every rebuild until a manual stop, and a plugin upgrade would have hit the
same wall). `omastorm-engine stop` does the same on request. Both signal the
daemon and wait for its socket and lock to be released. The daemon needs no
signal handler. The
fingerprint is a hash of the executable image, taken once at startup through
`/proc/self/exe`, rather than a list of sources and assets: it covers
dependencies and the compiler too, and cannot fall out of date when a module
is added. A byte-identical rebuild keeps the same fingerprint; any other
rebuild is treated as a different build, which errs toward a restart. This is
the transitional fixture path, not the live engine or decoder implementation.

**Errors: conditions in state, rejections as events (decided 2026-09-06).**
`state` is shared by every client and describes the engine's data path, so the
only errors in it are conditions of that path: `connection.status` (`stale`,
`offline`, `loading`), with a `message` added in phase 4 if a status needs
words. A client command that cannot be carried out, whether malformed or asking
for a site, frame, or product the build does not serve, is a per-client
matter: the engine answers the sender alone with an `error` event and neither
changes nor re-broadcasts `state`. The previous single `state.error` string
made one client's typo overwrite and a later accepted command clear whatever
another client was showing; with a live feed in phase 4, that would have let a
popover keystroke erase the window's connection failure or flash a rejection
across every surface. On the client, transport trouble (disconnected,
unreadable message, unknown version) and the latest rejection are separate
properties. A state broadcast clears the first and not the second; the
client's next command clears the second, because a rejection describes the
command that produced it and nothing else. They also show in different places:
transport trouble is centered on the map, which is empty whenever it applies,
while a rejection arrives with the radar fully drawn and so takes the header's
status slot (where the source detail sits) in the accent color, leaving the map
clear.

**Engine language: Rust.** Chosen over Go and Python after comparing libraries.
The `nexrad` crate family (`nexrad-model`, `nexrad-decode`, `nexrad-data`) is
actively maintained, decodes all Level II moments including dual polarization,
and already implements real-time chunk polling against the
`unidata-nexrad-level2-chunks` bucket. The Go alternative has not been updated
since 2023 and has no real-time support. A resident Python daemon is too heavy
for a shell companion. Rust gives a single static binary with low memory.

**Decoder crates (decided 2026-09-06).** The engine depends on `nexrad-data`
1.0.0-rc.7 with default features off plus `nexrad-model` (the Archive II reader:
gzip wrapper, LDM record splitting, bzip2), `nexrad-decode` 1.0.0-rc.3, and
`nexrad-model` 1.0.0-rc.2, pinned exactly because release candidates may break
between versions; phase 4 turns the `aws` feature on for live chunks. The
archived fixture volume is embedded in the binary, since launch never fetches.
The engine decodes only the first elevation cut, record by record, stopping at
the next cut's first radial, which is both quick at startup and the shape live
chunks take. Sweep rows are a stable sort by azimuth of decoded order, the
order the golden files were produced in. The first decode passed the pyart
golden comparison byte for byte on the first run; the one mismatch was ours
(ray times count from the cut's first radial, not from the whole second).

**Engine runtime: tokio from phase 3 (decided 2026-09-06).** Inspected
`nexrad-data` 1.0.0-rc.7 (crates.io newest, 2026-04-03; docs.rs "latest"
shows the older 0.2.0 because it prefers non-prerelease versions) and the
repository master. Every network call is `async fn` on a cached async
`reqwest::Client` (rustls, connection pool, no timeouts) behind the default
`aws` feature; there is no blocking variant, so a tokio reactor is
unavoidable. rc.7's real-time API is the pull-based `ChunkIterator`:
`start(site)` and `try_next()` are async, `try_next` returns `Ok(None)` when
the next chunk is not yet available, and `time_until_next()` tells the caller
how long to wait. Master adds `chunk_stream(PollConfig) -> impl Stream` behind
an `aws-polling` feature that sleeps on `tokio::time`.

Phases 3 and 4 are almost entirely I/O: lazy tile downloads and rasterization,
live chunk polling for one or more sites, and a frame catalog written while
that runs. Rather than host one async worker inside a threaded daemon and keep
two models in one small program, the whole engine moves onto tokio as the
first task of phase 3, before the first network code lands and while it is
about 600 lines with five integration tests: `tokio::net::UnixListener`, one
task per client, the shared state behind a `Mutex` still (no `.await` while
held), cleanup on `tokio::time::interval`, and the same protocol, files, and
launcher behaviour so the existing tests and captures check the port. Phase 2
stays synchronous decoding and is unaffected. In phase 4 the iterator runs as
a task with `tokio::time::timeout` around each call, because the client sets
none; dependencies are `nexrad-data` (default features) and `tokio` with
`rt`, `net`, `time`, `sync`, and `io-util`, not `full`. The window is
unaffected either way: it never waits on the engine, and smoothness comes from
the shader-side rules above.

Landed 2026-09-06 with tokio 1.53.1: a current-thread runtime (`rt` alone;
the work is I/O and the decode runs before the runtime starts, so the
multi-thread scheduler buys nothing), `UnixListener::accept` in a loop, a
reader task per client over `BufReader::take(MAX_LINE + 1)` and a writer task
draining a `tokio::sync::mpsc` queue of eight with `tokio::time::timeout` of
2 s per write, and the cleanup on `tokio::time::interval`, whose first tick
fires at once as the old thread did. `io-util` was missing from the planned
feature list: tokio's own feature docs put `AsyncReadExt`, `AsyncWriteExt`,
`AsyncBufReadExt`, and `BufReader` behind it. Protocol, files, launcher,
`stop`, fingerprint, and `engine.lock` are unchanged, and every existing
check passed without edits; startup stayed at about 0.3 s.

**Live sweeps as built (2026-09-06, phase 4 session 2).** Choices made while
building, each a refinement of the above:

- *Poller per station, events to the state owner.* `select_site` for a table
  station spawns one tokio task running rc.7's `ChunkIterator` (`aws`
  feature on; `aws-polling` would add `futures` for a stream the daemon does
  not need) and aborts the previous station's. The task knows `nexrad-data`
  and the sweep assembler and nothing of state or files: it sends
  `Sweep {site, sweep, complete, provenance}` and `Offline {site, reason}`
  events over a channel, and one task in `main.rs` encodes and publishes on
  the blocking pool, records complete frames, swaps the frame in, and
  broadcasts, dropping events for a station no longer selected. Every
  network call sits under a 30 s timeout (60 s for discovery), as decided.
- *Join by replay.* The iterator hands over the newest chunk and, joined
  mid-volume, the Start chunk; the chunks between them that the VCP maps to
  elevation 1 are downloaded once (twelve blindly if the VCP is unreadable),
  so the current volume's lowest cut appears within seconds of selecting a
  station and the next volume paints live. The iterator enters each next
  volume at its newest chunk, so the same replay runs there when a poll
  arrives after more than the Start chunk landed, and the assembler treats
  any chunk of a new volume as its beginning. Measured on the bucket: eight
  chunks replayed, the cut spanning six chunks of 120 rays, 30–250 ms per
  republish in a debug build.
- *Lowest cut = elevation number 1, ended by its last radial.* A frame is
  the radials whose elevation number is 1 in the current volume; the cut ends
  when a radial carries `ElevationEnd` or the next cut's first radial
  arrives, and a Start chunk begins a new volume. SAILS and MRLE cuts (extra
  0.5° sweeps mid-volume) are not yet frames; the timeline session decides
  whether they are, since they would double the frame rate at 0.5°.
- *The blank row.* A sweep whose rays leave a gap carries one blank row after
  the sorted rays, and the lookup names it for every tenth-degree entry
  farther than 0.75° from any ray (`GAP_DEG`). Without it the nearest-ray
  lookup smeared a half-finished sweep's last ray around the unscanned side
  of the circle. No shader or protocol shape changed: the row is a ray that
  draws nothing, `rays` counts it, and a complete 0.5° or 1° cut needs none,
  so the fixture's texture is unchanged and the golden checks still hold.
- *What shows on a switch.* The station's newest catalogued frame if it has
  one, else a one-row placeholder that draws nothing, both under `loading`;
  the frame is never left pointing at another station's sweep. Frames carry
  the station table's coordinates (the fixture keeps its measured ones).
- *Conditions.* A reachable feed's condition follows the age of the newest
  radial received (connection states as built, below). A single failed
  fetch is retried quietly after 5 s, since the bucket closes pooled
  connections now and then; the second in a row reports `offline`, four
  restart discovery. The daemon re-judges once a second while live and
  broadcasts only when the state text changed, so the age and the condition
  move on a quiet feed.
- *Catalog.* rusqlite 0.40 with bundled SQLite (a plugin install must not
  depend on the system library's version), WAL, one row per complete frame
  with the frame's JSON minus runtime paths and its PNGs beside it under
  `frames/<SITE>/`, sixty per station. `$XDG_CACHE_HOME/omastorm/` is now
  the one persistent location, shared with the vector-tile cache.
- *Home station.* `home_site` in `~/.config/omastorm/config.toml` is selected
  when state arrives (docs/protocol.md, configuration; site navigation as
  built below).

**Timeline as built (2026-09-07, phase 4 session 3).** The UX pass's timeline
row, with these engine-side choices:

- *The engine owns the position.* `state.timeline` is the station's catalog
  oldest first plus the sweep in progress as a last `partial` entry, and
  `frame` is one of them. Following is the absence of a pin: while the user
  has not stepped or sought away from the newest entry, each sweep event
  takes the screen; once pinned, sweeps only extend the timeline, and a step
  or seek that lands on the newest entry follows again. So End is `seek` to
  the last entry, and the UI needs no follow flag. A pinned frame that falls
  off the ring moves the pin to the new oldest frame.
- *Stepped frames are read back from the catalog and republished* under new
  `tex/` revisions every time. Anything shown earlier may have been retired
  by the 30 s rule, and checking for the file would race the cleanup; one
  900 KB copy into the runtime directory per step is cheap. The sweep in
  progress is held in memory (`Pending`) instead, since it is not in the
  catalog until it completes, and it is published only when it is to be
  shown. Encoding stays on the blocking pool; publishing moved under the
  state lock, where a tmpfs write takes about a millisecond.
- *Playback loops over complete frames only*, one per 250 ms, oldest after
  newest, on a task woken by `play`; the hollow partial tick stays out of the
  loop so a half-painted sweep never flashes by. `play` with fewer than two
  complete frames changes nothing. Step and seek pause. The archived fixture
  is a one-frame timeline on the same code.
- *One tick a second while live*, on the cleanup interval: the snapshot is
  re-judged and broadcast only if its text changed, which also stops any
  other repeated broadcast of identical state. Idle cost is one JSON
  comparison a second.
- *Missing frames are the UI's call.* The engine sends real times; the
  window inserts up to three stub ticks where a gap is about two or more
  median intervals, so a feed outage reads as absence without stretching
  the strip.
- *Left open for the loading-view session*: whether SAILS and MRLE 0.5° cuts
  become frames (they would double the low-level frame rate; `live.rs` keeps
  elevation number 1 only).

**Connection states as built (2026-09-07, phase 4 session 4).** The UX
pass's states, with these choices:

- *One judgement, by the newest radial.* Once a second while live, a
  reachable feed's condition is read from the age of the newest radial the
  station has published: the sweep in progress while one paints, else the
  newest complete frame. `ok` under ten minutes, `stale` from ten,
  `unavailable` from thirty. That replaces the earlier exception for a
  partial sweep on screen, so a half-finished cut from a station that then
  fell silent ages like any other evidence instead of reading SCANNING for
  good. `loading` (a switch) and `offline` (the poller) are not judged by
  age; the next sweep clears them. `ageSeconds` stays the newest complete
  frame's age, as the protocol says.
- *An empty listing is an answer.* The bucket holding no volume for a
  station (test and decommissioned table stations; KCRI on the first run)
  is `unavailable` at once, since the feed answered and the station is
  silent; only a failed or timed-out call is `offline`. The poller sends it
  as its own event and retries discovery on the same back-off.
- *The header's age is the frame on screen's.* The engine's `ageSeconds`
  plus the distance between the newest complete frame's scan time and the
  shown frame's, so a stepped-back frame says its own age, the sweep in
  progress says `just now`, and no local clock is consulted; it ticks with
  the live broadcast. Yellow under STALE, red under UNAVAILABLE and
  OFFLINE, foreground otherwise. Archived shows no age: the badge and LOCAL
  FIXTURE say it.
- *Status slot text*, from the canvas: nothing while LIVE and complete,
  `SCANNING` while a sweep paints, `STALE · LAST SWEEP 12:14 UTC`,
  `LOADING · KJAX`, `KJAX UNAVAILABLE · NO DATA FOR 47 MIN · LAST 12:14
  UTC` (`KJAX UNAVAILABLE · NO DATA` with nothing cached), `OFFLINE ·
  SHOWING CACHED 12:14 UTC · 3 MIN AGO` (`OFFLINE · NOTHING CACHED`). The
  canvas's radial counter and sweep azimuth wait for the loading view.
  Compact windows show the slot for any condition but `ok` and drop the
  middle clause. The radar layer alone dims to .6 under UNAVAILABLE; the
  basemap keeps its strength.
- *Theme colours* `yellow` and `red` come from Omarchy's `colors.toml`
  under those names, with Tokyo Night fallbacks in `ui/Theme.qml` beside
  the existing three.
- *A tile request is not the user's next command.* The map's own
  `tiles_needed` no longer clears the window's rejection, so a rejection
  stands in the status slot until the user acts (the leftover from the
  timeline session).
- *Captures.* `scripts/capture-states.sh` puts the states side by side in
  `review/states-*.png` and `review/states-sheet.png`: ARCHIVED and LIVE
  from the shared daemon; OFFLINE from a scratch daemon in a network
  namespace over a copy of the cache, so the real poller fails and the
  cached frames show with their age; SILENT (UNAVAILABLE with nothing
  cached) from a scratch daemon on KCRI; STALE, UNAVAILABLE with cached
  frames, and LOADING through a harness copy of the shell that lays a
  `connection` over the live state, since those cannot be scheduled on the
  real feed.

**Loading and scanning view as built (2026-09-07, phase 4 session 5).** The
UX pass's view, with these choices:

- *Drawable is a scan time.* The map shows the view while the frame is the
  loading placeholder (`docs/protocol.md`, `frame.status`: empty `scanTime`,
  one blank row), which is exactly the first launch on an uncached station,
  a switch to one, and UNAVAILABLE or OFFLINE with nothing cached. The
  moment a chunk lands the frame is drawable and the sweep paints over the
  basemap, so the view never covers real radials.
- *What the view owns.* Tiles, place labels, site markers, the coverage
  circle, the site tag beside the crosshair, and the attribution wait for
  the first frame (`RadarMap.drawable`), and no tile is requested
  meanwhile; the range rings and crosshair stay at the camera's position,
  so nothing jumps when the frame arrives. The words sit under the
  crosshair: the site ID, `WAITING FOR FIRST SWEEP` in accent (yellow or
  red when the condition is STALE, UNAVAILABLE, or OFFLINE), and
  `REFLECTIVITY · NO RADIALS YET`. The header's time slot shows a dash
  instead of repeating the line, and its product slot drops the elevation,
  since the placeholder's 0.0° is no angle anyone measured.
- *The wedge* is a third fragment shader (`ui/shaders/sweep.frag`): the
  leading edge turns clockwise once per six seconds over the nominal 460 km,
  four bands behind it graded like the palette's quartiles and painted on
  the same 3 px cell grid in the treatment's style (solid steps, glyph
  densities, stipple squares), so the empty map already looks like the one
  the first frame fills. It stands still under UNAVAILABLE and OFFLINE: a
  turning antenna on a station the feed has nothing for would contradict
  the header. It is motion for the eye, not the antenna's rotation, and
  the animation runs only while the view is on screen.
- *The radial counter needs nothing from the engine.* A partial sweep's
  `rays` counts the blank row a gap adds, and a sweep in progress always
  has a gap (the radial that ends the cut completes it), so the header's
  status slot counts `rays − 1` as `SCANNING · 214 RADIALS` while a sweep
  paints. The canvas's `/ 720` has no honest source before the cut ends (a
  1° cut has 360), so there is no denominator. The counter cannot live in
  the view itself: the placeholder has no radials, and the first chunk that
  brings some also dismisses the view.
- *The translucency was Hyprland's.* The window is class `org.quickshell`
  and carries Omarchy's `default-opacity` tag, whose rule is
  `opacity 0.985 0.96` (`/usr/share/omarchy/default/hypr/windows.lua`; this
  machine's `looknfeel.lua` sets 0.98 and 0.96). The window colour and the
  surface are opaque theme background, and the offscreen captures show no
  wallpaper. The view makes the empty map read as deliberate; it does not,
  and must not, touch the rule. A phase 5 panel inherits whatever the
  shell's own rule says (`default/hypr/apps/omarchy-shell.lua`).
- *Captures.* `scripts/capture-loading.sh`: the three treatments and the
  frozen UNAVAILABLE and OFFLINE wedges through the harness shell
  (`scripts/capture-harness.sh`, shared with `capture-states.sh` and merging
  the override one level deep so `frame` alone can become the placeholder),
  then a scratch daemon with an empty cache on KJAX captured before and
  after its first replayed cut; `review/loading-sheet.png` puts them beside
  the canvas artboard.

**Site navigation as built (2026-09-07, phase 4 session 6).** The UX pass's
markers, with these choices:

- *The engine judges nearest, by great-circle distance to the view centre*,
  on the radar's 6371 km sphere. The window sends `view_center` when a pan
  or zoom settles and the centre moved (the same 120 ms settle as the tile
  request); the engine hands off only while `follow` is on and `lock` is
  off, and only when the nearest station beats the current one by the
  hysteresis rule: closer than 0.8 of the current station's distance and by
  at least a kilometre. Relative, so the dead band scales with the spacing
  (about 11 km either side of the midpoint between stations 200 km apart, 2
  km for the Norman pair 20 km from KTLX); the kilometre keeps KOUN and KCRI,
  300 m apart, from swapping under a centre near both. A hand-off is a
  `select_site`, so an uncached station opens on the loading view and its
  cut is replayed from the bucket.
- *A switch does nothing to the camera.* The centre is the user's. The span
  is measured at the site's latitude, so the map rescales it as the site
  changes to keep the ground scale on screen exactly where it was; a
  hand-off changes the crosshair, the tag, the footprint, and the header,
  and nothing moves. `n` is the exception by design: it takes the station
  nearest the centre, releases the lock, and puts the camera on that
  station's home view (the same offset RESET uses), computed from the table
  before the frame arrives so the camera lands once. The configured home
  station is selected the same way.
- *The lock.* Shift+L (lowercase `l` is the pan key to come) toggles
  `lock`; locked, the marker and tag carry an accent 1 px frame, the chip
  after the site name reads LOCKED in accent (FOLLOWING at foreground .55
  while following; nothing when following is off with no lock), and the
  lock control leading the controls row is selected. Releasing the lock
  hands off on the next settle, not at once, so a release under a far-off
  centre does not yank the station; `n` is the immediate form.
- *Configuration.* `~/.config/omastorm/
  config.toml` with `home_site` and `follow` (docs/protocol.md,
  configuration), watched like the theme files, parsed by the same scalar
  TOML reader (`ui/Toml.js`); `OMASTORM_CONFIG` names another file, which
  the capture scripts point at scratch configs. A window applies the file when state first arrives and
  after a reconnect, moving the camera to the home station's view first so
  the settle that follows reports that centre and not the fixture's.
- *Glyphs.* The lock and follow glyphs join the transport set on the 16 px
  canvas grid (`Glyph`, `GlyphButton` in `ui/shell.qml`), drawn rather than
  typed.
- *Open, for the milestone 2 review.* The table includes test and
  decommissioned stations, so following around Norman lands on KOUN or KCRI
  (UNAVAILABLE) rather than KTLX. Whether following should skip stations
  the feed has answered nothing for, or the picker should mark them, is a
  judgement for daily use.
- *Captures.* `scripts/capture-handoff.sh`: one pan from Oklahoma City to
  Jacksonville in six windows on the shared daemon, each started at a point
  along the pan through `OMASTORM_VIEW` so its settle hands off for real,
  following and then locked on KTLX; `review/handoff-sheet.png` puts the two
  rows together.

**Site picker as built (2026-09-07, phase 4 session 7).** The UX pass's
picker, with these choices:

- *Its own file.* `ui/SitePicker.qml` lies over the whole window from
  `shell.qml`, and the matching is pure functions in `ui/Sites.js`, so the
  check drives the ranking through the real window and `shell.qml` does not
  grow by a card. The popover has no search by decision, so reuse there was
  not the reason. The card is 520 px, or the window less 40, centred, its top
  on the map's top edge.
- *Ranking in tiers, distance within a tier.* 0: the query starts the ID,
  with or without its leading letter (`tlx` finds KTLX). 1: it starts a word
  of the city. 2: it is the state's postal abbreviation or starts a word of
  its name. 3: it appears anywhere in the ID, city, or state. 4: its letters
  appear in order across the ID and place (`tul` reaches ALTUS AFB,
  OKLAHOMA). Ties go to the great-circle distance from the map centre, then
  the ID. Four rows; the footer counts the rows shown of the stations
  matching; an empty query lists the four nearest of the 163.
- *Rows.* The ID, the table's city with the state spelled out from a postal
  table in `Sites.js` (the four overseas sites have none), and the distance
  and eight-point bearing from the map centre, with `home ·` ahead of it for
  the configured home. Matched letters are accent; the selected row has the
  foreground fill at .08 and accent text, so its hits merge into it, as on
  the canvas.
- *Keys.* `/` or `s` opens, as does the SEARCH control leading the controls
  row (glyph alone in compact). A `TextInput` holds the query with the 7 px
  block caret on the insertion point; typing filters; ↑ ↓ move and stop at the
  ends; Enter selects the station, locks it unless already locked, and puts
  the camera on its home view the way `n` does; Escape, or a click on the
  scrim, closes. Backspace, Ctrl+Backspace, and Ctrl+U edit as in the
  launcher. While the picker is open the field holds focus and every window
  shortcut stands down.
- *Checks and captures.* An IPC handler (`quickshell ipc call picker open |
  accept | close | move | matches | status`) drives the picker from outside.
  `scripts/check-picker.sh` runs it in the real window against the fixture
  daemon, last in `check.sh` since Enter selects a station for real;
  `scripts/capture-picker.sh` captures it open over live KTLX and the state
  after Enter (`review/picker-sheet.png`).
- *Open, for the milestone 2 review.* Rows do not mark stations the feed
  answers nothing for (site navigation above).

**Keyboard map as built (2026-09-07, phase 4 session 8).** The proposed
bindings (picker and keyboard, below) as the KeyboardHints canvas drew them,
with the treatment chip and the current-location home from the same
session. Decided while building, within what the plan allowed:

- *Bindings.* `/` `s` search, `n` nearest, `Shift+L` lock (lowercase `l`
  pans), `h j k l` and the arrows pan an eighth of the viewport's shorter
  side, `+` `=` and `-` zoom by the buttons' 1.25, `0` reset, `[` `]` step,
  Space play, Home End jump, `1 2 3` treatment, `w` the weak-return floor
  (added 2026-09-07; weak-return floor, above), `?` the sheet, Escape with
  nothing open closes the window (the UX path). Checked 2026-09-07 against
  the installed Omarchy bindings (`omarchy menu keybindings --print`):
  every Omarchy default carries SUPER, and the only bare keys on this
  machine are PRINT and F9, so nothing collides; Qt reports Shift+L and l
  as distinct keys, and `?` does not fire `/`.
- *Rebinding.* `[keys]` in config.toml, one Qt key sequence string per
  action, several separated by spaces, `""` to unbind (docs/protocol.md,
  configuration). `ui/Keys.js` lays the table over the defaults as pure
  functions; the window supplies Qt's own parser through a disabled probe
  `Shortcut`, whose empty `portableText` means a sequence is not a key. A
  mistake of any kind, including a key another action already holds, keeps
  that action's default and stands in the status slot in accent like a
  rejection, first mistake plus a count, until the file is fixed; the same
  for a `treatment` outside the three. One `Shortcut` per action through an
  `Instantiator` carries the sequences in force; all stand down while the
  picker or the sheet holds the keyboard.
- *The `?` sheet.* `ui/KeysSheet.qml`, over the window like the picker: a
  660 px card below the map's top edge, KEYS, "rebindable in
  ~/.config/omastorm/config.toml", two columns of key caps built from the
  bindings in force (a row of several actions shows each one's first key
  and names the alternates, "arrows too"), and a footer saying the global
  key is Hyprland's and which key closes the window. `?`, Escape, or the
  scrim closes it. The `?` chip with the keyboard glyph sits in the map's
  top-right corner at .7, as drawn.
- *The treatment chip.* The three buttons are gone; one chip at .7 names the
  treatment before a 1 px separator, `−`, `+`, and RESET, right-aligned
  after the lock. Click opens a menu (not a cycle: the canvas drew a
  chevron, and three choices are quicker to see than to step through): a
  168 px `Popup` above the chip in the picker's row style, the current
  treatment and the hovered row in accent, each row's key at the right,
  Escape or a click outside closes, `1 2 3` choose and close it. Glyphs
  stays the default; `treatment` in config.toml sets the launch value.
- *The current-location home.* `Config.qml` reads Omarchy's weather.json
  beside config.toml with the same `FileView`, so a change from the weather
  panel re-homes the open window. `home_site` wins; otherwise the station
  nearest the location (KFCX for Stokesdale, 91 km) is selected the way
  `home_site` is, and the picker marks it `home`. The header's second row
  says `HOME · NEAR STOKESDALE` or `HOME · CONFIG.TOML` after the site chip
  while the home station is shown, so the source is named without a new
  row. A file with no coordinates is no location. `OMASTORM_LOCATION` names
  another file; with `OMASTORM_CONFIG` set the machine's file is left alone
  unless it does, since a check that isolates its configuration is
  isolating this too.
- *Checks and captures.* `quickshell ipc call keys run <action> | bindings |
  errors | menu | field | status` drives the map from outside.
  `scripts/check-keys.sh` runs in the real window against the fixture
  daemon with a config carrying every kind of mistake, each action's
  effect, the sheet, the menu, the picker, the live fix through the file
  watch, and the location home from a Stokesdale weather.json; it is last
  in `check.sh` since it selects stations. `scripts/capture-keys.sh`
  captures the sheet and the menu over live KTLX and the header with the
  location home (`review/keys-sheet.png`).

**Independent answer key.** pyart, a battle-tested Python decoder, was run once
on 2026-09-05 to produce golden files under `golden/` that the Rust decoder's
tests compare against exactly. Python is not a project dependency; a future
fixture is produced the same way and committed alongside. The milestone 1
Python build scripts are deleted as the phases that replace them land.

**Radar path: polar.** The engine emits each sweep as a polar texture (rays ×
gates, palette class and status bytes) plus an azimuth lookup table. The fragment
shader performs the polar-to-screen lookup per pixel with nearest sampling. This
replaces the 500 m Cartesian grid, preserves native 250 m × 0.5° resolution at
every zoom, and lets partial sweeps draw radial by radial as live chunks arrive.
The 3 px screen-cell sampling and the treatments sit on top, unchanged. The
shader converts each cell's ground distance to slant range on the 4/3
effective-radius earth in closed form, the inverse of pyart's
`antenna_to_cartesian`, so gates land where the golden reference places them
(a flat conversion would be half a gate off at 230 km); it takes the nearest
gate, and more than half a gate before the first or past the last draws
nothing. The map frame is defined once as shader uniforms (below, map frame),
shared with the tile layer and the overlay. Sweep geometry (`rays`, `gates`,
`firstGateM`, `gateSpacingM`, `elevationDeg`) also travels as uniforms; it is
geometry, not radar values.

**Map frame: the whole network.** The map is a Web Mercator frame covering every
NEXRAD site (about 160 across the lower 48, Alaska, Hawaii, Puerto Rico, Guam,
and overseas bases), not a square around one site. The polar path is unaffected:
the shader maps pixel to latitude/longitude to site-relative range and azimuth,
and each sweep still draws at native resolution in its own geometry.

Landed 2026-09-06 (phase 3 session 3). `ui/RadarMap.qml` computes the camera
in double: the unit square is the world (`x = (lon + 180) / 360`,
`y = (1 − asinh(tan φ) / π) / 2`), and the shader receives `viewport`,
`centerOffset` (the clamped view centre minus the site, in Mercator units),
`unitsPerPixel`, and `siteLatDeg`. The offset form matters: a pixel's Mercator
position relative to the site is a small number with full float precision,
where an absolute Mercator coordinate would carry about a metre of float
error and, worse, the bearing would come from subtracting two large
latitudes. So the shader never forms an absolute latitude difference either:
with `ψ` the isometric latitude (`ψ = ψ₀ − 2π·Δy`), it takes
`Δφ = atan(2 cosh(ψ₀ + Δψ/2) sinh(Δψ/2) / (1 + tan φ₀ sinh(ψ₀ + Δψ)))`, then the
haversine distance `2R·atan2(√h, √(1−h))` and the bearing
`atan2(sin Δλ cos φ, sin Δφ + sin φ₀ cos φ · 2 sin²(Δλ/2))`, every term a small
difference or a smooth function of one. `asin` was avoided for the distance
because an absolute error of 1e-6 rad in it is 13 m of ground; `atan` had
already proved accurate in the phase 2 azimuth. Hyperbolics are spelled with
`exp` so the GLSL ES 1.00 target qsb emits still compiles. Ground distance is
on a sphere of radius 6,371 km (the base of the 4/3 beam radius); the
ellipsoid would move a gate at 230 km by about half a kilometre, which is
left for a later pass if it ever shows. The rendering test replays this in
single precision and passed with the same epsilons as the flat frame. `span`
is still ground kilometres across the shorter side, measured at the site's
latitude, and range rings are circles at that scale, which over 200 km hides
a Mercator scale drift of about two percent.

**Geography: mask tiles.** A standard tile pyramid. Low zoom levels are
rasterized from Natural Earth and ship with the binary; high zoom levels are
rasterized lazily from OpenStreetMap around used sites and cached on disk. Tiles
are antialiased masks, one channel per layer, tinted with theme colors in a
shader. Only visible tiles are resident; zoom swaps levels and keeps the previous
level until the new one loads. Labels, rings, site markers, and coverage circles
are camera-translated QML items. Collision layout uses site-relative pixels
across the fixture and runs only on zoom, resize, theme, or data changes; pan
updates the parent translation and viewport visibility without relayout. The UI never parses vector data.

**Basemap tiles (decided 2026-09-06, phase 3 design pass).** Every claim here
was checked against the library's source or the service's published metadata
on 2026-09-06; versions are the ones current on crates.io that day. Tile
geometry is the standard Web Mercator XYZ pyramid at 512 px per tile: `2^z`
tiles across, `x = (lon + 180) / 360 · 2^z`,
`y = (1 − ln(tan φ + sec φ) / π) / 2 · 2^z`; a 512 px tile covers the same
ground as the 256 px tile of the same `z/x/y`, at twice the density.

*Rasterizer: `tiny-skia` 0.12.0.* Now `linebender/tiny-skia`; released
2026-02-02, repository pushed 2026-08-20, 45.6 M downloads, the rasterizer
under resvg. Verified in source: `Mask::new(w, h)` is an 8-bit coverage plane
with `fill_path(&path, FillRule, anti_alias, Transform)` and `data() -> &[u8]`,
and `PathStroker::stroke(&path, &Stroke { width, line_cap, line_join, .. },
resolution_scale)` turns a stroke into a fillable outline
(`path/src/stroker.rs`). Each layer of a tile is therefore one `Mask` filled
with antialiased outlines, and the four masks interleave into the RGBA PNG the
protocol describes; there is no premultiplied color pass and no channel
extraction. Depend on it with `default-features = false, features = ["std",
"simd"]`, since its optional `png-format` would duplicate the `png` crate the
engine already uses. `raqote` 0.8.5 was rejected: last release 2024-09-11,
repository last pushed 2025-02-11, 0.7 M downloads, and it renders
premultiplied ARGB32 only, so a per-layer mask would need a color pass and a
channel split. `resvg` is not needed; nothing here is SVG.

*Shipped geography: Natural Earth at two scales, converted by a build script.*
Themes: coastline, lakes (shorelines only), country boundary lines on land,
and state and province lines, all public domain, at 1:50m for the whole world
and 1:10m for a network envelope (5–75° N, west of 20° W or east of 120° E),
which holds every site in the table, including Lajes, Guam, Kunsan, and
Kadena, with its 460 km coverage. Plus `ne_10m_populated_places_simple`
(already downloaded) for low-zoom labels. Measured 2026-09-06 from upstream
master: the 1:10m line files are 36.7 MB of GeoJSON and 1.07 M vertices
worldwide; the envelope keeps 324 k vertices, 1.24 MB as zigzag varint deltas
of coordinates quantized to 1e-5° (1.1 m, far inside the data's accuracy); the
1:50m world set is 116 k vertices, about 0.5 MB the same way; places about
0.2 MB. `scripts/setup-fixture.sh` downloads and checksums the files into
`data/raw/` beside the volume, and `engine/build.rs` (serde_json as a build
dependency) clips, quantizes, and writes one polyline blob into `OUT_DIR` for
`include_bytes!`: about +2 MB of binary and about a second of build time,
rerun only when an input changes. Committed rasters were rejected: the
envelope holds roughly 1,400 512 px tiles at z0–6, a 5–10 MB set that changes
whenever a stroke width or the rasterizer does, and `ne` would run on a
different code path from `osm`. A committed derived blob was rejected as a
second copy of data the checkout already downloads and checksums. `ne` tiles
are rasterized lazily at runtime by the same code as `osm` tiles, at any
zoom: z0–4 from the 1:50m set, z5 and up from 1:10m. So the network view
always has geography offline, and a site with no OSM data cached gets crisp
coastlines and boundaries rather than a scaled-up tile. Lakes are stroked, not
filled: quiet geography, the same rule as coastlines and rivers today.
Built 2026-09-06 (`engine/build.rs`, `engine/src/tiles.rs`); the polyline
blob came out at 1.8 MB for both sets and the places JSON at 0.65 MB. Reviewed on the tile captures
2026-09-06 and accepted as the starting point, to be re-judged once the UI
tints them with theme colors: stroke widths of 1.5 px for boundaries and
1.25 px for coasts and lake shores with round caps and joins (two constants
in `engine/src/tiles.rs`; masks are redrawn each session, so changing them
costs a rebuild and nothing else). Still proposals: place labels classed
`capital`, `city` (100 k+), `town` (10 k+), or `village` from Natural Earth's
capital flag and `pop_max`, ranked by `scalerank`, and carried by a tile when
the data's `min_zoom` is at most `z + 1`; the overlay session settles them.

*Detail geography: OpenMapTiles-schema vector tiles, one HTTPS GET per tile,
OpenFreeMap by default.* Runtime Overpass was rejected on its own numbers: the
public instance's usage policy (OSM wiki, read 2026-09-06) allows an
application fewer than 100 queries and 10 MB per day summed over all of its
users, the one-site extract behind today's `basemap.json` is 58 MB of Overpass
output, and the operators describe the server as overloaded and ask for
alternatives. Prebuilt extracts (Geofabrik) are hundreds of MB per state and
need PBF filtering the engine would otherwise never do. Vector tiles are the
shape of the problem: generalized per zoom, immutable per data version,
fetched only for the tiles a viewport asks for. OpenFreeMap, verified
2026-09-06: TileJSON at `https://tiles.openfreemap.org/planet` names the
current data version (`20260830_080001_pt`) and the URL template; tiles z0–14,
`Cache-Control: public, max-age=315360000`, plain protobuf unless gzip is
requested; measured near KTLX, 4.5 kB at z7, 21 kB at z11, 12 kB at z12, 57 B
for an empty tile. Terms on openfreemap.org: free public instance, no
registration or key, "no limits on the number of map views or requests",
commercial use allowed, no SLA, donation-funded by one maintainer;
attribution required, "OpenFreeMap © OpenMapTiles Data from OpenStreetMap"
(the OpenFreeMap part optional). Layers used, all verified present in a KTLX
tile: `boundary` (`admin_level` 2 and 4, `maritime` 0), `water` and
`waterway` (shorelines and rivers, stroked), `transportation` (class
motorway, trunk, primary as major roads; secondary, tertiary as minor), and
`place` (city, town, village; `name:en` preferred as today) for labels. The
URL template is configuration (`tiles_url` in phase 4's `config.toml`), so a
self-hosted OpenFreeMap or any OpenMapTiles-schema server can stand in; the
engine sends `User-Agent: omastorm/<version> (https://omastorm.com)` as OSM
services ask, times a fetch out at 10 s, keeps at most four in flight, and
backs off 30 s on 429 or 5xx. HTTP is `reqwest` 0.13 with rustls, the major
`nexrad-data` rc.7 declares itself (`reqwest = "0.13.1"`, `tokio = "1.47.1"`
in its manifest), so phase 4 adds no second HTTP stack. MVT decoding is
`mvt-reader` 2.5.0 with default features off (prost-based, released
2026-09-06; dependencies geo-types, prost, thiserror) rather than hand-rolled
protobuf. The 4096-unit tile extent maps onto 512 px at 1/8.

*Zoom split and fallback.* `osm` starts at z7 (about 500 m/px at 35° N, where
1:10m's kilometre vertex spacing begins to show and OpenMapTiles carries
boundaries, water, and major roads). A tile request names no set: the engine
answers each tile with `osm` when the vector tile is cached or fetched and
`ne` otherwise, and announces the tile again under a new path when `osm`
later becomes available, so the UI has no fallback policy of its own. Offline,
the picture degrades to Natural Earth lines without roads, never to a scaled
tile or to nothing.

*Cache: `$XDG_CACHE_HOME/omastorm/` (default `~/.cache/omastorm/`).*
`vt/<source>/<version>/<z>/<x>/<y>.pbf` holds fetched vector tiles, immutable
(the server's own ten-year max-age); empty tiles are stored too so the sea is
not refetched. `<source>` is a hash of the URL template; `<version>` comes
from TileJSON, fetched lazily on the first tile request of a daemon's life
(launch fetches nothing) and otherwise the newest directory present; the two
newest versions are kept. Ceiling: 512 MB, `cache_mb` in phase 4's config.
Eviction is least-recent use with a clock we control: the daemon bumps a
tile's mtime when it reads it, so `relatime` cannot mislead, and after a fetch
batch or at startup, when the total exceeds the ceiling, deletes the
oldest-touched files until it is under 448 MB. Sizing: a site's full z7–z12
pyramid at 35° N is about 4,500 tiles (4 + 16 + 64 + 225 + 841 + 3,364),
about 60 MB at the measured sizes, so the ceiling holds eight fully explored
sites and far more ordinary viewport-sized use. Rendered masks are not cached
on disk. They live in `$XDG_RUNTIME_DIR/omastorm/tiles/<set>/<z>/<x>/<y>-<gen>.png`
(tmpfs, per session), where `<gen>` is eight hex digits over the build
fingerprint and the source version, so pixels never change under a name Qt has
cached; a daemon deletes other generations at startup and caps the directory
at 4,096 files by dropping the oldest announced (a client that finds a file
gone asks again). A mask depends only on data version and rasterizer build
and costs milliseconds to redraw, so persisting it would spend disk on the
cheap half of the pipeline; the cache holds only what the network gave us.
This machine's runtime tmpfs is 987 MB; 4,096 masks at the measured 5–40 kB
stay under 160 MB in the worst case.

*Premultiplication (decided 2026-09-06, phase 3 session 3).* Probed before
the tile shader with a one-file Quickshell harness that sampled a published
`ne` mask through `Image` and a pass-through `ShaderEffect`: a texel with
R 255 and A 0 arrived as all zeros, and the same PNG with A forced to 255
arrived intact. Qt Quick converts every image with an alpha channel to a
premultiplied format on upload and QML has no switch for it; the only
UI-side escapes are a three-channel PNG (one layer short) or a C++ image
provider the thin QML client does not have. So the protocol changed instead:
A is `255 − major-road coverage`, an empty tile is opaque, and the tile
shader (`ui/shaders/tile.frag`) divides R, G, and B by A to recover the
straight masks and reads major roads as `1 − A`. Where a major road covers a
texel fully the other layers are lost under it, which is also what drawing
the road on top would show. `ne` tiles have A 255 everywhere. The recovery
of partial alpha (roads) was checked when `osm` tiles landed (2026-09-06):
road edges with intermediate A draw as soft edges in the theme tint, with no
dark fringe, in the seven-size capture review.

*`osm` as built (2026-09-06, phase 3 session 4).* Choices made while
building, each a refinement of the above rather than a change: `<source>` in
the cache path is a tag of the configured TileJSON URL, the one address the
configuration names, rather than of the template it yields; the data version
is the template's path segment before `{z}` (`20260830_080001_pt`), which
TileJSON's own `version` field (`3.16.0`, the schema) is not; `osm.source`
and `osm.attribution` come from TileJSON's `name` and `attribution` (HTML
reduced to text, which yields exactly the wording above) once read, so a
self-hosted server credits itself; tiles past z14, OpenFreeMap's deepest,
fall back to `ne` rather than overzooming, since the camera cannot reach
them this phase; roads are stroked at 1.5 px (major) and 1.0 px (minor),
accepted with the `ne` widths as the starting point on the seven-size
capture review (Wes, 2026-09-06, `review/quarter.png`). Measured on the
live service: the z7 tile holding KTLX is 132 kB and the z11 tile 21 kB, so
the 4.5 kB z7 figure above was a sparse tile and a site's full pyramid is
nearer 100 MB than 60; the ceiling still holds several explored sites.
TLS is `reqwest` 0.13's `rustls` feature (aws-lc-rs, platform verifier),
the same feature `nexrad-data` rc.7 declares, so phase 4 adds nothing.

*UI tile layer (landed 2026-09-06, phase 3 session 3).* The map picks the
zoom whose 512 px tiles come nearest 1:1 on screen,
`round(log2(worldPixels / 512))`, so tiles draw between 0.71× and 1.41×, and
steps out a level while the visible rectangle would exceed the protocol's 64
tiles (a 4K window at 0.71× would). A 120 ms settle timer after the last
camera or size change sends `tiles_needed` for the visible rectangle when it
differs from the last request, or unconditionally after a state change,
since a restarted daemon publishes under a new generation. Every announced
tile inside the last request is a `ShaderEffect` over an `Image` of the
mask, placed by the camera each frame with edges rounded to whole pixels so
neighbours meet without seams; the delegates live in a `ListModel` keyed by
tile so a pan or a re-announcement never recreates an image already on
screen. Tints as a starting point, to be judged on captures with the stroke
widths: boundaries foreground at 0.32, water accent at 0.55, minor roads
foreground at 0.24, major roads foreground at 0.48.

Level retention and labels landed in session 5 (2026-09-06): keep the visible
part of the displayed level while asynchronously loading the requested level,
then swap only when every image in the rectangle is Ready. The swap is atomic
rather than painting both levels together, since translucent line masks would
double their weight. Labels and attribution follow the displayed level. Prune
metadata and delegates to the visible displayed/requested rectangles, reject
superseded replies, and disable Qt's image cache so discarded masks do not
remain resident. A rapid reversal keeps the displayed level and drops the
abandoned request. Newly exposed ground on a zoom-out or pan still needs its
own tiles. Labels sort by ascending source rank, then class, name, and position;
duplicate name/coordinate pairs collapse before the existing collision pass.
Natural Earth's current classing remains the starting point; the overlay
review can refine it. Site overlays and the free camera follow below.

*`tile_ready`.* A reply to the requesting client only, like `error`, not
shared state: one message per tile, sent at once for a tile already rendered
this session and otherwise when it is drawn; a tile the engine cannot produce
is not answered, and the lasting condition shows in state. It carries `set`,
`z`, `x`, `y`, `path`, and `labels`, the tile's places (`name`, `lat`, `lon`,
`class`, `rank`) from OpenMapTiles `place` or Natural Earth populated places,
which the overlay projects and lays out as it does today. Paths follow one
rule, `tiles/<set>/<z>/<x>/<file>`, checked at both ends like texture paths.
`state.basemap` gains `ne.version` and `osm.status` (`ok`, `offline`,
`unavailable`), `osm.source`, `osm.version`, and `osm.attribution`; the UI
shows the attribution string verbatim whenever an `osm` tile is on screen.
`tiles_needed` names an inclusive rectangle of at most 64 tiles at one zoom,
served centre-out; a newer request from the same client supersedes its pending
tiles outside the new rectangle. `docs/protocol.md` carries the wire shapes.

**Map component.** `ui/RadarMap.qml` is the one map: camera, radar shader,
palette strip, tile layer, overlay, and pointer handling. Every surface (the
window now; popover and tiles in phase 5) places an instance, feeds it `scan`,
`texture`, `azimuthLut`, `siteId`, `sites` (the hello table), `tileRoot`, `theme`, `treatment`, and
`labelSize`, and drives the camera through `center` (a longitude/latitude
point, or null for the home view), `span`, `maxSpan`, `reset()`, `zoom()`, and
`look(mx, my)`. Tiles flow through the surface, not a socket the map knows
about: the map raises `tilesNeeded(z, x0, y0, x1, y1)` when its camera
settles and the surface calls `tileReady(tile)` with each reply (2026-09-06).
The component draws no chrome and holds no station or product strings; corner
annotations, the legend, and controls belong to the surface. Its camera exposes
`viewCenterX` and `viewCenterY` (Mercator units), `worldPixels`,
`unitsPerPixel`, and `pixelsPerKm` read-only, and `sx()`/`sy()` from Mercator
units to pixels, for overlays that must stay in the same frame as the shader.

**Place labels.** The current English interface prefers OSM `name:en`, with
`name` as fallback. The source multilingual name stays in the committed map
database; display selection does not strip scripts or rewrite source names.

**Site overlay (phase 3 session 6, 2026-09-06).** Informational hollow 6 px
squares and muted station IDs distinguish table locations from the active
crosshair. The table includes archived/test sites; no marker implies live
availability. Station IDs take priority in collision layout, including
co-located stations, and the active tag uses measured text width. Nominal
460 km reflectivity footprints use 1 px dashed foreground strokes at 0.12
alpha; the surface labels them nominal (approximately 460 km in compact
layouts). These are geographic reference footprints, not measured coverage or
a promise of low-altitude visibility. The existing 50 km range rings stay.

Continental review decision (2026-09-06, Wes): draw the nominal footprint
only for the active radar. Every station keeps its marker, but overlapping
coverage circles across the network make the zoomed-out view too busy.

Footprints project great-circle destinations on the same 6,371 km sphere as
the radar; the active footprint uses the frame's measured site position in
preference to today's table. Longitudes unwrap about the station. Qt Quick
Shapes draw clipped vector paths, with no full-extent raster allocation;
clipping and station label candidates use the padded overlay viewport below.
Coverage paths stay fixed through pan and zoom; a scene-graph scale and
inverse stroke width preserve a 1 px line. Pan only translates the overlay.

**Free camera (phase 3 session 7, 2026-09-06).** Zoom limits chosen from the
network layout captures (`scripts/check-map-network.sh`): the span runs from
25 km across the shorter viewport side, measured at the scan-site latitude,
to one world across the longer side, so the smallest window still shows the
whole network and the largest still resolves a storm cell. The view centre is
clamped so the viewport stays inside the tile pyramid on both axes, the same
rule for longitude as for latitude. The map does not wrap at the date line:
every NEXRAD station including Guam fits one Mercator world, a
Pacific-centred view has no radar use, and staying inside the pyramid keeps
`tiles_needed` a single canonical rectangle. Should a wrapping view ever be
wanted, the extension is `x0 > x1` meaning a date-line-crossing rectangle;
that is a protocol change to decide first, not a UI-only tweak.

Overlays lay out inside a viewport padded by 256 px on each side and keep
that geometry until the camera drifts 128 px, doubles its zoom, or the
padding would fall short of the viewport; the rebuild is then one layout
pass, and every pan between rebuilds only translates the scene. Far from the
site the radar shader's bearing and latitude use a bounded atan2 series
(argument reduced to tan(π/8), next term under 1.9e-8 rad), because some GPU
atan implementations moved a bearing across a 0.1° lookup boundary; the
rendering test replays the series and checks it against libm in every
quadrant. A cell whose ground arc plus elevation reaches 90° draws nothing.

**Site model: single active site, free map, auto-follow.** One site's exact
sweep is drawn at a time. The active site is the one picked, or by default the
nearest site to the map center, so panning hands off from radar to radar. A lock
pins a site. A configured home site gives the default view; without one, the
current location picks it: the station nearest Omarchy's weather location
(`~/.local/state/omarchy/settings/weather.json`, a local file the shell's
weather panel writes), so no location service and no fetch at launch (decided
2026-09-07; phase 4). A blended mosaic is
deferred; when wanted it is built by drawing the sites in view and compositing
by maximum reflectivity from exact sweeps, not by downloading a derived grid.

**Picker and keyboard.** The site picker follows the Omarchy launcher pattern: a
fuzzy search opened by a keystroke, matching site ID, city, and state, Enter to
select, Escape to close. Everything reachable by pointer is reachable by
keyboard. Proposed bindings, all rebindable: `/` or `s` search, `h j k l` or
arrows pan, `+ -` zoom, `n` nearest site, `L` lock, `[ ]` step frames, `space`
play/pause, `1 2 3` treatment, `0` reset, `Esc` close. Confirm against the
installed Omarchy keybinding conventions before finalizing.

**Theme.** Quickshell `FileView` watches the Omarchy theme files, and a script in
`~/.config/omarchy/hooks/theme-set.d/` signals explicit theme switches. No timer,
no polling process. Qt resolves the generic monospace family through fontconfig.
The hook ships in `scripts/hooks/omastorm` for optional user installation; the
app never writes Omarchy configuration. Theme and user shell inputs are watched
separately, with user overrides taking precedence and missing keys using defaults.

**Plugin identity (decided 2026-09-05).** The Omarchy plugin ID is
`com.omastorm.radar`, reverse-domain from omastorm.com, which Wes owns. IDs on
the community marketplace are globally unique and permanent, so this is fixed
before phase 5 packaging and must not change. Display name: Omastorm. The
third segment is deliberate: it keeps `com.omastorm.*` free for sibling
plugins under the same domain.

**Distribution (decided 2026-09-06).** One public MIT repository,
`wesleygrimes/omastorm`, is both the source tree and the plugin.
`omarchy plugin add` is a plain full `git clone` of the default branch into
`~/.config/omarchy/plugins/<id>`, validated by `manifest.json` at the clone
root; it accepts no subpath, runs no install hook, and updates by fast-forward
to the branch head. Users have no Rust toolchain, so the engine cannot be
built at install time and is not committed either: the marketplace scanner
flags any committed ELF for manual review and flags download-then-execute
paths that skip verification. The engine therefore travels as a GitHub Release
asset on the same repo, and the plugin pins one release tag and its sha256 in
a committed file. A script in the repo fetches the asset for the running
architecture, verifies it, installs it under `~/.local/share/omastorm/bin`,
and runs `ensure`, which already replaces a stale daemon by self-hash. The UI
invokes that script on launch when the binary is missing or its version
disagrees with the pin. Pinning, not "latest", because the UI and engine
share a protocol and must not drift. Every push to main reaches users on
their next plugin update, so the pin is bumped only after the release it
names exists. Weighed and set aside: a closed-source engine (the marketplace
is about 92% MIT with a handful of source-available listings and no closed
binaries; a private repo's release assets are also not anonymously
downloadable), a separate plugin repo published by `git subtree`, and a
GitHub org (`omastorm` was free on 2026-09-06; the marketplace binds a listing
to its repo URL, so any move happens before the phase 5 listing). Reference
plugin: Omamail, which ships one repo from main and fetches its one external
binary through `omarchy-mise-install`.

**Summoning the window (decided 2026-09-05).** Omarchy plugins are loaded and
shown by the shell, so the window is opened with
`omarchy shell shell toggle com.omastorm.radar '{}'`; the target is `shell`,
not the plugin ID, because a plugin-scoped IPC target only exists once the
window is already open. Two integrations build on that command and follow the
Omarchy convention that a plugin never edits Hyprland, shell, or theme
configuration: a `.desktop` file in `~/.local/share/applications/` so the app
launcher lists Omastorm, written only by an explicit opt-in script and
documented for removal (the pattern Omamail uses for `mailto:`), and a
documented one-line `~/.config/hypr/bindings.lua` entry using `o.bind` for a
global shortcut, which the user adds. The in-app keyboard map above is separate
from that global key. The manifest kinds are expected to be `bar-widget` plus
`panel`, matching the two entry points in the layout direction above; the phase
4 UX design pass confirms this before phase 5 packaging. Checked 2026-09-06
against the installed `/usr/share/omarchy/shell/shell.qml`: a plugin whose
`kinds` carry both is mounted in the bar by the widget registry and also gets
a panel `Loader`, and `summon`/`toggle` route to the panel loader for any
plugin that has a `panel` kind (`isBarWidgetPanelPlugin` returns false for
it), so the shell toggle command opens the window and never the bar widget's
popover. Decided with the UX review the same day: `kinds` are `bar-widget`
and `panel`.

**Frame storage.** A per-station ring buffer: SQLite catalog of station, time,
elevation, product, and provenance, with texture blobs on disk. Storage is a
catalog, not a transport; the UI never reads it. Landed 2026-09-06
(`engine/src/catalog.rs`; live sweeps as built, above).

**Products.** From phase 2 onward a product is a texture plus legend, units,
timestamp, and source, regardless of origin. Three origins share the path:

- Raw Level II moments: reflectivity, velocity, spectrum width, dual-pol fields.
- Volume math in the engine: composite reflectivity, echo tops, vertically
  integrated liquid, storm-relative velocity, higher tilts.
- Level III downloads for NWS algorithm output that should not be reinvented:
  storm tracks, mesocyclone and tornado signatures, hail, precipitation totals.
  These arrive as overlays alongside warnings and reports.

Level II is the source of truth for what is drawn. Level III is a future overlay
feed, not a replacement for the radar layer.

## Data references

- [NOAA Level II data description](https://www.roc.noaa.gov/level-two-data-types.php)
  explains the measurements and associated metadata.
- [NEXRAD on AWS](https://registry.opendata.aws/noaa-nexrad/) documents public
  archive and real-time access. Verify current bucket names when implementing.
- [nexrad crates](https://github.com/danielway/nexrad): decode, model, and AWS
  archive plus real-time download for Rust.
- [Iowa Environmental Mesonet local storm reports](https://mesonet3.agron.iastate.edu/lsr/)
  describes GeoJSON services for NWS reports.
- mPING access, terms, and integration remain unverified.

## Open choices for iteration

- Which radar palette fits the theme while preserving intensity readability?
- The weak-return floor's default and shape (weak-return floor, above): 5 dBZ
  hidden with `w` to show all is built; the value, dimming instead of hiding,
  and a per-VCP default wait for live evenings in both scan modes. A
  correlation-coefficient filter belongs with milestone 3's dual-pol products
  (PLAN.md, phase 5 review).
- Keep or drop stipple after watching live storms (milestone 2 review).
- Glyph masks as density patterns or as the font's actual shade glyphs?
- Zoom-out limit and how coverage circles and site markers read at continental
  scale without clutter.
- Whether a multi-site composite is worth its bandwidth (milestone 3).

Resolve these with a working sample and user review. Implementation progress
and the next concrete task live in the local PLAN.md, which is not in the
repository.
