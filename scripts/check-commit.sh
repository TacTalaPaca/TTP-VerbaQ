#!/usr/bin/env bash
# Local release gate. Run before asking for a commit.
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=$(tr -d '\n' < VERSION)
CARGO_VERSION=$(awk -F'"' '/^version = /{print $2; exit}' Cargo.toml)

if [[ "$CARGO_VERSION" != "$VERSION" && "$CARGO_VERSION" != "$VERSION.0" ]]; then
    echo "version mismatch: VERSION=$VERSION Cargo.toml=$CARGO_VERSION" >&2
    exit 1
fi

if grep -rIP '\x{2014}|\x{2013}' \
    --exclude-dir=target --exclude-dir=dist --exclude-dir=.git .; then
    echo "em dash found" >&2
    exit 1
fi

cargo fmt --check
cargo clippy --release --all-targets -- -D warnings
cargo test --release
desktop-file-validate assets/verbaq.desktop

echo "checks passed for VerbaQ $VERSION"
