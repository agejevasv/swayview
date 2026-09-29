# swayview

A workspace overview for sway. It shows every workspace as a box laid out like
your screen, with each window drawn as a box labelled with its app and title.
It reads the layout live over sway IPC. There are no screenshots, no daemon and
no cache.

## Requirements

- sway 1.4 or later (or SwayFX). Other compositors are not supported: swayview
  talks to sway over its IPC socket. Fractional output scales render sharply
  with sway 1.8 or later; older versions round the scale up.
- `libxkbcommon`, which every Wayland desktop has.
- `fc-match` from fontconfig, optional: without it, start-up is slower as
  every installed font is scanned.

## Install

Release builds for x86_64 Linux are on the
[releases page](https://github.com/agejevasv/swayview/releases). They need
glibc 2.35 or later (Ubuntu 22.04, Debian 12, Fedora 36 and newer, Arch).

```sh
tar xzf swayview-v*-x86_64-linux.tar.gz
sudo install -m755 swayview-v*-x86_64-linux/swayview /usr/local/bin/
```

### From source

Needs Rust 1.89 or later (use [rustup](https://rustup.rs) if your
distribution's Rust is older) and the `libxkbcommon` development files
(`libxkbcommon-dev` on Debian and Ubuntu).

```sh
make                 # release build
sudo make install    # to /usr/local/bin/swayview
```

`PREFIX` and `DESTDIR` are honoured (`make install PREFIX=~/.local`), and
`sudo make uninstall` removes it again.

Development:

| Target       | Runs                                          |
|--------------|-----------------------------------------------|
| `make check` | `fmt-check`, `lint` and `test`                |
| `make fmt`   | `cargo fmt`                                   |
| `make lint`  | clippy on all targets with warnings as errors |
| `make test`  | unit tests                                    |

Lints are configured in `Cargo.toml` for the whole workspace: `unsafe` is
forbidden, and clippy's `pedantic` group is on apart from numeric-cast lints
and a couple of style lints that fight geometry code.

`tools/fake-window` opens a blank window with a chosen app_id and title, for
building test layouts in a headless sway. The trees in `tests/fixtures` were
made that way, with `swaymsg -t get_tree`.

```sh
cargo run -p fake-window -- firefox "GitHub - Mozilla Firefox"
```

## Releases

Pushing a tag that matches the version in `Cargo.toml`, like `v0.1.0`, runs
the checks, builds the x86_64 binary and publishes it as a GitHub release.

## sway config

```
bindsym $mod+Tab exec pkill -x swayview || swayview
```

Pressing the binding again closes the overview. While open, swayview takes
all keyboard input; this binding still works, as sway handles its own bindings
first. Warnings (for example about `theme.yaml`) go to stderr, which is sway's
log when started from a binding; run `swayview` in a terminal to see them.

## Use

| Input               | Action                              |
|---------------------|-------------------------------------|
| click window        | focus it                            |
| click anywhere else | close                               |
| arrows / hjkl       | move selection, on to other outputs |
| Tab / Shift+Tab     | next / previous window, all outputs |
| Enter               | focus selected window               |
| 1 … 9, 0            | switch to workspace number (0 = 10) |
| Esc                 | close                               |

A fullscreen window is drawn in its place in the layout, so the windows
behind it stay reachable; picking one of them ends the fullscreen first, as
sway will not focus a window hidden behind it.

Each output shows its own workspaces, rendered at that output's scale
(fractional scales need `wp_fractional_scale_v1`, sway 1.8 or later).


## Theme

Windows show their app name, then the title and any of `float`,
`fullscreen` and `sticky`. The selection starts on the focused window and
moves with the keys and mouse; it and its workspace number use the active
color.

By default window colors come from `client.focused` (selected),
`client.unfocused` and `client.urgent` in the config sway loaded, following
`set $var` and `include`. Everything else has built-in defaults.

Any color can be set in `~/.config/swayview/theme.yaml` (or
`$XDG_CONFIG_HOME/swayview/theme.yaml`). Every key is optional; unset keys keep
their default. Colors are `"#rrggbb"` or `"#rrggbbaa"` and must be quoted,
since `#` starts a YAML comment. Unknown keys are reported and skipped; a file
that cannot be read or parsed is reported and ignored. Warnings go to stderr,
which is sway's log when started from a binding, so run `swayview` in a
terminal to see them.

```yaml
backdrop: "#101216e0"      # behind everything
output_name: "#8a93a5"     # e.g. "eDP-1", top left
workspace:
  fill: "#16181d"
  label: "#dde1e8"         # workspace number
  selected: "#285577"      # number of the selection's workspace
                           # (default: client.focused background)
  urgent: "#900000"        # number of a workspace with an urgent window
                           # (default: client.urgent background)
window:
  normal:                  # default: client.unfocused
    border: "#333333"
    background: "#222222"
    text: "#888888"
  selected:                # default: client.focused
    border: "#4c7899"
    background: "#285577"
    text: "#ffffff"
  urgent:                  # default: client.urgent
    border: "#2f343a"
    background: "#900000"
    text: "#ffffff"
```
