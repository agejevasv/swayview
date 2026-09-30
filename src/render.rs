//! Draws a `Scene` into a pixmap with tiny-skia and cosmic-text: whole, or,
//! for thumbnails, its workspaces and one overlay per window.

use cosmic_text::{
    Attrs, Buffer, Ellipsize, EllipsizeHeightLimit, Family, FontSystem, Metrics, Shaping, SwashCache, Weight,
    Wrap,
};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, PremultipliedColorU8, Stroke, Transform};

use crate::color::Rgba;
use crate::config::{Colors, Config, Font, Fonts};
use crate::fonts;
use crate::layout::{HEADER, Scene, WinItem};
use crate::model::Rect;

/// Alpha of the title line relative to the app line.
const TITLE_FADE: u8 = 0xb0;
/// Alpha of the selected window's color over its thumbnail.
const SELECTED_TINT: u8 = 0x80;

const PAD: f32 = 6.0;
const WINDOW_RADIUS: f32 = 4.0;
/// The border's outer edge is rounder than the window by half its width, up
/// to 1; outside this, the overlay covers a thumbnail's corners.
const THUMB_RADIUS: f32 = WINDOW_RADIUS + 1.0;
const LINE_HEIGHT: f32 = 1.3;
const MIN_TEXT_WIDTH: f32 = 12.0;

const WS_LABEL_SIZE: f32 = 16.0;
const OUTPUT_LABEL_SIZE: f32 = 13.0;

#[derive(Clone, Copy, Debug)]
struct TextStyle<'a> {
    family: &'a str,
    size: f32,
    bold: bool,
    color: Rgba,
}

impl<'a> TextStyle<'a> {
    fn line_height(&self) -> f32 {
        self.size * LINE_HEIGHT
    }

    /// For a line `line_h` high, at `scale`.
    fn attrs(&self, line_h: f32, scale: f32) -> Attrs<'a> {
        let weight = if self.bold { Weight::BOLD } else { Weight::NORMAL };
        let metrics = Metrics::new(self.size * scale, line_h * scale);
        Attrs::new().family(family(self.family)).weight(weight).color(self.color.into()).metrics(metrics)
    }
}

impl Font {
    fn style(&self, size: f32, bold: bool, color: Rgba) -> TextStyle<'_> {
        TextStyle { family: &self.family, size, bold, color }
    }
}

/// What a window's overlay from `Renderer::draw_tile` goes over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Behind {
    /// Nothing yet: the overlay is the whole box.
    Nothing,
    Thumbnail,
}

#[derive(Debug)]
pub struct View {
    /// The selected window, if it is on this scene.
    pub selected: Option<usize>,
    pub selected_workspace: Option<usize>,
}

pub struct Renderer {
    text: Text,
    /// With the family names fontconfig resolved.
    fonts: Fonts,
    colors: Colors,
}

struct Text {
    fonts: FontSystem,
    cache: SwashCache,
}

impl Renderer {
    pub fn new(config: Config) -> Self {
        let Config { mut fonts, colors, .. } = config;
        let (system, [app, title]) = fonts::load([(&fonts.app.family, true), (&fonts.title.family, false)]);
        (fonts.app.family, fonts.title.family) = (app, title);
        Renderer { text: Text { fonts: system, cache: SwashCache::new() }, fonts, colors }
    }

    /// Renders at `scale` physical pixels per logical pixel; `None` if `w` or `h` is 0.
    pub fn draw(&mut self, scene: &Scene, view: &View, w: u32, h: u32, scale: f32) -> Option<Pixmap> {
        let mut pix = self.draw_workspaces(scene, view, w, h, scale)?;
        for (i, win) in scene.windows.iter().enumerate() {
            self.draw_window(&mut pix, win, win.rect, view.selected == Some(i), Behind::Nothing, scale);
        }
        Some(pix)
    }

