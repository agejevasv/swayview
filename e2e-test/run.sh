#!/bin/bash
# Runs swayview under a headless sway once per pixel format, each time with
# sway made to offer that format for window capture, and saves a screenshot
# and logs per format in $OUT, plus a summary.
set -u

out=${OUT:-/out}
here=$(dirname "$(readlink -f "$0")")
shim=${SHIM:-/usr/local/lib/readformat.so}
formats=${FORMATS:-"xrgb8888 argb8888 xbgr8888 abgr8888 bgr888 rgb565 rgbx4444 rgba4444
    rgbx5551 rgba5551 xbgr2101010 abgr2101010 bgr161616 xbgr16161616 abgr16161616
    bgr161616f xbgr16161616f abgr16161616f"}

export WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=gles2 FAKE_WINDOW_PATTERN=1
export XDG_CONFIG_HOME=$here/config
if compgen -G "/dev/dri/renderD*" >/dev/null; then
    echo "rendering on $(compgen -G '/dev/dri/renderD*' | head -1)"
elif [ -e /dev/udmabuf ]; then
    echo "rendering in software (llvmpipe)"
    export WLR_RENDERER_FORCE_SOFTWARE=1
else
    echo "needs a GPU (--device /dev/dri) or udmabuf for software rendering" \
        "(sudo modprobe udmabuf, then --device /dev/udmabuf)" >&2
    exit 1
fi

mkdir -p "$out"
sway --version >"$out/versions.txt"
pacman -Q mesa 2>/dev/null >>"$out/versions.txt"
: >"$out/summary.txt"

# A wl_shm format code as its four letters, e.g. 875709016 as XB24; ARGB8888
# and XRGB8888 have the codes 0 and 1 instead.
fourcc() {
    local n=$1
    case $n in
    0) printf AR24; return ;;
    1) printf XR24; return ;;
    esac
    printf "\\x$(printf %x $((n & 255)))\\x$(printf %x $((n >> 8 & 255)))"
    printf "\\x$(printf %x $((n >> 16 & 255)))\\x$(printf %x $((n >> 24 & 255)))"
}

# Starts a test window and waits until sway has it, so windows always come in
# the same order, and the last one started has the focus.
open_window() {
    fake-window "$1" "$2" &
    for _ in $(seq 100); do
        swaymsg -t get_tree | grep -q "\"app_id\": \"$1\"" && return 0
        sleep 0.05
    done
    echo "$1 did not appear" >&2
}

wait_for() {
    for _ in $(seq 100); do
        compgen -G "$1" >/dev/null && return 0
        sleep 0.05
    done
    return 1
}

run_one() {
    local f=$1 dir=$out/$1
    mkdir -p "$dir"
    export XDG_RUNTIME_DIR
    XDG_RUNTIME_DIR=$(mktemp -d)
    local pause=$XDG_RUNTIME_DIR/pause
    # Left from the previous run, sway would try to reuse them.
    unset SWAYSOCK WAYLAND_DISPLAY

    E2E_FORMAT=$f E2E_PAUSE=$pause LD_PRELOAD=$shim sway -c "$here/sway.conf" >"$dir/sway.log" 2>&1 &
    local sway=$!
    if ! wait_for "$XDG_RUNTIME_DIR/sway-ipc.*.sock" || ! wait_for "$XDG_RUNTIME_DIR/wayland-[0-9]"; then
        echo "$f: sway did not start, see $f/sway.log" | tee -a "$out/summary.txt"
        kill $sway 2>/dev/null
        return
    fi
    export SWAYSOCK WAYLAND_DISPLAY
    SWAYSOCK=$(compgen -G "$XDG_RUNTIME_DIR/sway-ipc.*.sock" | head -1)
    WAYLAND_DISPLAY=$(basename "$(compgen -G "$XDG_RUNTIME_DIR/wayland-[0-9]" | head -1)")

    open_window tiled-a "Tiled A"
    open_window tiled-b "Tiled B"
    open_window hidden "On a hidden workspace"
    open_window floating "Floating, odd size"
    sleep 0.5

    WAYLAND_DEBUG=1 swayview >"$dir/swayview.debug.log" 2>&1 &
    local swayview=$!
    sleep 2

    touch "$pause"
    grim "$dir/screenshot.png"
    grim -t ppm "$dir/screenshot.ppm"

    kill $swayview 2>/dev/null
    pkill -x fake-window
    swaymsg -q exit 2>/dev/null
    wait 2>/dev/null
    rm -rf "$XDG_RUNTIME_DIR"

    grep -v '^\[' "$dir/swayview.debug.log" >"$dir/swayview.log"
    local offered
    offered=$(grep -o 'shm_format, ([0-9]*)' "$dir/swayview.debug.log" | grep -o '[0-9]*' | sort -u |
        while read -r n; do fourcc "$n"; echo -n " "; done)
    local problems
    problems=$(grep -c . "$dir/swayview.log")
    echo "$f: sway offered ${offered:-nothing}, swayview printed $problems lines" | tee -a "$out/summary.txt"
}

for f in $formats; do
    run_one "$f"
done
python3 "$here/compare.py" "$out" | tee -a "$out/summary.txt"
