# Releasing Omastorm

The repository is the plugin: installs clone the default branch into
`~/.config/omarchy/plugins/com.omastorm.radar`; updates fast-forward it.
The engine is a separate GitHub Release binary, selected by tag and sha256 in
`engine/release.pin`. Plugin and engine versions move independently.

Published releases are immutable. Prepare assets in a draft; a mistake after
publication requires a new version. Never pin an unpublished or unverified
binary.

## Engine

Use a configured checkout with GitHub CLI authentication and release access.
The implementation is [scripts/release-engine.sh](../scripts/release-engine.sh).

1. Bump `version` in `engine/Cargo.toml`. Run
   `mise exec -- cargo build --offline` to update `Cargo.lock`, then
   `mise check`. Commit both files and push to `main`.
2. Run `mise release --dry-run` from clean `main`, even with `origin/main`.
   It builds the stripped candidate, checks its reported version and UI
   protocol compatibility, and prints the tag, sha256, and release notes.
   Review these before publishing.
3. Run `mise release` and confirm publication. It creates `engine-<version>`
   with the binary and `SHA256SUMS`, downloads the published asset, and verifies
   it against the candidate before writing `engine/release.pin`.
4. Run `mise check` to verify installation from the new pin, then commit the
   pin separately and push. Users receive it on their next plugin update.

`mise build-release` builds a local candidate without publishing. Do not use
its `--write-pin` option to bypass published-asset verification.
If release validation fails, resolve the reported precondition. If an already
published asset is wrong, leave the pin unchanged and release a new version.

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
