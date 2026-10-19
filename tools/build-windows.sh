#!/usr/bin/env bash
# Builds canopy.exe for 64-bit Windows from Linux (or macOS): one executable
# with the C/C++ runtimes linked in, needing only DLLs that come with Windows.
# On Windows itself (Git Bash, as CI runs it), it builds with Visual Studio's
# MSVC directly, and needs neither cargo-xwin nor clang.
#
#   tools/build-windows.sh [options] [-- more cargo arguments]
#
#   --gnu          build with MinGW-w64 instead of MSVC
#   --console      keep the console window, to read what canopy logs
#   --profile P    cargo profile (default: dist; `release` builds faster)
#   --accept-microsoft-license
#                  let cargo-xwin download Microsoft's C runtime and Windows
#                  SDK, which means accepting Microsoft's licence for them:
#                  https://go.microsoft.com/fwlink/?LinkId=2086102
#                  Needed once, until they're in cargo-xwin's cache.
#
# Writes dist/canopy-windows-x86_64/canopy.exe (and canopy.pdb, its debug
# symbols, with MSVC), then lists the DLLs it imports and fails if any of them
# doesn't come with Windows.
#
# MSVC (the default) needs:   clang          (sudo apt install clang)
#                             cargo-xwin     (cargo install --locked cargo-xwin)
# MinGW (--gnu) needs:        mingw-w64      (sudo apt install mingw-w64)
#
# See "Building for Windows" in the README.
set -euo pipefail

MICROSOFT_LICENSE=https://go.microsoft.com/fwlink/?LinkId=2086102

die() {
    echo "build-windows: $*" >&2
    exit 1
}

usage() {
    sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'
}

toolchain=msvc
console=false
profile=dist
accept_license=false
cargo_args=()
while (($#)); do
    case "$1" in
        --gnu) toolchain=gnu ;;
        --console) console=true ;;
        --profile)
            (($# >= 2)) || die "--profile needs a name"
            profile=$2
            shift
            ;;
        --profile=*) profile=${1#*=} ;;
        --accept-microsoft-license) accept_license=true ;;
        -h | --help)
            usage
            exit 0
            ;;
        --)
            shift
            cargo_args+=("$@")
            break
            ;;
        *) die "unknown option $1 (see --help)" ;;
    esac
    shift
done

cd "$(dirname "$0")/.."

case "$(uname -s)" in
    MINGW* | MSYS* | CYGWIN*) on_windows=true ;;
    *) on_windows=false ;;
esac
if $on_windows && [[ $toolchain == gnu ]]; then
    die "--gnu is for building from Linux; on Windows, build with MSVC"
fi

if [[ $toolchain == msvc ]]; then
    target=x86_64-pc-windows-msvc
else
    target=x86_64-pc-windows-gnu
fi
if $console; then
    cargo_args+=(--features console)
fi

# RUSTFLAGS replaces the target's flags in .cargo/config.toml instead of
# adding to them, and with them the static C runtime.
if [[ -n ${RUSTFLAGS:-} || -n ${CARGO_ENCODED_RUSTFLAGS:-} ]]; then
    echo "build-windows: ignoring RUSTFLAGS; it would drop the static C runtime" >&2
    unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS
fi

# Windows' standard library, and LLVM's tools from rustup: llvm-objdump for
# the DLL check below, and the linker and archiver cargo-xwin uses.
if ! rustup target list --installed | grep -qx "$target"; then
    rustup target add "$target"
fi
host=$(rustc -vV | sed -n 's/^host: //p')
sysroot=$(rustc --print sysroot)
if $on_windows; then
    sysroot=$(cygpath -u "$sysroot")
fi
llvm_bin=$sysroot/lib/rustlib/$host/bin
if [[ ! -x $llvm_bin/llvm-objdump && ! -x $llvm_bin/llvm-objdump.exe ]]; then
    rustup component add llvm-tools
fi

build=(build --profile "$profile" --target "$target" -p ui --bin canopy)
if $on_windows; then
    cargo "${build[@]}" "${cargo_args[@]}"
elif [[ $toolchain == msvc ]]; then
    command -v cargo-xwin >/dev/null ||
        die "needs cargo-xwin: cargo install --locked cargo-xwin"
    # ssstretch's C++, which cargo-xwin compiles with clang in MSVC mode.
    command -v clang >/dev/null ||
        die "needs clang for the C++ in ssstretch: sudo apt install clang"
    if [[ $(uname -s) == Darwin ]]; then
        cache_home=$HOME/Library/Caches
    else
        cache_home=${XDG_CACHE_HOME:-$HOME/.cache}
    fi
    xwin_cache=${XWIN_CACHE_DIR:-$cache_home/cargo-xwin}/xwin
    if [[ ! -e $xwin_cache/DONE ]] && ! $accept_license; then
        cat >&2 <<EOF
build-windows: cargo-xwin is about to download Microsoft's C runtime and
Windows SDK into $xwin_cache.
Doing so accepts Microsoft's licence for them:

    $MICROSOFT_LICENSE

If you accept it, run this again with --accept-microsoft-license. Otherwise,
build with --gnu (MinGW-w64, open source), or on Windows itself (see the README).
EOF
        exit 2
    fi
    cargo xwin "${build[@]}" "${cargo_args[@]}"
else
    command -v x86_64-w64-mingw32-g++ >/dev/null ||
        die "needs MinGW-w64: sudo apt install mingw-w64"
    # MinGW keeps debug info inside the .exe (there's no .pdb); leave it out.
    cargo "${build[@]}" --config "profile.$profile.strip=\"debuginfo\"" "${cargo_args[@]}"
fi

built=${CARGO_TARGET_DIR:-target}/$target/$profile
if [[ $profile == dev ]]; then
    built=${CARGO_TARGET_DIR:-target}/$target/debug # the dev profile's old name
fi
out=dist/canopy-windows-x86_64
rm -rf "$out"
mkdir -p "$out"
cp "$built/canopy.exe" "$out/"
if [[ -f $built/canopy.pdb ]]; then
    cp "$built/canopy.pdb" "$out/"
fi

echo
echo "Built $out/canopy.exe ($(du -h "$out/canopy.exe" | cut -f1)) for $target."
tools/check-windows-dlls.sh "$llvm_bin/llvm-objdump" "$out/canopy.exe"