    /// Like `draw`, without the windows, which are drawn with `draw_tile`.
    pub fn draw_workspaces(
        &mut self,
        scene: &Scene,
        view: &View,
        w: u32,
        h: u32,
        scale: f32,
    ) -> Option<Pixmap> {
        let mut pix = Pixmap::new(w, h)?;
        let (fonts, colors) = (&self.fonts, &self.colors);
        pix.fill(colors.backdrop.into());
        let t = Transform::from_scale(scale, scale);
        let output_name = fonts.title.style(OUTPUT_LABEL_SIZE, false, colors.output_name);
        self.text.draw(&mut pix, &[(&scene.output_name, output_name)], scene.output_label, scale);

        let c = &colors.workspace;
        for (i, ws) in scene.workspaces.iter().enumerate() {
            // Only the number marks the selected or urgent workspace.
            let label = if view.selected_workspace == Some(i) {
                c.selected
            } else if ws.urgent {
                c.urgent
            } else {
                c.label
            };
            fill(&mut pix, ws.rect, (6.0, 6.0), c.fill, t);
            let r = Rect::new(ws.header.x + 2.0, ws.header.y, ws.header.w - 4.0, HEADER);
            self.text.draw(&mut pix, &[(&ws.name, fonts.app.style(WS_LABEL_SIZE, true, label))], r, scale);
        }
        Some(pix)
    }

    /// Window `win` alone, filling a `w`×`h` pixmap. Over a thumbnail, only
    /// what goes on top of it: border, text on a bar in the window color, the
    /// selected tint, and corners in the workspace color, which round it off.
    pub fn draw_tile(
        &mut self,
        win: &WinItem,
        selected: bool,
        behind: Behind,
        (w, h): (u32, u32),
        scale: f32,
    ) -> Option<Pixmap> {
        let mut pix = Pixmap::new(w, h)?;
        let r = Rect::new(0.0, 0.0, w as f32 / scale, h as f32 / scale);
        self.draw_window(&mut pix, win, r, selected, behind, scale);
        Some(pix)
    }

    /// Draws `win` in `r`.
    fn draw_window(
        &mut self,
        pix: &mut Pixmap,
        win: &WinItem,
        r: Rect,
        selected: bool,
        behind: Behind,
        scale: f32,
    ) {
        let (fonts, colors) = (&self.fonts, &self.colors);
        let t = Transform::from_scale(scale, scale);
        let class = if selected {
            colors.window.selected
        } else if win.urgent {
            colors.window.urgent
        } else {
            colors.window.normal
        };
        let (border, width) = (class.border, if selected { 2.0 } else { 1.0 });
        let rounded = (WINDOW_RADIUS, WINDOW_RADIUS);

        let inner = r.inset(PAD);
        let app = fonts.app.style(fonts.app.size, true, class.text);
        let title = fonts.title.style(fonts.title.size, false, class.text.fade(TITLE_FADE));
        let has_text = inner.w >= MIN_TEXT_WIDTH && inner.h >= app.line_height();
        let (first, second) = lines(win);
        // Over a thumbnail, the text takes one line, to hide as little of it as can be.
        let one_line = app.line_height().max(title.line_height());

        if behind == Behind::Thumbnail {
            corners(pix, r, THUMB_RADIUS, colors.workspace.fill, t);
            if selected {
                fill(pix, r, (THUMB_RADIUS, THUMB_RADIUS), class.background.fade(SELECTED_TINT), t);
            }
            if has_text {
                let bar = Rect::new(r.x, r.y, r.w, (one_line + 2.0 * PAD).min(r.h));
                // Square bottom corners, unless the bar covers the whole window.
                let bottom = if bar.h < r.h { 0.0 } else { THUMB_RADIUS };
                fill(pix, bar, (THUMB_RADIUS, bottom), class.background, t);
            }
        } else {
            fill(pix, r, rounded, class.background, t);
        }
        stroke(pix, r, rounded, border, width, t);

        if !has_text {
            return;
        }
        if behind == Behind::Thumbnail {
            let rest = second.map(|s| format!(": {s}"));
            let spans: Vec<_> =
                [(first, app)].into_iter().chain(rest.as_deref().map(|s| (s, title))).collect();
            self.text.draw(pix, &spans, Rect::new(inner.x, inner.y, inner.w, one_line), scale);
            return;
        }
        self.text.draw(pix, &[(first, app)], Rect::new(inner.x, inner.y, inner.w, app.line_height()), scale);
        if let Some(second) = second.filter(|_| inner.h >= app.line_height() + title.line_height()) {
            let line = Rect::new(inner.x, inner.y + app.line_height(), inner.w, title.line_height());
            self.text.draw(pix, &[(&second, title)], line, scale);
        }
    }
}

