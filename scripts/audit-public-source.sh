#!/usr/bin/env bash
set -euo pipefail

project_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$project_root"

file_list=$(mktemp "${TMPDIR:-/tmp}/chatextractor-public-files.XXXXXX")
cleanup() {
    rm -f -- "$file_list"
}
trap cleanup EXIT HUP INT TERM

find . \
    -path './.git' -prune -o \
    -path './target' -prune -o \
    -path './dist' -prune -o \
    -path './debian/.debhelper' -prune -o \
    -path './debian/cargo-home' -prune -o \
    -path './debian/chatextractor' -prune -o \
    -type f -print0 > "$file_list"

unexpected=0
files=0
while IFS= read -r -d '' path; do
    files=$((files + 1))
    case "$path" in
        ./.gitattributes|./.gitignore|./build.rs|./Cargo.lock|./Cargo.toml|./LICENSE|./Makefile|./README.md|./SECURITY.md) ;;
        ./.github/workflows/*.yml) ;;
        ./assets/resources.gresource.xml|./assets/signal-chat-export-icon.png|./assets/signal-chat-export-icon.svg) ;;
        ./debian/changelog|./debian/control|./debian/copyright|./debian/rules|./debian/source/format) ;;
        ./docs/*.md|./packaging/*.desktop|./packaging/*.sh|./packaging/*.xml|./scripts/*.sh|./src/*.rs|./tests/*.rs) ;;
        *) unexpected=$((unexpected + 1)) ;;
    esac
done < "$file_list"

if [ "$unexpected" -ne 0 ]; then
    printf 'Public-source audit failed: %s unexpected file(s).\n' "$unexpected" >&2
    exit 1
fi

private_path_pattern='/(ho''me|Users)/[^/[:space:]]+|/run/us''er/[0-9]+'
matches=0
while IFS= read -r -d '' path; do
    if LC_ALL=C grep -aEq "$private_path_pattern" "$path"; then
        matches=$((matches + 1))
    fi
done < "$file_list"

if [ "$matches" -ne 0 ]; then
    printf 'Public-source privacy audit failed: private-machine paths found in %s file(s).\n' "$matches" >&2
    exit 1
fi

if LC_ALL=C grep -aEiq 'sodipodi:docname|inkscape:export-filename' \
    assets/signal-chat-export-icon.svg; then
    printf 'Public-source privacy audit failed: editor filename metadata remains in the SVG icon.\n' >&2
    exit 1
fi

printf 'Public-source privacy audit passed: %s allowlisted files and no private-machine paths.\n' "$files"
