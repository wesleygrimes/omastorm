# Contributing

Report bugs and propose work in [GitHub issues](https://github.com/wesleygrimes/omastorm/issues).
Every pull request requires an issue approved by a maintainer before the PR is
opened. This applies to bug fixes and feature requests, including draft PRs
and automated PRs.

## Issues

For a bug, use the
[bug report template](https://github.com/wesleygrimes/omastorm/issues/new?template=bug-report.md).
Say what happened, what you expected, and your Omarchy version, plugin commit,
engine, and GPU as the template asks.

`$XDG_RUNTIME_DIR/omastorm/engine.log` is the daemon's stderr: startup,
`Live {site}: …` feed lines, decode and tile errors. Attach the last screenful
covering the failure, not a single line. If the engine never installed, attach
`bootstrap.log` from the same directory too. Paths are in the
[README](README.md#if-something-is-wrong).

For a feature, use the
[feature request template](https://github.com/wesleygrimes/omastorm/issues/new?template=feature-request.md): what should change on screen, why it belongs
in this app, and how it fits [DESIGN.md](DESIGN.md). Keep the feature set small.

## Issue approval

1. Open an issue using the bug report or feature request template.
2. Agree on the scope and acceptance criteria with a maintainer. Wait for them
   to apply the `approved` label before opening a PR.
3. Link the approved issue in the PR body and keep the change within its agreed
   scope. Use `Closes #123` if merging should close the issue.

Issues have exactly one class: `bug` for a bug fix or `enhancement` for a feature
request. Other labels can help triage, but do not grant approval. Changes to
docs, tests, tooling, and dependencies still need an issue describing the bug
they fix or the improvement they propose.

Maintainers apply `approved` once the scope is settled. Approval means the work
is welcome for review; it does not guarantee the PR will be merged. Discuss
scope changes on the issue before expanding the PR.

Maintainers enforce this policy during review and may close PRs opened without
prior issue approval or outside the approved scope. PRs must also pass the
required `CI` check and receive maintainer review before merging.

## Labels

Every issue has exactly one of `bug` or `enhancement`. A PR's label sorts it
in the release notes, so it describes what people running Omastorm see:
`bug` for a user-visible fix, `enhancement` for a user-visible feature, and no
type label for tooling, CI, tests or release chores, which then list under
Other changes. Add other labels only when they help someone decide what to do
next.

| Label | Use |
| --- | --- |
| `bug` | Existing behavior is broken. |
| `enhancement` | New capability or improvement. |
| `documentation` | Docs work; supplement the issue or PR type. |
| `approved` | A maintainer agreed to the issue scope before implementation. |
| `needs-author` | Waiting for information or changes from the author. |
| `blocked` | Waiting on another issue or an external dependency. |
| `help wanted` | Approved work available for a contributor to pick up. |
| `good first issue` | Approved, small, scoped work suitable for a newcomer. |
| `duplicate` | Already tracked elsewhere; link the original when closing. |
| `wontfix` | Outside scope or declined; explain the decision when closing. |

`approved` applies to issues and does not replace PR review. Remove
`needs-author` or `blocked` when the wait ends. Use GitHub review requests
to indicate that a PR needs review. Priority, component, and release labels
are not part of this set.

## Develop

Read [README.md](README.md) for the user guide and [DESIGN.md](DESIGN.md) for product
rules. [docs/README.md](docs/README.md) indexes internal docs.
[docs/protocol.md](docs/protocol.md) defines the engine/client contract;
[engine/README.md](engine/README.md) maps the backend.

Use an Omarchy desktop with Quickshell/OpenGL, Python 3, Qt Shader Tools,
Qt Declarative tools, Node.js, and socat. `mise setup --install-tools` installs the
mise-managed toolchain and fetches locked dependencies. Desktop packages need
an explicit `omarchy pkg add`; setup never installs privileged packages.
Use `--profile engine` or `--profile ui` for a narrower CI environment.
ImageMagick and ffmpeg are optional capture tools.

```sh
mise setup --install-tools
mise build
mise dev
```

Use `mise tasks` to discover the public commands. For routine verification,
invoke the named tasks and report the task and result when handing off work.
The Python and shell scripts underneath are implementation helpers; `mise exec`
selects a toolchain without selecting a verification workflow.

The nine public tasks are setup, dev, test, lint, format, build, check, bench,
and release. `mise <command> --help` shows exact argument help, generated from
`scripts/tooling/cli.py` into each task's `usage`; after changing arguments,
run `mise format --scope tooling`. Arguments after `--` pass through unparsed.
Setup prepares verified fixtures and a checksum-verified published engine under
ignored `target/pinned-data/`; subsequent development and tests run offline.

`mise dev` installs `com.omastorm.radar-dev` beside the production plugin,
marked with a D and a dev name in the left bar. Open its popover and expand
using the usual controls. The dev panel, runtime socket, cache, config, and
remembered state are separate from production. The command prints its selected
binary and uses the committed published pin by default. `mise dev --engine
candidate` builds source offline and rebuilds/restarts its owned daemon on
engine changes. `mise build` and candidate dev builds explicitly use this
checkout's `target/`, overriding `CARGO_TARGET_DIR` and Cargo's target-dir setting
to keep redirected build output from leaving the launched candidate stale.
QML/JS/manifest saves reload; shader saves compile before reload.
A rescan reloads shell plugins generally. No shell restart is automatic.

The global ownership lock prevents two worktrees replacing one another's dev
plugin. Normal exit, Ctrl-C, TERM, HUP, and handled failures remove owned
resources and layout entries while preserving unrelated configuration.
SIGKILL/power loss cannot run cleanup; the next session detects owned stale
artifacts. Cleanup errors are reported. Logs remain under `target/dev/`.
The development command modifies your actual bar: agent-driven installation
needs your permission before running it on the desktop.

## What not to change

Do not bump [engine/release.pin](engine/release.pin) until the named GitHub
Release exists and its asset is verified. [docs/RELEASING.md](docs/RELEASING.md)
is the sequence.

[golden/](golden/) is the decoder's answer key. Regenerating it is a
decoder-contract change: keep the provenance and dates in the JSON, and do not
rewrite it to match a new decode by accident. Capture scripts write images
under `review/` for visual review; those stay out of git. README stills are
`docs/media/readme/` (`bash scripts/capture-readme.sh`). Include review
captures with a rendering change.

Capture scripts remove their temporary inputs, caches, and raw demo frames on
exit, including failures and handled signals. Final images and videos remain in
`review/` and `docs/media/`. Register cleanup immediately after `mktemp`, keep
helper directories inside the same scratch tree, and stop only processes owned
by that capture. Build outputs, fixtures, and the shared daemon are preserved.

Honor [DESIGN.md](DESIGN.md): actual scan times, no forecasts, chrome from the
Omarchy theme, radar color only from `frame.palette`.

## Verify and submit

Branch from `main`. One change per pull request. During iteration, run the
focused checks that cover the change:

| Change | Command |
| --- | --- |
| Engine logic, decoding, storage | `mise check --scope engine` |
| Wire output, commands, daemon lifecycle | `mise test integration --scope protocol` |
| QML, launcher, installer, UI integration | `mise check --scope ui` |
| Shader sampling, camera, rendering | `mise check --gpu --scope rendering` |

Focused checks support iteration. CI enforces the complete applicable PR suite.
PR CI has lint, source engine tests, published-pin plugin integration and a
fail-closed required result named `CI`. Documentation-only changes run basic,
documentation and tooling regressions without Rust. UI-only changes use the
verified published engine without compiling Rust. Dependencies/build settings,
fixture extraction, native prerequisites, release/tooling and unknown paths
also require native x86_64/aarch64 optimized artifact and packaging checks.
Engine source changes also run native aarch64 tests, so an architecture
failure appears before an immutable release tag.
Engine release production is a separate workflow; a manual full run provides
candidate platform evidence without drafting or publishing a release.
`mise test` runs unit and CPU rendering math without desktop processes.
`mise test integration --scope protocol` exercises real socket processes.
`mise test integration --scope ui --case popover` selects one UI scenario;
repeat `--case` to select several. `--scope installer` covers binding docs,
launcher discovery and engine installation. `--engine candidate --binary
/path/to/engine` adds compatible candidate coverage after the published-pin
run, without overwriting either binary. `mise build` prepares a local candidate.
UI-only integration does not compile Rust. `mise test --scope ui` runs pure JS
logic without Quickshell; `--scope tooling` runs isolated command/lifecycle cases.
`mise check --scope engine` includes formatting, Clippy and engine unit checks.
`mise check` remains available for complete verification. `mise check --changed`
selects the union of the branch diff against `origin/main` plus staged,
unstaged and new files; `--base` changes that comparison. Unknown paths select
complete applicable checks. GPU verification is explicit with `--gpu`;
`--changed` names it when rendering paths changed.
QML/JS parsing and native JS lint run with Qt tools. This is a syntax check:
`mise format` applies rustfmt only and never rewrites QML/JS. Integration exercises
Quickshell imports, and desktop checks validate Omarchy imports/routing.
`mise check` also verifies both published pin assets using GitHub; this
read-only release validation needs network access. Development and integration
consume prepared dependencies/fixtures offline. Native platform checks for the
other architecture run in CI. The Cargo helper resolves the installed Rust
toolchain directly so isolated XDG paths do not trigger mise tool downloads.
Documentation drift checks run in every CI scope.
Logs and timings live
under ignored `target/evidence/`; use the recorded wall time for elapsed verification, rather than summing
historical overlapping step durations. Tests must use owned isolated processes and deterministic fixture inputs.

A new standalone script or public task needs a distinct operational reason;
new regressions normally belong in an existing suite. Keep docs synchronized
with behavior in the same commit. UI/pin changes must pass with the committed
published engine; candidate tests add coverage when compatible.

For shader, sampling, or camera changes, also run `mise check --gpu` and
`bash scripts/capture-review.sh`, inspect the images in `review/`, and include
captures with the review. The rendering test replays the shader's sampling
rule in Rust; update both when changing that rule. Rebuild changed radar,
tile, or grid shaders with `bash scripts/build-shader.sh` and commit their `.qsb` files.
The GPU checks need a desktop OpenGL context; software Qt Quick is unsupported.
If the environment cannot run a required check, report that explicitly.

Open a pull request linking the previously approved issue as described above.
Keep commits small. Commit messages and
pull request titles use
[Angular conventional commits](https://www.conventionalcommits.org/en/v1.0.0/#summary):

```text
<type>(optional-scope): <description>
```

Use a lowercase type (`feat`, `fix`, `docs`, `refactor`, `test`, `ci`,
`chore`, `perf`, `build`, `revert`), an imperative description, and no
trailing period. Scope is optional; common ones are `engine`, `ui`, `docs`,
and `scripts`. Examples:

```text
feat(ui): remember camera and station after reconnect
fix(engine): restart a quiet live poller instead of UNAVAILABLE
docs: document the first-time contributor path
ci: bump jdx/mise-action to v4.3.0
```

The pull request body says why the behavior changed and how you verified it.
Omit co-author and tool trailers. Update the relevant docs when behavior
changes. Document current behavior, not implementation history.

## Releases

Maintainers: [docs/RELEASING.md](docs/RELEASING.md).

## Contributors

Merged help is credited in the README with
[all-contributors](https://allcontributors.org). On a pull request or issue,
comment:

```text
@all-contributors please add @username for code
```

Use the right
[emoji key](https://allcontributors.org/docs/en/emoji-key) type
(`code`, `doc`, `bug`, `infra`, and so on). The bot opens a small follow-up
pull request that updates the contributor table.

## Conduct

Be kind and treat people well.
