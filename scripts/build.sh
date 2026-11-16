#!/usr/bin/env bash
# Build Nudge's WebAssembly module, and optionally install it.
#
#   scripts/build.sh            build nudge.wasm next to plugin.json
#   scripts/build.sh install    build, then copy into rune's plugin directory
#                               ($XDG_DATA_HOME/rune/plugins/nudge)
#   scripts/build.sh check      build and fail if nudge.wasm is out of date
#
# rune installs plugins from Git *built*: commit nudge.wasm after building.
# Local paths are remapped so the module carries no file names from this
# machine (they would otherwise end up in panic locations).
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

die() { echo "build.sh: $*" >&2; exit 1; }

command -v cargo >/dev/null || die "cargo is not installed (https://rustup.rs)"
if command -v rustup >/dev/null; then
    rustup target list --installed | grep -qx wasm32-unknown-unknown \
        || die "run: rustup target add wasm32-unknown-unknown"
fi

build() {
    local cargo_home="${CARGO_HOME:-$HOME/.cargo}" sysroot
    sysroot="$(rustc --print sysroot)"
    # The last matching prefix wins: general first, specific after.
    export RUSTFLAGS="${RUSTFLAGS:-} --remap-path-prefix=$HOME=~ \
--remap-path-prefix=$cargo_home=/cargo --remap-path-prefix=$sysroot=/rust \
--remap-path-prefix=$root=/nudge"
    export CARGO_TARGET_DIR="$root/target"
    cargo build --release --target wasm32-unknown-unknown --locked
    install -m 0644 target/wasm32-unknown-unknown/release/nudge.wasm "$1"
    if grep -q --binary-files=text "$HOME" "$1"; then
        die "$1 still contains $HOME; refusing to keep it"
    fi
}

case "${1:-}" in
    "")
        build nudge.wasm
        echo "built nudge.wasm ($(wc -c <nudge.wasm) bytes)"
        ;;
    install)
        build nudge.wasm
        to="${XDG_DATA_HOME:-$HOME/.local/share}/rune/plugins/nudge"
        install -d -m 0755 "$to"
        install -m 0644 plugin.json nudge.wasm "$to/"
        echo "installed to $to — enable it in rune: Settings › Plugins › Nudge"
        ;;
    check)
        tmp="$(mktemp)"
        trap 'rm -f "$tmp"' EXIT
        build "$tmp"
        cmp -s "$tmp" nudge.wasm || die "nudge.wasm is out of date: run scripts/build.sh and commit it"
        echo "nudge.wasm is up to date"
        ;;
    *) sed -n '2,12s/^# \{0,1\}//p' "$0"; exit 2 ;;
esac
