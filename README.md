# swayview

A workspace overview for sway. It shows every workspace as a box laid out like
your screen, with each window drawn as a box labelled with its app and title.
It reads the layout live over sway IPC. There are no screenshots, no daemon and
no cache.

## Build and install

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

## sway config

```
bindsym $mod+Tab exec pkill -x swayview || swayview
```

Pressing the binding again closes the overview.

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

Windows show their title, then the app name and any of `float`,
`fullscreen` and `sticky`, with a stripe on the left edge in a color picked
from the app name (the same for every window of that app). The selection
starts on the focused window and moves with the keys and mouse; it and its
workspace number use the active color.

By default window colors come from `client.focused` (selected),
`client.unfocused` and `client.urgent` in the config sway loaded, following
`set $var` and `include`. Everything else has built-in defaults.

Any color can be set in `~/.config/swayview/theme.yaml` (or
`$XDG_CONFIG_HOME/swayview/theme.yaml`). Every key is optional; unset keys keep
their default. Colors are `"#rrggbb"` or `"#rrggbbaa"` and must be quoted,
since `#` starts a YAML comment. A file that cannot be read is reported and
ignored.

```yaml
backdrop: "#101216e0"      # behind everything
output_name: "#8a93a5"     # e.g. "eDP-1", top left
workspace:
  fill: "#16181d"
  border: "#3a3f4b"
  visible: "#6b7385"       # border of a workspace shown on its output
  label: "#dde1e8"         # workspace number
  selected: "#285577"      # number and border of the selection's workspace
                           # (default: client.focused background)
  urgent: "#900000"        # default: client.urgent background
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
app_colors:                # stripe colors apps are hashed into
  - "#e06c75"
  - "#e8915a"
  - "#e5c07b"
  - "#b5d468"
  - "#98c379"
  - "#5fc9a4"
  - "#56b6c2"
  - "#61afef"
  - "#8a8cf0"
  - "#c678dd"
  - "#e87fd0"
  - "#f78fb3"
```
