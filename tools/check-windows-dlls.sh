#!/usr/bin/env bash
# Checks that a Windows .exe or .dll is self-contained: every DLL it imports
# should come with Windows. The C runtime, the C++ runtime, libgcc and
# winpthread are linked in, so their DLLs (and any other library's) mustn't
# show up. Used by build-windows.sh and package-clap.sh.
#
#   tools/check-windows-dlls.sh LLVM-OBJDUMP FILE
#
# Lists what FILE imports, warns of DLLs it doesn't know, and fails if one of
# them doesn't come with Windows.
set -euo pipefail

(($# == 2)) || {
    sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//' >&2
    exit 1
}
objdump=$1
file=$2

dlls=$("$objdump" -p "$file" |
    sed -n 's/^ *DLL Name: *//p' | tr 'A-Z' 'a-z' | sort -u)
not_windows=()
unknown=()
for dll in $dlls; do
    case "$dll" in
        vcruntime*.dll | msvcp*.dll | concrt*.dll | vccorlib*.dll | ucrtbased.dll | \
            msvcr[0-9]*.dll | mfc[0-9]*.dll | libstdc++*.dll | libgcc*.dll | \
            libwinpthread*.dll | libc++*.dll | libunwind*.dll | dxcompiler.dll | dxil.dll)
            not_windows+=("$dll")
            ;;
        api-ms-win-*.dll | ext-ms-*.dll | kernel32.dll | kernelbase.dll | ntdll.dll | \
            user32.dll | gdi32.dll | shell32.dll | shlwapi.dll | ole32.dll | \
            oleaut32.dll | combase.dll | advapi32.dll | comctl32.dll | comdlg32.dll | \
            ws2_32.dll | bcrypt.dll | bcryptprimitives.dll | crypt32.dll | secur32.dll | \
            userenv.dll | dwmapi.dll | uxtheme.dll | imm32.dll | winmm.dll | avrt.dll | \
            d3d12.dll | dxgi.dll | d3dcompiler_47.dll | dcomp.dll | opengl32.dll | \
            propsys.dll | setupapi.dll | cfgmgr32.dll | hid.dll | version.dll | \
            powrprof.dll | dbghelp.dll | msimg32.dll | ucrtbase.dll | msvcrt.dll | \
            mmdevapi.dll | ksuser.dll | winspool.drv | wtsapi32.dll | netapi32.dll | \
            iphlpapi.dll | dwrite.dll | d2d1.dll | windowscodecs.dll | mf*.dll | \
            uiautomationcore.dll)
            ;;
        *) unknown+=("$dll") ;;
    esac
done

echo "It imports:" $dlls
if ((${#unknown[@]})); then
    echo "Not on the list of DLLs known to come with Windows (check them):" "${unknown[@]}"
fi
if ((${#not_windows[@]})); then
    echo "check-windows-dlls: these DLLs don't come with Windows, so $file isn't self-contained: ${not_windows[*]}" >&2
    exit 1
fi
