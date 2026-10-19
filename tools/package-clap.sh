#!/usr/bin/env bash
# Packages canopy's built-in instruments as one CLAP plugin file for other
# hosts (Bitwig, Reaper, Ardour, FL Studio...): every built-in but the
# Sampler, each listed as "Canopy <name>" (crates/canopy-clap).
#
#   tools/package-clap.sh [options] [-- more cargo arguments]
#
#   --windows      build for 64-bit Windows (from Linux with cargo-xwin, as
#                  build-windows.sh builds canopy.exe; on Windows, natively)
#   --install      also copy it where this machine's hosts look for plugins:
#                  ~/.clap on Linux, %LOCALAPPDATA%\Programs\Common\CLAP on
#                  Windows
#   --profile P    cargo profile (default: release; `dist` is what canopy.exe
#                  ships with, slower to build)
#   --accept-microsoft-license
#                  for --windows from Linux: let cargo-xwin download
#                  Microsoft's C runtime and Windows SDK (see build-windows.sh)
#
# Writes dist/clap/canopy.clap, or dist/clap-windows-x86_64/canopy.clap for
# Windows: a DLL with the C runtime linked in, needing only DLLs that come
# with Windows (checked after building).
set -euo pipefail

MICROSOFT_LICENSE=https://go.microsoft.com/fwlink/?LinkId=2086102

die() {
    echo "package-clap: $*" >&2
    exit 1
}

usage() {
    sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'
}

windows=false
install=false
profile=release
accept_license=false
cargo_args=()
while (($#)); do
    case "$1" in
        --windows) windows=true ;;
        --install) install=true ;;
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
            cargo_args=("$@")
            break
            ;;
        *) die "unknown option $1 (see --help)" ;;
    esac
    shift
done

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

case "$(uname -s)" in
    MINGW* | MSYS* | CYGWIN*) on_windows=true ;;
    *) on_windows=false ;;
esac
if $on_windows; then
    windows=true
fi
if $windows && ! $on_windows && $install; then
    die "--install puts the plugin where this machine's hosts look; copy the Windows one over yourself"
fi

build=(build --profile "$profile" -p canopy-clap)
target_dir=${CARGO_TARGET_DIR:-target}
# Cargo puts the dev and test profiles' output in debug/.
case "$profile" in
    dev | test) folder=debug ;;
    *) folder=$profile ;;
esac

if $windows; then
    target=x86_64-pc-windows-msvc
    library=canopy_clap.dll
    out=dist/clap-windows-x86_64
    build+=(--target "$target")
    built=$target_dir/$target/$folder/$library
    # RUSTFLAGS replaces the target's flags in .cargo/config.toml instead of
    # adding to them, and with them the static C runtime.
    if [[ -n ${RUSTFLAGS:-} || -n ${CARGO_ENCODED_RUSTFLAGS:-} ]]; then
        echo "package-clap: ignoring RUSTFLAGS; it would drop the static C runtime" >&2
        unset RUSTFLAGS CARGO_ENCODED_RUSTFLAGS
    fi
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
    if $on_windows; then
        cargo "${build[@]}" "${cargo_args[@]}"
    else
        command -v cargo-xwin >/dev/null ||
            die "needs cargo-xwin: cargo install --locked cargo-xwin"
        if [[ $(uname -s) == Darwin ]]; then
            cache_home=$HOME/Library/Caches
        else
            cache_home=${XDG_CACHE_HOME:-$HOME/.cache}
        fi
        xwin_cache=${XWIN_CACHE_DIR:-$cache_home/cargo-xwin}/xwin
        if [[ ! -e $xwin_cache/DONE ]] && ! $accept_license; then
            cat >&2 <<EOF
package-clap: cargo-xwin is about to download Microsoft's C runtime and
Windows SDK into $xwin_cache.
Doing so accepts Microsoft's licence for them:

    $MICROSOFT_LICENSE

If you accept it, run this again with --accept-microsoft-license.
EOF
            exit 2
        fi
        cargo xwin "${build[@]}" "${cargo_args[@]}"
    fi
else
    case "$(uname -s)" in
        Darwin) library=libcanopy_clap.dylib ;;
        *) library=libcanopy_clap.so ;;
    esac
    out=dist/clap
    cargo "${build[@]}" "${cargo_args[@]}"
    built=$target_dir/$folder/$library
fi

[[ -f $built ]] || die "cargo built no $built"
mkdir -p "$out"
cp "$built" "$out/canopy.clap"
echo "wrote $out/canopy.clap ($(du -h "$out/canopy.clap" | cut -f1))"

if $windows; then
    objdump=$llvm_bin/llvm-objdump
    "$objdump" -p "$out/canopy.clap" | grep -qw clap_entry ||
        die "$out/canopy.clap doesn't export clap_entry"
    tools/check-windows-dlls.sh "$objdump" "$out/canopy.clap"
fi

if $install; then
    if $on_windows; then
        folder=$(cygpath -u "$LOCALAPPDATA")/Programs/Common/CLAP
    else
        folder=$HOME/.clap
    fi
    mkdir -p "$folder"
    # Through another name, so a host that has the old file loaded keeps it.
    cp "$out/canopy.clap" "$folder/canopy.clap.tmp$$"
    mv "$folder/canopy.clap.tmp$$" "$folder/canopy.clap"
    echo "installed $folder/canopy.clap"
fi
