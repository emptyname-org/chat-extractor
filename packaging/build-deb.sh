#!/bin/sh
set -eu

project_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$project_root"

version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)
test -n "$version"
package_version="${version}-1"
architecture=$(dpkg --print-architecture)
source_date_epoch=${SOURCE_DATE_EPOCH:-1787184000}
output_directory="$project_root/dist"
output_file="$output_directory/chatextractor_${package_version}_${architecture}.deb"

cargo_source_root=${CARGO_HOME:-"$HOME/.cargo"}
remap_flags="--remap-path-prefix=$project_root=. --remap-path-prefix=$cargo_source_root=/cargo"
RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }$remap_flags" cargo build --release --locked
binary="$project_root/target/release/chatextractor"
test -x "$binary"

package_root="$project_root/debian/chatextractor"
if [ -e "$package_root" ]; then
    printf 'Refusing to replace existing packaging directory: debian/chatextractor\n' >&2
    exit 1
fi
cleanup() {
    case "$package_root" in
        "$project_root/debian/chatextractor") rm -rf -- "$package_root" ;;
    esac
}
trap cleanup EXIT HUP INT TERM

install -d -m 0755 \
    "$package_root/DEBIAN" \
    "$package_root/usr/bin" \
    "$package_root/usr/share/applications" \
    "$package_root/usr/share/doc/chatextractor" \
    "$package_root/usr/share/icons/hicolor/scalable/apps" \
    "$package_root/usr/share/metainfo"
install -m 0755 "$binary" "$package_root/usr/bin/chatextractor"
install -m 0644 packaging/org.chatextractor.app.desktop \
    "$package_root/usr/share/applications/org.chatextractor.app.desktop"
install -m 0644 packaging/org.chatextractor.app.metainfo.xml \
    "$package_root/usr/share/metainfo/org.chatextractor.app.metainfo.xml"
install -m 0644 assets/signal-chat-export-icon.svg \
    "$package_root/usr/share/icons/hicolor/scalable/apps/chatextractor.svg"
install -m 0644 debian/copyright "$package_root/usr/share/doc/chatextractor/copyright"
gzip -n -9 -c debian/changelog > "$package_root/usr/share/doc/chatextractor/changelog.Debian.gz"
gzip -n -9 -c README.md > "$package_root/usr/share/doc/chatextractor/README.md.gz"

shlibs_output=$(dpkg-shlibdeps -O -e"$package_root/usr/bin/chatextractor")
depends=${shlibs_output#shlibs:Depends=}
test -n "$depends"
installed_size=$(du -sk "$package_root/usr" | cut -f1)

cat > "$package_root/DEBIAN/control" <<EOF
Package: chatextractor
Version: $package_version
Section: utils
Priority: optional
Architecture: $architecture
Maintainer: Chat Extractor for Signal contributors <contributors@invalid.example>
Installed-Size: $installed_size
Depends: $depends
Description: offline chat extractor for Signal exports
 Select chats from a Signal Desktop plaintext export by date and write Markdown
 or JSON. Referenced media can be copied. Processing stays local.
EOF

private_path_pattern='/(ho''me|Users)/[^/[:space:]]+|/run/us''er/[0-9]+'
if LC_ALL=C grep -aREq "$private_path_pattern" "$package_root"; then
    printf 'Package privacy audit failed: private-machine paths are embedded in the package.\n' >&2
    exit 1
fi

find "$package_root" -exec touch --no-dereference --date="@$source_date_epoch" {} +
mkdir -p "$output_directory"
dpkg-deb --root-owner-group --build "$package_root" "$output_file"
printf 'Built %s\n' "${output_file#"$project_root/"}"
