# End-to-end test of capture pixel formats

Sway offers window captures in one pixel format: whichever its GPU driver
reads fastest. This runs real sway on a headless output once per format that
wlroots' OpenGL renderer can offer, with swayview capturing four test windows,
and saves a screenshot of each run.

A small preloaded library, `readformat.c`, makes sway's renderer report the
chosen format and converts pixels into it, so every format can be tried on any
driver. What it tests is swayview and wlroots handling the format, from the
capture to sway showing the buffer again; not the driver's own conversion.

## Run

From the repo root:

```sh
docker build -f e2e-test/Dockerfile -t swayview-e2e .
docker run --rm --device /dev/dri -v "$PWD/e2e-test/out:/out" swayview-e2e
```

Without a GPU in the container, sway renders in software, which needs udmabuf:

```sh
sudo modprobe udmabuf
docker run --rm --device /dev/udmabuf -v "$PWD/e2e-test/out:/out" swayview-e2e
```

`FORMATS="bgr888 rgb565"` (with `-e`) runs only those.

## Results

In `e2e-test/out`:

- `summary.txt`: per format, what sway offered and how many lines swayview
  printed (it prints only on problems), then whether its screenshot matches
  the `xrgb8888` one, pixel by pixel.
- `<format>/screenshot.png`: the overview; `screenshot.ppm` is the same,
  uncompressed, for the comparison.
- `<format>/swayview.log`, `sway.log`: the logs, and `swayview.debug.log` with
  the Wayland messages.
- `versions.txt`: sway and Mesa versions.

Each thumbnail shows the test pattern: red, green and blue quarters, and
black and white diagonal stripes. Swapped colors mean a wrong byte order;
broken or slanted stripes mean rows of the wrong length. The floating window
is 401×301, so in 2-, 3- and 6-byte formats its rows need padding. The window
on workspace 2 is hidden when captured. Windows open one at a time, so every
run has the same layout and the comparison can be exact; formats with fewer
bits per channel, like `rgb565`, may round colors a little.
