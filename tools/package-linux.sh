#!/usr/bin/env bash
# Packages canopy for Linux: the DAW (canopy), the keyboard-learning app
# (canopy-learn), the MCP bridge for LLM clients (canopy-mcp), the built-in
# instruments as a CLAP plugin (canopy.clap), desktop entries, the icon and
# the .canopy file type.
#
#   tools/package-linux.sh [options]
#
#   --deb            make a Debian/Ubuntu package: dist/canopy_<version>_<arch>.deb
#                    (the default; needs dpkg-deb, and dpkg-shlibdeps from
#                    dpkg-dev to work out the libraries it needs)
#   --install-to D   put the files under D as they'd be installed (D/usr/...)
#                    instead of making a package: what packaging/arch/PKGBUILD's
#                    package() runs
#   --no-build       use what's already built rather than building first
#   --profile P      cargo profile (default: dist, what ships: fat LTO, slow to
#                    build; release is quicker and nearly as fast)
#
# Everything builds into the workspace's own target/ (or $CARGO_TARGET_DIR).
# For Arch Linux, see packaging/arch/PKGBUILD (makepkg -si there).
set -euo pipefail

die() {
    echo "package-linux: $*" >&2
    exit 1
}

usage() {
    sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'
}

mode=deb
destination=
build=true
profile=dist
while (($#)); do
    case "$1" in
        --deb) mode=deb ;;
        --install-to)
            (($# >= 2)) || die "--install-to needs a folder"
            mode=install
            destination=$2
            shift
            ;;
        --install-to=*)
            mode=install
            destination=${1#*=}
            ;;
        --no-build) build=false ;;
        --profile)
            (($# >= 2)) || die "--profile needs a name"
            profile=$2
            shift
            ;;
        --profile=*) profile=${1#*=} ;;
        -h | --help)
            usage
            exit 0
            ;;
        *) die "unknown option $1 (see --help)" ;;
    esac
    shift
done

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"
[[ $(uname -s) == Linux ]] || die "this packages for Linux; see tools/build-windows.sh for Windows"

target_dir=${CARGO_TARGET_DIR:-target}
case "$profile" in
    dev | test) folder=debug ;;
    *) folder=$profile ;;
esac
built=$target_dir/$folder

if $build; then
    cargo build --locked --profile "$profile" -p ui -p learn -p mcp -p canopy-clap
fi
for file in canopy canopy-learn canopy-mcp libcanopy_clap.so; do
    [[ -f $built/$file ]] || die "no $built/$file; build first (or drop --no-build)"
done

# The files, laid out as installed.
stage() {
    local to=$1
    install -Dm755 "$built/canopy" "$to/usr/bin/canopy"
    install -Dm755 "$built/canopy-learn" "$to/usr/bin/canopy-learn"
    install -Dm755 "$built/canopy-mcp" "$to/usr/bin/canopy-mcp"
    # Where CLAP hosts look for plugins installed for everyone.
    install -Dm755 "$built/libcanopy_clap.so" "$to/usr/lib/clap/canopy.clap"
    install -Dm644 packaging/linux/canopy.desktop "$to/usr/share/applications/canopy.desktop"
    install -Dm644 packaging/linux/canopy-learn.desktop \
        "$to/usr/share/applications/canopy-learn.desktop"
    install -Dm644 packaging/linux/canopy.svg \
        "$to/usr/share/icons/hicolor/scalable/apps/canopy.svg"
    install -Dm644 packaging/linux/canopy.xml "$to/usr/share/mime/packages/canopy.xml"
    install -Dm644 README.md "$to/usr/share/doc/canopy/README.md"
}

if [[ $mode == install ]]; then
    [[ -n $destination ]] || die "--install-to needs a folder"
    stage "$destination"
    echo "installed canopy's files under $destination"
    exit 0
fi

command -v dpkg-deb >/dev/null || die "needs dpkg-deb (a Debian or Ubuntu system)"
command -v dpkg-shlibdeps >/dev/null || die "needs dpkg-shlibdeps: sudo apt install dpkg-dev"

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
if commit=$(git rev-parse --short HEAD 2>/dev/null); then
    # Tells builds of the same version apart, and sorts after the plain
    # version as a later build of it.
    version="$version+git$(git log -1 --format=%cd --date=format:%Y%m%d).$commit"
fi
arch=$(dpkg --print-architecture)
work=$built/deb
package=$work/canopy
rm -rf "$work"
mkdir -p "$package/DEBIAN"
stage "$package"
# Stripped as Debian's own packages are (and as makepkg strips Arch's): the
# dist profile's line tables, kept for the Windows build's .pdb, would more
# than triple the size.
strip --strip-unneeded --remove-section=.comment --remove-section=.note \
    "$package/usr/bin/"* "$package/usr/lib/clap/canopy.clap"

# The libraries the programs link to, worked out from the binaries. Those
# opened at run time aren't among them: X11 or Wayland's keyboard handling,
# the GPU's Vulkan or GL driver, and PipeWire (which canopy uses when it's
# running, and plays through ALSA otherwise).
mkdir -p "$work/debian"
echo "Source: canopy" >"$work/debian/control"
linked=$(cd "$work" &&
    dpkg-shlibdeps -O --ignore-missing-info \
        canopy/usr/bin/canopy canopy/usr/bin/canopy-learn canopy/usr/bin/canopy-mcp \
        canopy/usr/lib/clap/canopy.clap 2>/dev/null |
    sed -n 's/^shlibs:Depends=//p')
rm -rf "$work/debian"
depends="$linked, libxkbcommon0, libvulkan1 | libgl1"
recommends="libpipewire-0.3-0t64 | libpipewire-0.3-0, mesa-vulkan-drivers | vulkan-icd, libxkbcommon-x11-0, libxcursor1, libxi6, libxrandr2"
size=$(du -sk --exclude=DEBIAN "$package" | cut -f1)

cat >"$package/DEBIAN/control" <<EOF
Package: canopy
Version: $version
Architecture: $arch
Maintainer: Mateusz Polito <m.polito@hertznt.pl>
Installed-Size: $size
Depends: $depends
Recommends: $recommends
Section: sound
Priority: optional
Homepage: https://github.com/Matipolit/Canopy-vibe
Description: digital audio workstation
 canopy is a DAW: a timeline of MIDI and audio clips, a piano roll, built-in
 instruments (a wavetable synth, drums, a physical guitar, a sampler, a loop
 slicer) and effects, and CLAP, VST3 and VST2 plugins.
 .
 Also included: canopy-learn, for learning to play keyboard with a MIDI
 keyboard; canopy-mcp, which lets LLM clients work in the running canopy; and
 the built-in instruments as a CLAP plugin for other hosts.
EOF
doc=$package/usr/share/doc/canopy
cat >"$doc/copyright" <<EOF
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: canopy
Source: https://github.com/Matipolit/Canopy-vibe

Files: *
Copyright: 2026 Mateusz Polito
License: none
 No licence has been chosen yet; all rights reserved.
EOF
gzip -9n >"$doc/changelog.gz" <<EOF
canopy ($version) unstable; urgency=low

  * Built from ${commit:-a checkout without git}.

 -- Mateusz Polito <m.polito@hertznt.pl>  $(date -R)
EOF
chmod 644 "$doc/copyright" "$doc/changelog.gz"

mkdir -p dist
out=dist/canopy_${version}_$arch.deb
dpkg-deb --build --root-owner-group -Zxz "$package" "$out" >/dev/null
rm -rf "$work"
echo "wrote $out ($(du -h "$out" | cut -f1))"
echo "install it with: sudo apt install ./$out"
