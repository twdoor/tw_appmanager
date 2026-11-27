#!/bin/sh
set -eu

if [ "${1:-}" = "--help" ]; then
    printf 'Usage: sh install.sh [vVERSION]\nInstall the latest stable Twck release, or select a release tag.\n'
    exit 0
fi

fail() {
    printf 'Twck installer: %s\n' "$*" >&2
    exit 1
}

[ "$#" -le 1 ] || fail 'Usage: sh install.sh [vVERSION]'
[ "$(uname -s)" = Linux ] && [ "$(uname -m)" = x86_64 ] ||
    fail 'Twck releases are available for x86-64 Linux.'
[ "$(id -u)" -ne 0 ] || fail 'Run the installer as your regular user.'

releases=https://github.com/twdoor/twck/releases
release=$releases/latest/download
if [ "$#" -eq 1 ]; then
    case "$1" in
        *[!a-zA-Z0-9.+-]*) fail 'Invalid release tag.' ;;
        v[0-9]*) release=$releases/download/$1 ;;
        *) fail 'Use a release tag such as v0.2.0 or v0.2.0-beta.1.' ;;
    esac
fi

bin_dir=$HOME/.local/bin
target=$bin_dir/twck
asset=Twck-x86_64.AppImage
mkdir -p -- "$bin_dir"
stage=$(mktemp -d "$bin_dir/.twck-install.XXXXXX")
rollback=false

cleanup() {
    if [ "$rollback" = true ] && [ ! -e "$stage/$asset" ]; then
        if [ -e "$stage/previous" ] || [ -L "$stage/previous" ]; then
            if ! mv -fT -- "$stage/previous" "$target"; then
                printf 'Could not restore the previous installation. Backup kept at %s\n' \
                    "$stage/previous" >&2
                return
            fi
        else
            rm -f -- "$target"
        fi
    fi
    rm -rf -- "$stage"
}

trap cleanup EXIT
trap 'exit 1' HUP INT TERM

download() {
    curl --disable --fail --silent --show-error --location \
        --proto '=https' --proto-redir '=https' --connect-timeout 15 \
        --max-time 300 --retry 2 --output "$stage/$1" "$release/$1"
}

download SHA256SUMS
download "$asset"
read -r expected filename < "$stage/SHA256SUMS"
actual=$(sha256sum -- "$stage/$asset")
[ "$filename" = "$asset" ] && [ "$expected" = "${actual%% *}" ] ||
    fail 'The AppImage checksum did not match.'
chmod 755 -- "$stage/$asset"
version=$("$stage/$asset" --version) || fail 'The downloaded AppImage could not run.'

# Serialize replacement and desktop registration across installer invocations.
exec 9<"$bin_dir"
flock 9
if [ -e "$target" ] || [ -L "$target" ]; then
    [ -f "$target" ] || [ -L "$target" ] || fail "$target is not a file."
    ln -P -- "$target" "$stage/previous"
fi
rollback=true
mv -fT -- "$stage/$asset" "$target"
"$target" install-desktop || fail 'Desktop registration failed.'
rollback=false

printf 'Installed %s at %s\n' "$version" "$target"
case :$PATH: in
    *:"$bin_dir":*) ;;
    *) printf 'Add %s to your PATH to use twck from the terminal.\n' "$bin_dir" ;;
esac
