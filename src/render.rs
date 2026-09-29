//! Draws a `Scene` into a pixmap with tiny-skia and cosmic-text.

use std::collections::HashMap;

use cosmic_text::{
    Attrs, Buffer, Ellipsize, EllipsizeHeightLimit, Family, FontSystem, Metrics, Shaping, SwashCache, Weight,
    Wrap,
};
use tiny_skia::{
    FillRule, FilterQuality, Paint, PathBuilder, Pattern, Pixmap, PixmapPaint, PremultipliedColorU8,
    SpreadMode, Stroke, Transform,
};

use crate::color::Rgba;
use crate::config::{Colors, Config, Font, Fonts};
use crate::fonts;
use crate::layout::{HEADER, Scene, WinItem};
use crate::model::Rect;

/// Alpha of the title line relative to the app line.
const TITLE_FADE: u8 = 0xb0;
/// Darkens a thumbnail under the text lines.
const SHADE: Rgba = Rgba(0x000000a0);
/// Alpha of the selected window's color over its thumbnail.
const SELECTED_TINT: u8 = 0x80;

const PAD: f32 = 6.0;
const WINDOW_RADIUS: f32 = 4.0;
/// The border's outer edge is rounder than the window by half its width, up
/// to 1; drawn with this, a thumbnail does not show past the border's corners.
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

impl TextStyle<'_> {
    fn line_height(&self) -> f32 {
        self.size * LINE_HEIGHT
    }
}

impl Font {
    fn style(&self, size: f32, bold: bool, color: Rgba) -> TextStyle<'_> {
        TextStyle { family: &self.family, size, bold, color }
    }
}

#[derive(Debug)]
pub struct Thumb {
    source: Pixmap,
    /// `source` made to fit its window's box, see `fit`.
    fitted: Option<Pixmap>,
}

impl Thumb {
    pub fn new(source: Pixmap) -> Self {
        Thumb { source, fitted: None }
    }

    /// Scales the thumbnail for a window drawn in `r` at `scale`, unless it
    /// already is. Scaling is slow, so `Renderer::draw` only copies the result.
    pub fn fit(&mut self, r: Rect, scale: f32) {
        let (_, _, w, h) = snap(r, scale);
        if self.fitted_to(w, h).is_some() {
            return;
        }
        self.fitted = Pixmap::new(w, h).map(|mut pix| {
            fill_cover(&mut pix, &self.source, THUMB_RADIUS * scale);
            pix
        });
    }

    fn fitted_to(&self, w: u32, h: u32) -> Option<&Pixmap> {
        self.fitted.as_ref().filter(|f| (f.width(), f.height()) == (w, h))
    }
}

/// Window contents by `WinItem::toplevel`.
pub type Thumbs = HashMap<String, Thumb>;

