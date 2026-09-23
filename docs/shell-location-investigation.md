# Omastorm location, red-dot, and idle-lock investigation

Status: investigation notes from September 23, 2026. No fix is included here.

## Report

After updating Omastorm, a user reported that:

- the bar widget appeared to lose its manually chosen location a few minutes
  after the expanded radar window was closed;
- the bar icon showed a red dot;
- reopening the window appeared to restore the radar temporarily; and
- a second tester observed that the Omarchy shell was not entering its
  screensaver or locking.

These symptoms are related in time, but the code indicates that they do not
all represent the same condition.

## First action: restart the shell after an update

The most likely first explanation is that the plugin was updated without
restarting the Omarchy shell:

```sh
omarchy restart shell
```

Omarchy keeps the already-loaded plugin QML and singleton state after a plugin
checkout is updated. Omastorm detects that the manifest on disk differs from
the loaded version and displays `RESTART THE SHELL`, but it cannot safely
replace its own loaded code. See `ui/PluginSession.qml`, `updatePending`.

Any report made immediately after an upgrade should be reproduced after this
restart before treating it as a new Omastorm defect.

## What the red dot means

The red dot is not a location indicator. `ui/RadarBar.qml` displays it when:

- the bar's engine client has no state, such as during a disconnect; or
- the selected radar feed is `offline` or `unavailable`.

A feed becomes stale after ten minutes without a new radial and unavailable
after thirty minutes. The selected location can remain intact in
`~/.local/state/omastorm/state.json` while the dot is red.

This distinction can be checked at the time of failure:

```sh
cat ~/.local/state/omastorm/state.json
tail -n 100 "$XDG_RUNTIME_DIR/omastorm/engine.log"
tail -n 100 "$XDG_RUNTIME_DIR/omastorm/bootstrap.log"
```

If `state.json` still contains the expected `lat` and `lon`, the location was
not deleted; the failure is in engine connectivity or feed health.

## Confirmed Omastorm view-state gaps

The review found two concrete view synchronization defects.

### Popover navigation is not remembered

The expanded window sends its settled center to the engine and calls
`rememberView()`. The bar popover only refreshes METAR observations when its
map settles. For an already configured user, panning or zooming the popover
therefore changes only that temporary map instance. Closing and reopening it
restores the older session center.

Relevant code:

- `ui/RadarWindow.qml`, `onViewSettled`
- `ui/Popover.qml`, `onViewSettled`
- `ui/PluginSession.qml`, `userNavigated`

### The bar does not adopt view changes from the expanded window

The bar and expanded window share `state.json`. The bar session reacts when
another client changes the remembered radar lock, but it does not react when
the remembered latitude, longitude, or span changes. `adoptRememberedView()`
runs during initialization, not on subsequent file updates.

Relevant code:

- `ui/Remembered.qml`, watched `state.json`
- `ui/PluginSession.qml`, `rememberedEvents`
- `ui/PluginSession.qml`, `adoptRememberedView()`

This can make a location chosen in the expanded window appear not to reach the
already-running bar session.

## Strong in-repository shell-stall candidate

Closing the expanded panel hides its `FloatingWindow`; it does not suspend or
destroy the panel's engine client and radar map. While hidden, it can retain:

- an engine socket receiving live state;
- two asynchronous radar image buffers;
- tile and label models;
- delayed OSM tile replies and retries; and
- METAR refresh timers when aviation mode is enabled.

Each `tile_ready` reply immediately rebuilds the retained tile model. That
work scans tiles and labels, sorts labels, serializes arrays for comparisons,
updates a `ListModel`, and schedules collision layout. A burst of delayed
replies can therefore perform substantial synchronous work after the panel is
closed.

Because third-party plugin QML runs within the Omarchy shell, excessive GUI
thread work can also delay the shell's idle, screensaver, and lock callbacks.
This is a plausible causal path, not yet a reproduced root cause.

Relevant code:

- `ui/RadarWindow.qml`, `visible: app.opened`
- `ui/RadarWindow.qml`, unconditional `Engine` and `RadarMap`
- `ui/RadarMap.qml`, `tileReady()` and `rebuildTileModel()`
- `ui/RadarMap.qml`, `rebuildLabels()`

Confirmation requires profiling the shell while reproducing the failure.
Post-close activity in `rebuildTileModel()`, `JSON.stringify`,
`rebuildLabels()`, or QML delegate creation would support this explanation.

## Known upstream idle failures

Omastorm does not create a Wayland `IdleInhibitor`, so it has no direct code
that asks the compositor to stay awake. Two upstream failures can produce the
reported idle behavior:

1. Changing `idle.screensaver` or `idle.lock` while the shell is running can
   permanently silence Quickshell's `IdleMonitor` until the shell restarts.
   This is especially relevant when a test harness shortens idle timeouts.
   See [Omarchy #8038](https://github.com/basecamp/omarchy/issues/8038) and
   [Quickshell #938](https://github.com/quickshell-mirror/quickshell/issues/938).
2. Leaving a `KeyboardPanel` open can prevent the terminal screensaver from
   acquiring focus. Its self-termination is then treated as activity and the
   pending lock is cancelled. See
   [Omarchy #6917](https://github.com/basecamp/omarchy/issues/6917).

The first issue closely matches a shell that reports idle as enabled but never
produces another idle transition. Restarting the shell restores it.

## Diagnostics for a reproduction

Run these while the failure is present, before restarting:

```sh
omarchy-shell idle status
journalctl --user -t omarchy-shell | rg "omarchy idle"
cat ~/.local/state/omastorm/state.json
tail -n 100 "$XDG_RUNTIME_DIR/omastorm/engine.log"
tail -n 100 "$XDG_RUNTIME_DIR/omastorm/bootstrap.log"
```

Also record:

- whether the bar popover or any other bar panel was left open;
- whether `idle.screensaver` or `idle.lock` was edited during the session;
- whether the red dot corresponds to `offline`, `unavailable`, or a
  disconnected engine;
- whether the location remains in `state.json`; and
- whether closing the expanded window leaves an Omastorm engine-socket client
  connected.

## Current assessment

| Finding | Confidence |
| --- | --- |
| Restarting the shell is required after a plugin update | High |
| The red dot means engine/feed health, not location deletion | High |
| Popover pan/zoom is not persisted | High |
| The bar does not adopt expanded-window view changes live | High |
| Hidden radar work can continue after panel close | High |
| Hidden map work caused this exact shell wedge | Medium; needs profiling |
| Omastorm directly inhibits idle | Ruled out |

The recommended troubleshooting order is:

1. restart the shell and reproduce;
2. verify whether the coordinates remain in `state.json`;
3. identify the red-dot engine condition from logs;
4. check the known upstream idle-monitor and open-panel failures; and
5. profile post-close radar-map work if the shell still wedges.