impl Text {
    /// One line of text, each part in its style, vertically centered in `r`,
    /// ellipsized to its width.
    fn draw(&mut self, pix: &mut Pixmap, parts: &[(&str, TextStyle<'_>)], r: Rect, scale: f32) {
        let Some(&(_, style)) = parts.first() else { return };
        let line_h = parts.iter().map(|(_, s)| s.line_height()).fold(0.0, f32::max);
        let mut buf = Buffer::new(&mut self.fonts, Metrics::new(style.size * scale, line_h * scale));
        buf.set_wrap(Wrap::None);
        buf.set_ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)));
        buf.set_size(Some(r.w * scale), Some(line_h * scale));
        let spans = parts.iter().map(|(text, s)| (*text, s.attrs(line_h, scale)));
        buf.set_rich_text(spans, &style.attrs(line_h, scale), Shaping::Advanced, None);

        let origin = ((r.x * scale).round() as i32, ((r.y + (r.h - line_h) / 2.0) * scale).round() as i32);
        let (pw, ph) = (pix.width() as i32, pix.height() as i32);
        let x_range = ((r.x * scale) as i32).max(0)..((r.x + r.w) * scale).min(pw as f32) as i32;
        let y_range = ((r.y * scale) as i32).max(0)..((r.y + r.h) * scale).min(ph as f32) as i32;
        let pixels = pix.pixels_mut();
        buf.draw(&mut self.fonts, &mut self.cache, style.color.into(), |x, y, w, h, c| {
            for py in origin.1 + y..origin.1 + y + h as i32 {
                for px in origin.0 + x..origin.0 + x + w as i32 {
                    if x_range.contains(&px) && y_range.contains(&py) {
                        blend(&mut pixels[(py * pw + px) as usize], c);
                    }
                }
            }
        });
    }
}

/// Generic names only reach here when fontconfig was not available to resolve them.
fn family(name: &str) -> Family<'_> {
    match name {
        "sans-serif" => Family::SansSerif,
        "serif" => Family::Serif,
        "monospace" => Family::Monospace,
        _ => Family::Name(name),
    }
}

/// The two text lines of a window: the app name, then the title and state tags.
fn lines(win: &WinItem) -> (&str, Option<String>) {
    let title = (!win.title.is_empty()).then_some(win.title.as_str());
    let rest: Vec<&str> = title.into_iter().chain(win.tags.iter().copied()).collect();
    (&win.app, (!rest.is_empty()).then(|| rest.join(" · ")))
}

/// Source-over blend of a straight-alpha text color onto a premultiplied pixel.
fn blend(dst: &mut PremultipliedColorU8, c: cosmic_text::Color) {
    let a = u32::from(c.a());
    if a == 0 {
        return;
    }
    let inv = 255 - a;
    let mix = |s: u8, d: u8| ((u32::from(s) * a + u32::from(d) * inv) / 255) as u8;
    let out_a = (a + u32::from(dst.alpha()) * inv / 255) as u8;
    if let Some(p) = PremultipliedColorU8::from_rgba(
        mix(c.r(), dst.red()),
        mix(c.g(), dst.green()),
        mix(c.b(), dst.blue()),
        out_a,
    ) {
        *dst = p;
    }
}

fn paint(color: Rgba) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(color.into());
    p.anti_alias = true;
    p
}

/// A rectangle with its top and bottom corners rounded by the given radii.
fn rounded(r: Rect, (top, bottom): (f32, f32)) -> Option<tiny_skia::Path> {
    let max = r.w.min(r.h) / 2.0;
    let (top, bottom) = (top.min(max), bottom.min(max));
    let (x0, y0, x1, y1) = (r.x, r.y, r.x + r.w, r.y + r.h);
    let mut pb = PathBuilder::new();
    pb.move_to(x0 + top, y0);
    pb.line_to(x1 - top, y0);
    pb.quad_to(x1, y0, x1, y0 + top);
    pb.line_to(x1, y1 - bottom);
    pb.quad_to(x1, y1, x1 - bottom, y1);
    pb.line_to(x0 + bottom, y1);
    pb.quad_to(x0, y1, x0, y1 - bottom);
    pb.line_to(x0, y0 + top);
    pb.quad_to(x0, y0, x0 + top, y0);
    pb.close();
    pb.finish()
}

fn fill(pix: &mut Pixmap, r: Rect, radii: (f32, f32), color: Rgba, t: Transform) {
    if let Some(path) = rounded(r, radii) {
        pix.fill_path(&path, &paint(color), FillRule::Winding, t, None);
    }
}