#[derive(Debug)]
pub struct View<'a> {
    /// The selected window, if it is on this scene.
    pub selected: Option<usize>,
    pub selected_workspace: Option<usize>,
    pub thumbs: &'a Thumbs,
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
    pub fn draw(&mut self, scene: &Scene, view: &View<'_>, w: u32, h: u32, scale: f32) -> Option<Pixmap> {
        let mut pix = Pixmap::new(w, h)?;
        let (fonts, colors) = (&self.fonts, &self.colors);
        pix.fill(colors.backdrop.into());
        let t = Transform::from_scale(scale, scale);
        let output_name = fonts.title.style(OUTPUT_LABEL_SIZE, false, colors.output_name);
        self.text.draw(&mut pix, &scene.output_name, scene.output_label, output_name, scale);

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
            self.text.draw(&mut pix, &ws.name, r, fonts.app.style(WS_LABEL_SIZE, true, label), scale);
        }

        for (i, win) in scene.windows.iter().enumerate() {
            let selected = view.selected == Some(i);
            let class = if selected {
                colors.window.selected
            } else if win.urgent {
                colors.window.urgent
            } else {
                colors.window.normal
            };
            let (border, width) = (class.border, if selected { 2.0 } else { 1.0 });
            let rounded = (WINDOW_RADIUS, WINDOW_RADIUS);
            fill(&mut pix, win.rect, rounded, class.background, t);

            let inner = win.rect.inset(PAD);
            let app = fonts.app.style(fonts.app.size, true, class.text);
            let title = fonts.title.style(fonts.title.size, false, class.text.fade(TITLE_FADE));
            let has_text = inner.w >= MIN_TEXT_WIDTH && inner.h >= app.line_height();
            let (first, second) = lines(win);
            let second = second.filter(|_| inner.h >= app.line_height() + title.line_height());
            let text_h = app.line_height() + second.as_ref().map_or(0.0, |_| title.line_height());

            let thumb = win.toplevel.as_ref().and_then(|id| view.thumbs.get(id));
            let (x, y, w, h) = snap(win.rect, scale);
            // Left out if not fitted to this size; `Thumb::fit` is the caller's job.
            if let Some(fitted) = thumb.and_then(|t| t.fitted_to(w, h)) {
                pix.draw_pixmap(x, y, fitted.as_ref(), &PixmapPaint::default(), Transform::identity(), None);
                if selected {
                    let tint = class.background.fade(SELECTED_TINT);
                    fill(&mut pix, win.rect, (THUMB_RADIUS, THUMB_RADIUS), tint, t);
                }
                if has_text {
                    let r = win.rect;
                    let shade = Rect::new(r.x, r.y, r.w, (text_h + 2.0 * PAD).min(r.h));
                    // Square bottom corners, unless the shade covers the whole window.
                    let bottom = if shade.h < r.h { 0.0 } else { THUMB_RADIUS };
                    fill(&mut pix, shade, (THUMB_RADIUS, bottom), SHADE, t);
                }
            }
            stroke(&mut pix, win.rect, rounded, border, width, t);

            if !has_text {
                continue;
            }
            let r = Rect::new(inner.x, inner.y, inner.w, app.line_height());
            self.text.draw(&mut pix, first, r, app, scale);
            if let Some(second) = second {
                let r = Rect::new(inner.x, inner.y + app.line_height(), inner.w, title.line_height());
                self.text.draw(&mut pix, &second, r, title, scale);
            }
        }
        Some(pix)
    }
}

impl Text {
    /// Single line of text, vertically centered in `r`, ellipsized to its width.
    fn draw(&mut self, pix: &mut Pixmap, s: &str, r: Rect, style: TextStyle<'_>, scale: f32) {
        let line_h = style.line_height();
        let mut buf = Buffer::new(&mut self.fonts, Metrics::new(style.size * scale, line_h * scale));
        buf.set_wrap(Wrap::None);
        buf.set_ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)));
        buf.set_size(Some(r.w * scale), Some(line_h * scale));
        let weight = if style.bold { Weight::BOLD } else { Weight::NORMAL };
        let attrs = Attrs::new().family(family(style.family)).weight(weight);
        buf.set_text(s, &attrs, Shaping::Advanced, None);

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

/// `r` at `scale`, in whole pixels: x, y, width, height.
fn snap(r: Rect, scale: f32) -> (i32, i32, u32, u32) {
    let (x0, y0) = ((r.x * scale).round(), (r.y * scale).round());
    let (x1, y1) = (((r.x + r.w) * scale).round(), ((r.y + r.h) * scale).round());
    (x0 as i32, y0 as i32, (x1 - x0).max(0.0) as u32, (y1 - y0).max(0.0) as u32)
}

