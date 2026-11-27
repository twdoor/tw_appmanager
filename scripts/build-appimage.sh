#!/bin/sh
set -eu

if [ "$#" -ne 4 ]; then
    printf 'Usage: %s BINARY APPIMAGETOOL RUNTIME OUTPUT_DIRECTORY\n' "$0" >&2
    exit 2
fi

binary=$1
appimagetool=$2
runtime=$3
output=$4
root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
stage=$(mktemp -d)
trap 'rm -rf -- "$stage"' EXIT
appdir=$stage/Twck.AppDir
asset=Twck-x86_64.AppImage

install -Dm755 "$binary" "$appdir/usr/bin/twck"
install -Dm755 "$root/packaging/AppRun" "$appdir/AppRun"
install -Dm644 "$root/packaging/twck.desktop" "$appdir/twck.desktop"
install -Dm644 "$root/twck.svg" "$appdir/twck.svg"
ln -s twck.svg "$appdir/.DirIcon"

# Keep bundled libraries scoped to Twck so launched apps use their own libraries.
dependencies=$(ldd "$binary")
for library in libxkbcommon.so.0 libxkbcommon-x11.so.0 libxcb-xkb.so.1; do
    source=$(printf '%s\n' "$dependencies" | awk -v name="$library" '$1 == name { print $3; exit }')
    if [ ! -f "$source" ]; then
        printf 'Could not find %s for the AppImage.\n' "$library" >&2
        exit 1
    fi
    install -Dm644 "$source" "$appdir/usr/lib/$library"
    patchelf --set-rpath "\$ORIGIN" "$appdir/usr/lib/$library"
done
patchelf --set-rpath "\$ORIGIN/../lib" "$appdir/usr/bin/twck"
desktop-file-validate "$appdir/twck.desktop"

mkdir -p -- "$output"
ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 "$appimagetool" --no-appstream \
    --runtime-file "$runtime" "$appdir" "$output/$asset"
(cd -- "$output" && sha256sum "$asset" > SHA256SUMS)
