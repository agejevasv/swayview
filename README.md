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

Lints are configured in `Cargo.toml`: `unsafe` is forbidden, and clippy's
`pedantic` group is on apart from numeric-cast lints and a couple of
style lints that fight geometry code.

## sway config

```
bindsym $mod+Tab exec pkill -x swayview || swayview
```

Pressing the binding again closes the overview.

## Use

| Input               | Action                              |
|---------------------|-------------------------------------|
| click window        | focus it                            |
| click workspace     | switch to it                        |
| click background    | close                               |
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


## Colors

Window colors come from `client.focused`, `client.focused_inactive`,
`client.unfocused` and `client.urgent` in the config sway loaded. `set $var` and `include` (with
`~`, environment variables and `*` globs) are followed. If a color is unset,
malformed or unreadable, sway's built-in default is used instead.

| Element           | Color                                  |
|-------------------|----------------------------------------|
| window            | `unfocused` border / background / text |
| focused window    | `focused` border / background / text   |
| hovered window    | `focused_inactive` background / text   |
| keyboard selection| the app's stripe color (outline)       |
| focused workspace | `focused` border                       |
| urgent window     | `urgent` border / background / text    |
| urgent workspace  | `urgent` background (outline, number)  |

The backdrop and workspace boxes use fixed neutral greys. Each window also
gets a stripe on its left edge in a color picked from its app name, the same
for every window of that app.

Windows show their title, then the app name and any of `float`,
`fullscreen` and `sticky`.