/// Fills `pix` with `source`, scaled to cover it and centered; the overflow is cut off.
fn fill_cover(pix: &mut Pixmap, source: &Pixmap, radius: f32) {
    let r = Rect::new(0.0, 0.0, pix.width() as f32, pix.height() as f32);
    let (sw, sh) = (source.width() as f32, source.height() as f32);
    let s = (r.w / sw).max(r.h / sh);
    let placed = Transform::from_row(s, 0.0, 0.0, s, r.x + (r.w - sw * s) / 2.0, r.y + (r.h - sh * s) / 2.0);
    let paint = Paint {
        shader: Pattern::new(source.as_ref(), SpreadMode::Pad, FilterQuality::Bilinear, 1.0, placed),
        anti_alias: true,
        ..Paint::default()
    };
    if let Some(path) = rounded(r, (radius, radius)) {
        pix.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
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

    const RED: [u8; 4] = [255, 0, 0, 255];

    /// The fixture's first window with a solid red thumbnail, fitted at `fit_scale`.
    fn scene_with_thumb(fit_scale: Option<f32>) -> (Scene, Thumbs) {
        let mut scene = crate::layout::build(&crate::model::tests::tree().outputs[0], 800.0, 450.0);
        scene.windows[0].toplevel = Some("a".into());
        let mut source = Pixmap::new(40, 30).unwrap();
        source.fill(tiny_skia::Color::from_rgba8(255, 0, 0, 255));
        let mut thumb = Thumb::new(source);
        if let Some(scale) = fit_scale {
            thumb.fit(scene.windows[0].rect, scale);
        }
        (scene, Thumbs::from([("a".into(), thumb)]))
    }

    fn render(scene: &Scene, thumbs: &Thumbs, selected: Option<usize>, scale: f32) -> Pixmap {
        let view = View { selected, selected_workspace: None, thumbs };
        let (w, h) = ((scene.size.0 * scale) as u32, (scene.size.1 * scale) as u32);
        Renderer::new(Config::load(None)).draw(scene, &view, w, h, scale).unwrap()
    }

    /// A point in the lower middle of window 0, below the text, at `scale`.
    fn below_text(scene: &Scene, scale: f32) -> (u32, u32) {
        let r = scene.windows[0].rect;
        (((r.x + r.w / 2.0) * scale) as u32, ((r.y + r.h * 0.8) * scale) as u32)
    }

    fn rgba(pix: &Pixmap, (x, y): (u32, u32)) -> [u8; 4] {
        let p = pix.pixel(x, y).unwrap();
        [p.red(), p.green(), p.blue(), p.alpha()]
    }

    #[test]
    fn thumbnail_drawn_only_when_fitted() {
        let (scene, _) = scene_with_thumb(None);
        let plain = render(&scene, &Thumbs::new(), None, 1.0);
        let at = below_text(&scene, 1.0);
        assert_ne!(rgba(&plain, at), RED);

        let (scene, thumbs) = scene_with_thumb(Some(1.0));
        assert_eq!(rgba(&render(&scene, &thumbs, None, 1.0), at), RED);
        // Not fitted, or fitted for another scale: a plain box.
        for fit in [None, Some(2.0)] {
            let (scene, thumbs) = scene_with_thumb(fit);
            assert_eq!(render(&scene, &thumbs, None, 1.0).data(), plain.data(), "{fit:?}");
        }
    }

    #[test]
    fn selected_thumbnail_is_tinted() {
        let (scene, thumbs) = scene_with_thumb(Some(1.0));
        let tinted = rgba(&render(&scene, &thumbs, Some(0), 1.0), below_text(&scene, 1.0));
        assert_ne!(tinted, RED);
        assert!(tinted[0] > tinted[1] && tinted[0] > tinted[2], "still mostly red: {tinted:?}");
    }

    #[test]
    fn thumbnail_stays_inside_the_border_corners() {
        for (selected, scale) in [(None, 1.0), (None, 2.0), (Some(0), 1.5), (Some(0), 2.0)] {
            let (scene, thumbs) = scene_with_thumb(Some(scale));
            let with = render(&scene, &thumbs, selected, scale);
            let without = render(&scene, &Thumbs::new(), selected, scale);
            let (x, y, w, h) = snap(scene.windows[0].rect, scale);
            let (x, y) = (x as u32, y as u32);
            for corner in [(x, y), (x + w - 1, y), (x, y + h - 1), (x + w - 1, y + h - 1)] {
                assert_eq!(rgba(&with, corner), rgba(&without, corner), "{corner:?} at {scale}");
            }
        }
    }

    #[test]
    fn snapping_to_pixels() {
        assert_eq!(snap(Rect::new(10.3, 20.7, 101.1, 60.3), 1.0), (10, 21, 101, 60));
        assert_eq!(snap(Rect::new(10.3, 20.7, 101.1, 60.3), 1.5), (15, 31, 152, 91));
        assert_eq!(snap(Rect::new(5.0, 5.0, 0.2, 0.2), 1.0).2, 0);
    }

    #[test]
    fn zero_size_is_none_not_a_panic() {
        let mut r = Renderer::new(Config::load(None));
        let view = View { selected: None, selected_workspace: None, thumbs: &Thumbs::new() };
        assert!(r.draw(&Scene::default(), &view, 0, 10, 1.0).is_none());
        assert!(r.draw(&Scene::default(), &view, 10, 10, 1.0).is_some());
    }
}
