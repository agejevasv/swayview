#!/bin/sh
# Installs the latest swayview release binary to /usr/local/bin:
#
#     curl -fsSL https://raw.githubusercontent.com/agejevasv/swayview/main/install.sh | sh
#
# To install to ~/.local/bin instead, end the command with `| PREFIX=~/.local sh`.
set -eu

repo=agejevasv/swayview

die() {
    echo "swayview: $*" >&2
    exit 1
}

# The release binary is built on Ubuntu 22.04, so it runs only on glibc 2.35+.
check_system() {
    [ "$(uname -sm)" = "Linux x86_64" ] ||
        die "release binaries are for x86_64 Linux only; build from source instead"
    v=$(getconf GNU_LIBC_VERSION 2>/dev/null) && [ -n "$v" ] ||
        die "needs glibc 2.35 or later; build from source instead"
    v=${v#glibc }
    major=${v%%.*}
    minor=${v#*.}
    minor=${minor%%.*}
    if [ "$major" -lt 2 ] || { [ "$major" -eq 2 ] && [ "$minor" -lt 35 ]; }; then
        die "needs glibc 2.35 or later, found $v; build from source instead"
    fi
}

main() {
    check_system

    # releases/latest redirects to .../releases/tag/<latest tag>.
    url=$(curl -fsSLo /dev/null -w '%{url_effective}' "https://github.com/$repo/releases/latest")
    tag=${url##*/}
    case $tag in
    v*) ;;
    *) die "could not find the latest release" ;;
    esac

    name=swayview-$tag-x86_64-linux
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    cd "$tmp"
    curl -fsSLO "https://github.com/$repo/releases/download/$tag/$name.tar.gz"
    curl -fsSLO "https://github.com/$repo/releases/download/$tag/$name.tar.gz.sha256"
    sha256sum -c "$name.tar.gz.sha256" >/dev/null || die "checksum mismatch for $name.tar.gz"
    tar xzf "$name.tar.gz"

    bindir=${PREFIX:-/usr/local}/bin
    if mkdir -p "$bindir" 2>/dev/null && [ -w "$bindir" ]; then
        sudo=
    else
        sudo=$(command -v sudo || command -v doas) ||
            die "$bindir is not writable and neither sudo nor doas is installed"
        $sudo mkdir -p "$bindir"
    fi
    $sudo install -m 755 "$name/swayview" "$bindir/swayview"
    echo "Installed swayview $tag to $bindir/swayview"
}

main