/// Fills what lies in `r` but outside its corners rounded by `radius`.
fn corners(pix: &mut Pixmap, r: Rect, radius: f32, color: Rgba, t: Transform) {
    let (Some(rect), Some(rounded)) =
        (tiny_skia::Rect::from_xywh(r.x, r.y, r.w, r.h), rounded(r, (radius, radius)))
    else {
        return;
    };
    let mut pb = PathBuilder::new();
    pb.push_rect(rect);
    pb.push_path(&rounded);
    if let Some(path) = pb.finish() {
        pix.fill_path(&path, &paint(color), FillRule::EvenOdd, t, None);
    }
}

fn stroke(pix: &mut Pixmap, r: Rect, radii: (f32, f32), color: Rgba, width: f32, t: Transform) {
    if let Some(path) = rounded(r.inset(width / 2.0), radii) {
        let s = Stroke { width, ..Stroke::default() };
        pix.stroke_path(&path, &paint(color), &s, t, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ConId, Rect};

    fn win(title: &str, app: &str, tags: Vec<&'static str>) -> WinItem {
        let (title, app) = (title.into(), app.into());
        WinItem {
            id: ConId(1),
            app,
            title,
            rect: Rect::default(),
            workspace: 0,
            focused: false,
            urgent: false,
            tags,
            toplevel: None,
        }
    }

    #[test]
    fn text_lines() {
        assert_eq!(lines(&win("btop", "Alacritty", vec![])), ("Alacritty", Some("btop".into())));
        assert_eq!(
            lines(&win("btop", "Alacritty", vec!["float", "sticky"])),
            ("Alacritty", Some("btop · float · sticky".into()))
        );
        assert_eq!(lines(&win("", "Alacritty", vec![])), ("Alacritty", None));
        assert_eq!(lines(&win("", "Alacritty", vec!["float"])), ("Alacritty", Some("float".into())));
    }

    fn rgba(pix: &Pixmap, (x, y): (u32, u32)) -> [u8; 4] {
        let p = pix.pixel(x, y).unwrap();
        [p.red(), p.green(), p.blue(), p.alpha()]
    }

    fn tile(selected: bool, behind: Behind) -> Pixmap {
        let mut r = Renderer::new(Config::load(None));
        r.draw_tile(&win("btop", "Alacritty", vec![]), selected, behind, (200, 120), 1.0).unwrap()
    }

    #[test]
    fn tile_over_a_thumbnail_leaves_it_visible() {
        let below_text = (100, 100);
        assert_eq!(rgba(&tile(false, Behind::Nothing), below_text)[3], 255);
        assert_eq!(rgba(&tile(false, Behind::Thumbnail), below_text)[3], 0);
        // The selected one is tinted, half see-through.
        let tinted = rgba(&tile(true, Behind::Thumbnail), below_text)[3];
        assert!((0x70..=0x90).contains(&tinted), "tint alpha {tinted}");
    }

    #[test]
    fn text_sits_on_a_bar_in_the_window_color() {
        let colors = Config::load(None).colors.window;
        let beside_text = (190, 6);
        let bytes = |c: Rgba| c.0.to_be_bytes();
        assert_eq!(rgba(&tile(false, Behind::Thumbnail), beside_text), bytes(colors.normal.background));
        assert_eq!(rgba(&tile(true, Behind::Thumbnail), beside_text), bytes(colors.selected.background));
        // One line: the bar ends above where a title line would start.
        assert_eq!(rgba(&tile(false, Behind::Thumbnail), (100, 34))[3], 0);
    }

    #[test]
    fn tile_corners_take_the_workspace_color() {
        let fill = Config::load(None).colors.workspace.fill.0.to_be_bytes();
        for corner in [(0, 0), (199, 0), (0, 119), (199, 119)] {
            assert_eq!(rgba(&tile(false, Behind::Thumbnail), corner), fill, "{corner:?}");
        }
        // Without a thumbnail, the rounded box shows the surface below.
        assert_eq!(rgba(&tile(false, Behind::Nothing), (0, 0))[3], 0);
    }

    #[test]
    fn zero_size_is_none_not_a_panic() {
        let mut r = Renderer::new(Config::load(None));
        let view = View { selected: None, selected_workspace: None };
        assert!(r.draw(&Scene::default(), &view, 0, 10, 1.0).is_none());
        assert!(r.draw(&Scene::default(), &view, 10, 10, 1.0).is_some());
    }
}
