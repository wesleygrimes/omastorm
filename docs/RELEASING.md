# Releasing Omastorm

The repository is the plugin: installs clone the default branch into
`~/.config/omarchy/plugins/com.omastorm.radar`; updates fast-forward it.
The engine is a separate GitHub Release binary, selected by tag and sha256 in
`engine/release.pin`. Plugin and engine versions move independently.

Published releases are immutable. Prepare assets in a draft; a mistake after
publication requires a new version. Never pin an unpublished or unverified
binary.

The pin contains a shared `tag` and `repo`, and an `asset_<architecture>` /
`sha256_<architecture>` pair for each published Linux architecture
(`x86_64`, `aarch64`). Missing architectures fail before download. Native
builds write the GNU target's binary, checksums, and a candidate pin under
`target/dist/`; the tracked pin changes only after public assets are verified.
The current x86 release is retained until a new release includes ARM64.
Never add the ARM asset to an already published, immutable release.

## Release paths

Users run `main`: a merged change reaches them on their next plugin update. A
`v*` tag is the thank-you note for that work, not the thing users wait for.
The engine they run is whatever `engine/release.pin` names. A maintainer
decides when an engine release happens; nothing here publishes one on its own.
Every change takes one of four paths.

Plugin or UI only (QML, copy, docs, README): merge anytime. No engine release.
Honor the work later with a `v*` tag; generate its notes as Plugin step 3
describes, from the previous `v*` tag and never an `engine-*` tag.

Engine only: the code may merge to `main`, but users keep the pinned binary
until a maintainer finishes the engine sequence below and the pin commit
lands. Batch engine releases; do not release on every merge. Credit the
engine author in the release notes.

Plugin and engine together in one PR: do not merge while the UI needs a
protocol or feature the published pin does not speak. `mise check` builds the
PR's own engine, so a green check proves nothing about the pin. Leave the PR
open until the engine binary is published, then land the pin and the UI in
the same push so an update never puts the UI ahead of the binary.

Split PRs: merge the engine PR first; the plugin on `main` must still speak
the current pin. Publish the engine and land the pin. Then merge the UI PR.
Never merge the UI first.

## Engine

Prefer CI. Bump the version on `main`, let
[.github/workflows/engine.yml](../.github/workflows/engine.yml) build both
Linux architectures, push `engine-<version>`, publish the draft, then verify
the public bytes before writing the pin.

The workflow runs the mise toolchain's lint, Rust tests, engine pin checks,
and release checks on native `ubuntu-24.04` x86_64 and `ubuntu-24.04-arm`
aarch64 runners. Every native candidate must answer hello with the engine
version and UI protocol. GPU/QML checks still require an Omarchy desktop.
PRs, branch pushes, and manual runs produce workflow artifacts. Pushing
`engine-<version>` (the tag must match Cargo.toml) combines both candidates
and creates a draft GitHub Release; it refuses to overwrite an existing
release.

Ubuntu runners execute the newly built native candidates. They verify
installation and checksums for the existing published pin with
`--published-install-only`: the Arch-built engine-0.1.2 x86 asset requires
glibc 2.44, newer than Ubuntu 24.04. Normal desktop checks still require the
pinned binary to run. Building future releases on Ubuntu also avoids inheriting
the build machine's newer Arch glibc.

1. Run `mise engine-bump` (next patch) or `mise engine-bump -- <version>`.
   It writes `engine/Cargo.toml` and `Cargo.lock`. Run `mise check`, commit
   both files, and push to `main`.
2. Wait for Engine builds CI on that commit to finish on both architectures.
3. From clean `main` whose HEAD is origin's current `main` commit, run
   `mise engine-tag`. It pushes an annotated `engine-<version>` tag. CI
   drafts the GitHub Release with both binaries, build metadata,
   `SHA256SUMS`, and the candidate pin.
4. Review the draft and publish it. Never pin a draft or add assets after
   publishing.
5. Download the published `release.pin` or the `engine-release` workflow
   artifact into `target/dist/`. Run `mise engine-verify` to require every
   public binary to match the candidate checksums. It writes nothing.
6. Run `mise engine-pin` to write `engine/release.pin`. Run `mise check`,
   then commit the pin separately and push. Users receive it on their next
   plugin update.

`mise engine-verify` and `mise engine-pin` call
[scripts/pin-engine-release.sh](../scripts/pin-engine-release.sh). Neither
commits. If an already published asset is wrong, leave the pin unchanged and
release a new version.

### Local fallback

Use a laptop only when CI cannot cut the release. You still need both native
binaries and their `.build.json` metadata from the same commit: download the
`engine-release` workflow artifact into `target/dist/`, or gather both native
outputs. A missing, stale, or mislabeled build fails packaging before a draft
is created.

`mise release` remains the one-shot local path
([scripts/release-engine.sh](../scripts/release-engine.sh)). From clean
`main` whose HEAD is origin's current `main` commit, and with GitHub CLI
authentication and release access:

1. After the version bump is on `main`, run `mise release --dry-run`. It
   builds the native stripped candidate, verifies both binaries' source
   commit/version/checksums, checks the native candidate's reported version
   and UI protocol compatibility, and prints the tag, sha256, and release
   notes. Review these before publishing.
2. Run `mise release` and confirm publication. It creates `engine-<version>`,
   publishes, verifies every published binary, and writes `engine/release.pin`.
3. Run `mise check`, then commit the pin separately and push.

`mise build-release` builds a local candidate without publishing (CI calls
this per architecture). Do not use its `--write-pin` option to bypass
published-asset verification. After a local candidate, prefer
`mise engine-verify` / `mise engine-pin`.

If release validation fails, resolve the reported precondition.

## Plugin

1. Bump `manifest.json` `version`. If shipping a new engine, complete the
   engine release and pin update first.
2. Run `omarchy plugin validate .` and the checks required by
   [CONTRIBUTING.md](../CONTRIBUTING.md#verify-and-submit). Commit and push
   the release changes to `main`.
3. Tag that same commit `v<version>` and push the tag. Create a draft GitHub
   Release for it with user-facing notes. Generate release notes against the
   previous `v*` tag, never an `engine-*` tag (engine releases compare only
   against the previous `engine-*` tag). GitHub groups pull requests by label
   per [.github/release.yml](../.github/release.yml); unlabeled ones land under
   Other changes, and GitHub adds New Contributors itself. Attach any new
   [README media](media/README.md) before publishing; keep media out of git.
4. Publish the draft. Keep README media URLs pointed at releases containing
   those assets; older demo assets can stay linked.

The website is deployed separately; see [site/README.md](../site/README.md)
for its version display, media preparation, and deployment steps.
