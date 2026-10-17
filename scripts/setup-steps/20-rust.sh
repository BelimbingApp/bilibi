#!/usr/bin/env bash
# Rust toolchain: the stable channel, which must satisfy rust-version in Cargo.toml.
set -euo pipefail
# shellcheck source=scripts/lib/install.sh
source "$(dirname -- "${BASH_SOURCE[0]}")/../lib/install.sh"

load_cargo_env
msrv=$(sed -n 's/^rust-version *= *"\(.*\)"/\1/p' "$SETUP_ROOT/Cargo.toml" | head -n1)
current=$(rustc --version 2>/dev/null | awk '{print $2}' || true)
if [[ -n $current ]] && version_ge "$current" "$msrv" && cargo clippy --version >/dev/null 2>&1 && cargo fmt --version >/dev/null 2>&1; then
    present rust
else
    if ! command -v rustup >/dev/null; then
        curl -fsSL --proto '=https' --tlsv1.2 https://sh.rustup.rs -o "$SETUP_ROOT/.local/rustup-init.sh" >>"$SETUP_LOG" 2>&1
        log sh "$SETUP_ROOT/.local/rustup-init.sh" -y --profile minimal --default-toolchain stable
        rm -f "$SETUP_ROOT/.local/rustup-init.sh"
        load_cargo_env
    fi
    log rustup toolchain install stable --profile minimal --component clippy --component rustfmt
    log rustup default stable
    current=$(rustc --version | awk '{print $2}')
    version_ge "$current" "$msrv" ||
        fail rust "rustc $current is older than rust-version $msrv in Cargo.toml" 'wait for that stable release, or lower rust-version'
    installed rust
fi
