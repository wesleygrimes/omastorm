# Contributing

Report bugs and propose work in [GitHub issues](https://github.com/wesleygrimes/omastorm/issues).
Track pending work in issues and projects; discuss new features there before
implementation.

## Issues

For a bug, use the
[bug report template](https://github.com/wesleygrimes/omastorm/issues/new?template=bug-report.md).
Say what happened, what you expected, and your Omarchy version, plugin commit,
engine, and GPU as the template asks.

`$XDG_RUNTIME_DIR/omastorm/engine.log` is the daemon's stderr: startup,
`Live {site}: …` feed lines, decode and tile errors. Attach the last screenful
covering the failure, not a single line. If the engine never installed, attach
`bootstrap.log` from the same directory too. Paths are in the
[README](README.md#troubleshooting).

For a feature, open an issue first: what should change on screen, why it belongs
in this app, and how it fits [DESIGN.md](DESIGN.md). Keep the feature set small.

## Develop

Read [README.md](README.md) for the app and [DESIGN.md](DESIGN.md) for product
rules. [docs/protocol.md](docs/protocol.md) defines the engine/client contract;
[engine/README.md](engine/README.md) maps the backend.

Use an Omarchy desktop with Quickshell and OpenGL, `qt6-shadertools`, and
`socat`. Install [mise](https://mise.jdx.dev), then from a checkout:

```sh
mise install
mise setup
mise start
```

Setup checks desktop dependencies, downloads verified fixtures, and builds the
engine. Rust comes from mise; use `mise exec -- cargo …` for Cargo commands.
[mise.toml](mise.toml) is the task and toolchain reference (`mise tasks` lists
jobs). For an offline archived scan:

```sh
OMASTORM_ARCHIVE=data/raw/KTLX20130520_201643_V06.gz mise start
```

The daemon is shared and outlives windows. Launch replaces a stale build and
open clients reconnect. Use `mise stop` to end it, never `kill`. Close only
Quickshell instances you launched; a windowless process left after closing is
a leak to investigate.

`mise start` loads this checkout's `ui/` in a window. The bar still uses the
installed plugin under `~/.config/omarchy/plugins/com.omastorm.radar` unless
you point it here:

```sh
mise plugin-link
```

That replaces the install directory with a symlink to this checkout (the
previous clone is kept beside it), restarts the Omarchy shell, and
enables the bar widget.
After that, `mise start`, `mise restart`, and `mise onboard` also restart the
shell so the popover matches this tree (a symlink skips the plugin file
watcher, and `rescanPlugins` keeps the old QML). `mise onboard`'s empty weather
and state files apply only to the window; the popover keeps its usual place
files. `mise plugin-unlink` restores the clone. A tty launch prints the qml path,
live vs archive, whether the bar is linked, and which config/state/place
files apply. `mise restart` stops the daemon first so a check or capture
leftover is not reused. `mise onboard` starts the window with no weather file
and no remembered view, so the location picker shows.

## What not to change

Do not bump [engine/release.pin](engine/release.pin) until the named GitHub
Release exists and its asset is verified. [docs/RELEASING.md](docs/RELEASING.md)
is the sequence.

[golden/](golden/) is the decoder's answer key. Regenerating it is a
decoder-contract change: keep the provenance and dates in the JSON, and do not
rewrite it to match a new decode by accident. Capture scripts write images
under `review/` for visual review; those stay out of git. Regenerate them when
the picture changed, and include the captures with the review.

Honor [DESIGN.md](DESIGN.md): actual scan times, no forecasts, chrome from the
Omarchy theme, radar color only from `frame.palette`.

## Verify and submit

Branch from `main`. One change per pull request. Run `mise check` before every
commit; it uses scratch daemons and leaves the shared daemon alone. Cargo runs
first, then the Rust tests run alongside the UI checks, which proceed in two
lanes. Scratch and logs live under `target/check/`, never `/tmp`; the daemons
and runtime files go on every exit, and the logs stay until the next run.
The checks read `target/debug/`, so leave `CARGO_TARGET_DIR` unset. There is no
GitHub Actions suite yet; a pull request is ready when those local checks pass.

For shader, sampling, or camera changes, also run `mise check --gpu` and
`bash scripts/capture-review.sh`, inspect the images in `review/`, and include
captures with the review. The rendering test replays the shader's sampling
rule in Rust; update both when changing that rule. Rebuild changed radar or
tile shaders with `bash scripts/build-shader.sh` and commit their `.qsb` files.
The GPU checks need a desktop OpenGL context; software Qt Quick is unsupported.
If the environment cannot run a required check, report that explicitly.

Open a pull request linking the issue. Subject is a short imperative; the body
says why the behavior changed and how you verified it. Keep commits small.
Omit co-author and tool trailers. Update the relevant docs when behavior
changes. Document current behavior, not implementation history.

## Releases

Maintainers: [docs/RELEASING.md](docs/RELEASING.md).

## Conduct

Be kind and treat people well.
