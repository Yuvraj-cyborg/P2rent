#!/bin/sh
# Installs mux on macOS and Linux: https://github.com/yuvichh/mux
#
#     curl -fsSL https://raw.githubusercontent.com/yuvichh/mux/main/install.sh | sh
#
# It downloads the build for this machine from the project's releases,
# checks it against the release's list of SHA-256 digests, and puts it in
# ~/.local/bin. Run it again to upgrade. To undo it, delete that one file.
#
# These variables change what it does:
#
#     MUX_VERSION   the release to install, as v0.3.0   [the latest]
#     MUX_INSTALL   the folder to put `mux` in          [~/.local/bin]
#
# as in:
#
#     curl -fsSL <the address above> | MUX_INSTALL=/usr/local/bin sh

set -eu

home=https://github.com/yuvichh/mux
# Where the releases are. Another address is for trying the script out.
releases=${MUX_RELEASES:-$home/releases}

say() {
    printf '%s\n' "$*"
}

fail() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

have() {
    command -v "$1" >/dev/null 2>&1
}

# The name the release gives the build for this machine.
build() {
    system=$(uname -s)
    machine=$(uname -m)
    case "$system" in
    Darwin)
        # A shell that runs translated on Apple silicon says x86_64.
        if [ "$(sysctl -n hw.optional.arm64 2>/dev/null || true)" = 1 ]; then
            machine=arm64
        fi
        case "$machine" in
        arm64 | x86_64) ;;
        *) machine= ;;
        esac
        ;;
    Linux)
        case "$machine" in
        aarch64 | arm64) machine=aarch64 ;;
        x86_64 | amd64) machine=x86_64 ;;
        *) machine= ;;
        esac
        # The builds need the GNU C library, 2.17 or newer.
        if have ldd && ldd --version 2>&1 | grep -qi musl; then
            fail "this system uses the musl C library, and the builds of mux need glibc. It can be built from source: $home"
        fi
        ;;
    *)
        machine=
        ;;
    esac
    if [ -z "$machine" ]; then
        fail "there is no build of mux for $system on $(uname -m). It can be built from source: $home"
    fi
    say "mux-$system-$machine"
}

# Fetches the address $1 into the file $2.
fetch() {
    if have curl; then
        curl --fail --silent --show-error --location --output "$2" "$1"
    elif have wget; then
        wget --quiet --output-document="$2" "$1"
    else
        fail "neither curl nor wget is here to download with"
    fi
}

# The SHA-256 digest of the file $1.
digest() {
    if have sha256sum; then
        sha256sum "$1" | cut -d ' ' -f 1
    elif have shasum; then
        shasum -a 256 "$1" | cut -d ' ' -f 1
    else
        fail "neither sha256sum nor shasum is here to check the download with"
    fi
}

main() {
    name=$(build)
    into=${MUX_INSTALL:-$HOME/.local/bin}
    case "${MUX_VERSION:-}" in
    '') from="$releases/latest/download" ;;
    *) from="$releases/download/$MUX_VERSION" ;;
    esac

    work=$(mktemp -d)
    # The download, and the copy of it that is made beside where it goes.
    new="$into/.mux-new-$$"
    trap 'rm -rf "$work" "$new"' EXIT

    say "downloading $name (${MUX_VERSION:-the latest release})"
    fetch "$from/$name" "$work/mux" ||
        fail "could not download $from/$name"
    fetch "$from/checksums-sha256.txt" "$work/checksums" ||
        fail "could not download the release's list of digests"

    # A line of the list is a digest, two spaces and a name.
    wanted=$(awk -v name="$name" '$2 == name { print $1 }' "$work/checksums")
    [ -n "$wanted" ] || fail "the release's list of digests does not name $name"
    got=$(digest "$work/mux")
    if [ "$got" != "$wanted" ]; then
        fail "the download does not have the digest the release gives for it, and was not installed.
  expected $wanted
  got      $got"
    fi

    chmod 755 "$work/mux"
    version=$("$work/mux" --version) ||
        fail "the download does not run on this machine, and was not installed"

    mkdir -p "$into" || fail "cannot make the folder $into"
    # Copied beside where it goes and renamed, so that a `mux` that is
    # running is not written over, and one that is half written is never
    # there under its name.
    { cp "$work/mux" "$new" && mv -f "$new" "$into/mux"; } ||
        fail "cannot write to $into. Another folder can be named: MUX_INSTALL=/some/folder"

    say "installed $version at $into/mux"
    case ":$PATH:" in
    *":$into:"*) ;;
    *)
        say ""
        say "$into is not on your PATH. To run mux by its name, add this line to"
        say "your shell's start-up file, and open a new terminal:"
        say ""
        say "    export PATH=\"$into:\$PATH\""
        ;;
    esac
    say ""
    say "To remove it again: rm $into/mux"
}

# Everything above is only read until this line: a download of this script
# that was cut short runs nothing.
main "$@"
