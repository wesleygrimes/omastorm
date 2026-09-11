#!/usr/bin/env bash
# Opt-in bar-plugin symlink (CONTRIBUTING.md, mise plugin-link): writes only under
# scratch XDG_CONFIG_HOME, never from install or ordinary launch.
set -euo pipefail
cd "$(dirname "$0")/.."

fail() { printf '%s\n' "$@" >&2; exit 1; }

scratch=$PWD/target/check-link-plugin
rm -rf "$scratch"
mkdir -p "$scratch"
trap 'rm -rf "$scratch"' EXIT
export XDG_CONFIG_HOME="$scratch/config"
plugins=$XDG_CONFIG_HOME/omarchy/plugins
target=$plugins/com.omastorm.radar
backup=$plugins/.com.omastorm.radar.unlinked
root=$(readlink -f "$PWD")
path=$(bash scripts/link-plugin.sh --print-path)
[[ $path == "$target" ]] || fail "--print-path: $path"

if bash scripts/link-plugin.sh --bogus 2>"$scratch/usage.err"; then
  fail 'link-plugin.sh accepted an unknown argument'
fi
rg -q 'usage: link-plugin.sh' "$scratch/usage.err" \
  || fail "Unknown-arg error was unclear: $(cat "$scratch/usage.err")"

rg -q '\[tasks\.plugin-link\]' mise.toml || fail 'mise.toml is missing plugin-link'
rg -q '\[tasks\.plugin-unlink\]' mise.toml || fail 'mise.toml is missing plugin-unlink'
if rg -q '^\[tasks\.link\]' mise.toml; then
  fail 'mise.toml must not use tasks.link; mise link is reserved'
fi
rg -q 'link-plugin.sh --rescan' run.sh \
  || fail 'run.sh does not call link-plugin.sh --rescan'
if rg -q 'link-plugin' scripts/fetch-engine.sh scripts/write-desktop-entry.sh; then
  fail 'an installer references link-plugin.sh'
fi
if rg -q 'OMASTORM_RESCAN_PLUGIN' scripts/check.sh scripts/capture-*.sh; then
  fail 'check or capture sets OMASTORM_RESCAN_PLUGIN'
fi
[[ ! -e $target && ! -L $target ]] || fail 'Scratch already had the plugin path'

status=$(bash scripts/link-plugin.sh --status)
[[ $status == 'installed copy' ]] || fail "unlinked --status: $status"

mkdir -p -- "$scratch/bin"
cat > "$scratch/bin/omarchy-shell" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${OMASTORM_SHELL_LOG:?}"
case $* in
  'shell listPlugins') printf '[{"id":"com.omastorm.radar"}]\n' ;;
  'shell enablePlugin com.omastorm.radar {}') printf 'ok\n' ;;
  'shell ping') printf 'ok\n' ;;
esac
exit 0
EOF
cat > "$scratch/bin/omarchy" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "${OMASTORM_OMARCHY_LOG:?}"
exit 0
EOF
chmod +x -- "$scratch/bin/omarchy-shell" "$scratch/bin/omarchy"
export PATH="$scratch/bin:$PATH" OMASTORM_SHELL_LOG="$scratch/ipc.log" \
  OMASTORM_OMARCHY_LOG="$scratch/omarchy.log"
: > "$OMASTORM_SHELL_LOG"
: > "$OMASTORM_OMARCHY_LOG"

bash scripts/link-plugin.sh --rescan
[[ ! -s $OMASTORM_SHELL_LOG ]] || fail 'unlinked --rescan talked to omarchy-shell'
[[ ! -s $OMASTORM_OMARCHY_LOG ]] || fail 'unlinked --rescan talked to omarchy'

bash scripts/link-plugin.sh
[[ -L $target ]] || fail 'link did not create a symlink'
[[ $(readlink -f "$target") == "$root" ]] || fail "link points at $(readlink -f "$target")"
[[ ! -e $backup ]] || fail 'link without a clone wrote a backup'
rg -q '^restart shell$' "$OMASTORM_OMARCHY_LOG" || fail 'link did not restart the shell'
rg -q 'enablePlugin com.omastorm.radar \{\}' "$OMASTORM_SHELL_LOG" \
  || fail 'link did not enable the shell plugin'
status=$(bash scripts/link-plugin.sh --status)
[[ $status == 'this checkout (linked)' ]] || fail "linked --status: $status"

: > "$OMASTORM_SHELL_LOG"
: > "$OMASTORM_OMARCHY_LOG"
bash scripts/link-plugin.sh
[[ -L $target ]] || fail 'second link dropped the symlink'
[[ ! -e $backup ]] || fail 'second link invented a backup'
rg -q '^restart shell$' "$OMASTORM_OMARCHY_LOG" || fail 'second link did not restart the shell'
rg -q 'enablePlugin com.omastorm.radar \{\}' "$OMASTORM_SHELL_LOG" \
  || fail 'second link did not enable the shell plugin'

: > "$OMASTORM_SHELL_LOG"
out=$(bash scripts/link-plugin.sh --unlink)
[[ ! -e $target && ! -L $target ]] || fail 'unlink without a clone left a plugin path'
[[ ! -e $backup ]] || fail 'unlink left a backup with no clone'
if rg -q 'enablePlugin' "$OMASTORM_SHELL_LOG"; then
  fail 'unlink enabled the plugin'
fi
printf '%s\n' "$out" | rg -q 'omarchy plugin add https://github.com/wesleygrimes/omastorm --enable' \
  || fail "unlink without clone should name plugin add: $out"

if bash scripts/link-plugin.sh --unlink 2>"$scratch/unlink.err"; then
  fail 'unlink succeeded when the plugin was already gone'
fi
rg -q 'not installed' "$scratch/unlink.err" || fail "gone-unlink error was unclear: $(cat "$scratch/unlink.err")"

mkdir -p -- "$target/ui"
printf 'clone\n' > "$target/manifest.json"
: > "$OMASTORM_SHELL_LOG"
bash scripts/link-plugin.sh
[[ -L $target ]] || fail 'link did not replace the clone'
[[ -d $backup ]] || fail 'link did not keep the clone'
[[ -f $backup/manifest.json ]] || fail 'kept clone lost manifest.json'
[[ $(<"$backup/manifest.json") == clone ]] || fail 'kept clone contents changed'

bash scripts/link-plugin.sh --unlink
[[ -d $target && ! -L $target ]] || fail 'unlink did not restore the clone'
[[ ! -e $backup ]] || fail 'unlink left the backup in place'
[[ $(<"$target/manifest.json") == clone ]] || fail 'restored clone contents changed'

mkdir -p -- "$backup"
if bash scripts/link-plugin.sh 2>"$scratch/conflict.err"; then
  fail 'link replaced a clone while a backup already existed'
fi
rg -q 'Kept clone already' "$scratch/conflict.err" \
  || fail "backup-conflict error was unclear: $(cat "$scratch/conflict.err")"
[[ -d $target && ! -L $target ]] || fail 'failed link moved the live clone'

rm -rf -- "$backup" "$target"
ln -s -- /tmp "$target"
if bash scripts/link-plugin.sh 2>"$scratch/other.err"; then
  fail 'link replaced a symlink to another path'
fi
rg -q 'not this checkout' "$scratch/other.err" \
  || fail "foreign-symlink error was unclear: $(cat "$scratch/other.err")"

echo 'Link-plugin: scratch symlink, backup/restore, gated shell restart, no install/launch write PASS'
