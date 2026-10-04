# Releasing Omastorm

The installed plugin follows `main`; merging UI changes delivers them on the
next install/update. The engine is a separately published binary selected by
[engine/release.pin](../engine/release.pin). Engine source may be ahead of that
pin. Plugin and engine versions move independently.

Every repository change, including versions and pins, goes through an approved
issue and a PR. See [CONTRIBUTING.md](../CONTRIBUTING.md#issue-approval).
Prepare and pin commands write locally on a working branch, never on `main`
or detached HEAD. They do not commit, merge, or push. Publication is an explicit
GitHub action after draft review. Published assets are immutable; a mistake
requires a new version.

## Engine

1. On a working branch, run `mise release engine prepare <version>` with a
   greater X.Y.Z version. It changes only the engine package version in
   `engine/Cargo.toml` and `Cargo.lock`, preserving dependency versions. Review,
   verify, and submit that change as a PR.
2. After the version PR merges, update local `main`. From clean local `main`
   matching freshly queried remote `main`, run `mise release engine tag`.
   The command verifies the origin and all push destinations match the release
   repository, refuses existing tags/releases, and pushes only the annotated
   `engine-<version>` tag.
3. [Engine release CI](../.github/workflows/engine-release.yml) builds native
   optimized x86_64 and aarch64 engines, runs source unit/protocol tests and
   native wire smoke checks, then packages exact source/version/hash metadata,
   both binaries, `SHA256SUMS`, and a candidate `release.pin`. Only a tag push
   creates a draft; manual workflow runs produce candidate artifacts.
4. Review both native results, the draft assets and notes, then publish in
   GitHub. Do not modify assets after publishing.
5. On a working branch, run `mise release engine pin engine-<version>`.
   It requires a public published release, resolves the immutable tag source,
   verifies both complete executable ELF architectures, checksums, build
   metadata and candidate pin, and writes the tracked pin only after every
   check passes. Failure leaves the tracked pin unchanged.
6. Submit the verified pin and compatible UI in a PR. Run published-pin UI
   coverage before shipping; protocol equality alone does not prove features.

For a breaking engine protocol change, the engine source PR can merge while
the existing UI continues using its published pin. Publish the new engine
first, then merge compatible UI and its verified pin together. UI must never
require an unpublished engine. Source tests exercise the candidate separately;
UI integration always checks the committed published pin, with compatible
candidate coverage added afterward. No disposable checkout or Cargo stub is
needed.

The internal native build operation is
`bash scripts/build-engine-release.sh`; packaging and native smoke operations
live in `scripts/tooling/release.py` for workflow reuse. They never update the
tracked pin or publish. Ordinary engine logic PRs avoid packaging and the
x86_64 optimized build, but run native aarch64 tests so an architecture
failure surfaces before the immutable tag. Build settings, dependencies, fixture extraction, native
prerequisites, release/tooling changes and explicit full workflow runs select
both native platform validations and packaging. Published engines run in
Arch-based plugin integration, while Ubuntu native jobs execute their own
source-built candidates.

## Plugin

1. On a working branch, run `mise release plugin prepare <version>`. Verify
   the change and submit the manifest version through a PR. Complete any new
   engine publication and compatible pin/UI PR before shipping dependent UI.
2. After merge, run `mise release plugin tag` from clean freshly verified
   `main`. It pushes only `v<version>` and creates a draft titled
   `Omastorm <version>`. Existing tags/releases are refused.
3. Check that each PR in the draft sits under the right heading. If one is
   misfiled, fix its label per [CONTRIBUTING](../CONTRIBUTING.md#labels) and
   regenerate the notes (Generate release notes in the draft editor).
4. Add a [summary](#release-notes) above the generated notes and any release
   [media](media/README.md), then publish the draft in GitHub. Keep README
   media URLs pointed at releases containing those assets.

## Release notes

Titles are the product and version only, without the tag's prefix or a
subtitle: `Omastorm 0.1.17` and `Engine 0.1.11`. The tooling sets them.

Keep the generated notes. Above them, add a few sentences summarizing the
highlights for people running the plugin. Name security fixes plainly and
credit the reporter.

Generated notes use an older ancestor tag from the same family, queried from remote
state: engine tags compare with engine tags, plugin tags with plugin tags.
GitHub lists every PR in that range, so the tooling keeps only PRs that changed
what the release ships: engine source and Cargo files for the engine; `ui/`,
the manifest, the installer scripts and the pin for the plugin. A PR reverted
in the same range is dropped along with its revert.
The first release in a family gets explicit initial-release notes. GitHub's
[release categories](../.github/release.yml) use PR labels; follow the
[contributor label guidance](../CONTRIBUTING.md#labels).

`mise release` with no stage shows help and performs no mutation. Commands
never publish releases. If a precondition fails, resolve that condition rather
than bypassing it. The website deploys separately; see
[site/README.md](../site/README.md).
