mod color;
mod config;
mod fonts;
mod input;
mod layout;
mod model;
mod render;
mod sway;
mod sway_config;
mod wayland;

use std::fmt::Display;

use anyhow::{Context, Result, bail, ensure};

use crate::config::Config;
use crate::model::Tree;
use crate::render::{Renderer, View};

const USAGE: &str = "\
usage: swayview
       swayview --png OUT [--tree TREE.json] [--output NAME] [--size WxH]

Each output shows its own workspaces.

  --png OUT      render to a PNG instead of opening the overlay
  --tree FILE    read `swaymsg -t get_tree` output instead of asking sway
  --output NAME  output to render (default: focused)
  --size WxH     image size (default: the output's size)";

#[derive(Debug, Default)]
struct Args {
    png: Option<String>,
    tree: Option<String>,
    output: Option<String>,
    size: Option<(u32, u32)>,
}

/// Reports a non-fatal error.
pub fn warn(e: impl Display) {
    eprintln!("swayview: {e:#}");
}

fn parse_args() -> Result<Args> {
    let mut a = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = || it.next().with_context(|| format!("{arg} needs a value"));
        match arg.as_str() {
            "--png" => a.png = Some(value()?),
            "--tree" => a.tree = Some(value()?),
            "--output" => a.output = Some(value()?),
            "--size" => a.size = Some(parse_size(&value()?).context("--size")?),
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            _ => bail!("unknown argument {arg:?}\n\n{USAGE}"),
        }
    }
    Ok(a)
}

fn parse_size(s: &str) -> Result<(u32, u32)> {
    let (w, h) = s.split_once('x').with_context(|| format!("expected WxH, got {s:?}"))?;
    let w: u32 = w.parse().with_context(|| format!("bad width {w:?}"))?;
    let h: u32 = h.parse().with_context(|| format!("bad height {h:?}"))?;
    ensure!(w > 0 && h > 0, "size must be non-zero, got {s:?}");
    Ok((w, h))
}

fn render_png(args: &Args, out: &str) -> Result<()> {
    let tree = match &args.tree {
        Some(path) => Tree::from_json(&std::fs::read(path).with_context(|| format!("reading {path}"))?)?,
        None => sway::Ipc::connect()?.get_tree()?,
    };
    let sway_config = sway::Ipc::connect().and_then(|mut ipc| ipc.config_path()).ok();
    let config = Config::load(sway_config.as_deref());
    let output = match &args.output {
        Some(name) => tree.output(name),
        None => tree.focused_output().or(tree.outputs.first()),
    }
    .context("no such output")?;
    let (w, h) = args.size.unwrap_or((output.rect.w as u32, output.rect.h as u32));
    let scene = layout::build(output, w as f32, h as f32);
    let selected = scene.focused_window();
    let view = View { selected, selected_workspace: scene.selected_workspace(selected) };
    let pix = Renderer::new(config).draw(&scene, &view, w, h, 1.0).context("image size is zero")?;
    pix.save_png(out).with_context(|| format!("writing {out}"))?;
    Ok(())
}

fn main() -> Result<()> {
    let args = parse_args()?;
    match &args.png {
        Some(out) => render_png(&args, out),
        None => wayland::run(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(parse_size("1920x1080").unwrap(), (1920, 1080));
        let err = |s| format!("{:#}", parse_size(s).unwrap_err());
        assert!(err("0x100").contains("non-zero"));
        assert!(err("10xabc").contains("bad height \"abc\""));
        assert!(err("10").contains("expected WxH"));
    }
}
