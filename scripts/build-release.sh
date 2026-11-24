#!/usr/bin/env bash
# Build the release binary and every package format into dist/.
set -euo pipefail
cd "$(dirname "$0")/.."

VERSION=$(tr -d '\n' < VERSION)
DIST="$PWD/dist"
TTP_PKGS="${TTP_PKGS:-$HOME/TacTalaPaca/TTP-pkgs}"
echo "VerbaQ $VERSION"

# The source must be committed before a release build.
if [[ "${ALLOW_DIRTY:-0}" != "1" ]]; then
    if ! git diff --quiet || ! git diff --cached --quiet; then
        echo "commit the current work before building a release" >&2
        echo "for a local test build set ALLOW_DIRTY=1" >&2
        exit 1
    fi
fi

# An older build in dist/ must already be archived in TTP-pkgs.
if [[ -d "$DIST" ]]; then
    old=""
    for file in "$DIST"/verbaq_*_amd64.deb; do
        [[ -e "$file" ]] || continue
        base=$(basename "$file")
        version=${base#verbaq_}
        old=${version%%_*}
    done
    if [[ -n "$old" && "$old" != "$VERSION" ]]; then
        archive="$TTP_PKGS/pkgs/verbaq/$old"
        if [[ ! -d "$archive" ]]; then
            echo "previous build $old is not archived in $archive" >&2
            echo "archive it before building $VERSION" >&2
            exit 1
        fi
        echo "previous build $old is archived, clearing dist"
    fi
fi

rm -rf "$DIST"
mkdir -p "$DIST"
cargo build --release
install -m755 target/release/verbaq "$DIST/verbaq"

for format in deb rpm archlinux; do
    case "$format" in
        deb) arch=amd64 ;;
        *) arch=x86_64 ;;
    esac
    VERSION="$VERSION" ARCH="$arch" nfpm package \
        --config packaging/nfpm.yaml \
        --packager "$format" \
        --target "$DIST/"
done

tar -czf "$DIST/verbaq-${VERSION}-linux-x86_64.tar.gz" \
    -C target/release verbaq \
    -C "$PWD" LICENSE README.md CHANGELOG.md

(cd "$DIST" && sha256sum ./* > SHA256SUMS)
ls -lh "$DIST"
