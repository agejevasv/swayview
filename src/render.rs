//! Draws a `Scene` into a pixmap with tiny-skia and cosmic-text.

use cosmic_text::{
    Attrs, Buffer, Ellipsize, EllipsizeHeightLimit, Family, FontSystem, Metrics, Shaping, SwashCache, Weight,
    Wrap,
};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, PremultipliedColorU8, Stroke, Transform};

use crate::color::Rgba;
use crate::fonts;
use crate::layout::{HEADER, Scene, WinItem};
use crate::model::Rect;
use crate::theme::Theme;

/// Alpha of the title line relative to the app line.
const TITLE_FADE: u8 = 0xb0;

const PAD: f32 = 6.0;
const LINE_HEIGHT: f32 = 1.3;
/// Narrower windows get no text.
const MIN_TEXT_WIDTH: f32 = 12.0;

#[derive(Clone, Copy, Debug)]
struct TextStyle {
    size: f32,
    bold: bool,
    color: Rgba,
}

impl TextStyle {
    /// White until given a color with `color`.
    const fn new(size: f32, bold: bool) -> Self {
        TextStyle { size, bold, color: Rgba(0xffffffff) }
    }

    fn color(self, color: Rgba) -> Self {
        TextStyle { color, ..self }
    }

    fn line_height(self) -> f32 {
        self.size * LINE_HEIGHT
    }
}

const APP_LINE: TextStyle = TextStyle::new(14.0, true);
const TITLE_LINE: TextStyle = TextStyle::new(12.0, false);
const WS_LABEL: TextStyle = TextStyle::new(16.0, true);
const OUTPUT_LABEL: TextStyle = TextStyle::new(13.0, false);

#[derive(Debug)]
pub struct View {
    /// The selected window, if it is on this scene.
    pub selected: Option<usize>,
    /// The workspace marked as holding the selection.
    pub selected_workspace: Option<usize>,
}

pub struct Renderer {
    text: Text,
    theme: Theme,
}

struct Text {
    fonts: FontSystem,
    cache: SwashCache,
}

impl Renderer {
    pub fn new(theme: Theme) -> Self {
        Renderer { text: Text { fonts: fonts::font_system(), cache: SwashCache::new() }, theme }
    }

    /// Renders at `scale` physical pixels per logical pixel; `None` if `w` or `h` is 0.
    pub fn draw(&mut self, scene: &Scene, view: &View, w: u32, h: u32, scale: f32) -> Option<Pixmap> {
        let mut pix = Pixmap::new(w, h)?;
        let theme = &self.theme;
        pix.fill(theme.backdrop.into());
        let t = Transform::from_scale(scale, scale);
        let output_name = OUTPUT_LABEL.color(theme.output_name);
        self.text.draw(&mut pix, &scene.output_name, scene.output_label, output_name, scale);

        let c = &theme.workspace;
        for (i, ws) in scene.workspaces.iter().enumerate() {
            // Only the number marks the selected or urgent workspace.
            let label = if view.selected_workspace == Some(i) {
                c.selected
            } else if ws.urgent {
                c.urgent
            } else {
                c.label
            };
            fill(&mut pix, ws.rect, 6.0, c.fill, t);
            let r = Rect::new(ws.header.x + 2.0, ws.header.y, ws.header.w - 4.0, HEADER);
            self.text.draw(&mut pix, &ws.name, r, WS_LABEL.color(label), scale);
        }

        for (i, win) in scene.windows.iter().enumerate() {
            let selected = view.selected == Some(i);
            // The selection starts on sway's focused window.
            let class = if selected {
                theme.window.selected
            } else if win.urgent {
                theme.window.urgent
            } else {
                theme.window.normal
            };
            let (border, width) = (class.border, if selected { 2.0 } else { 1.0 });
            fill(&mut pix, win.rect, 4.0, class.background, t);
            stroke(&mut pix, win.rect, 4.0, border, width, t);

            let inner = win.rect.inset(PAD);
            let (app, title) = (APP_LINE.color(class.text), TITLE_LINE.color(class.text.fade(TITLE_FADE)));
            if inner.w < MIN_TEXT_WIDTH || inner.h < app.line_height() {
                continue;
            }
            let (first, second) = lines(win);
            let r = Rect::new(inner.x, inner.y, inner.w, app.line_height());
            self.text.draw(&mut pix, first, r, app, scale);
            if let Some(second) = second
                && inner.h >= app.line_height() + title.line_height()
            {
                let r = Rect::new(inner.x, inner.y + app.line_height(), inner.w, title.line_height());
                self.text.draw(&mut pix, &second, r, title, scale);
            }
        }
        Some(pix)
    }
}

impl Text {
    /// Single line of text, vertically centered in `r`, ellipsized to its width.
    fn draw(&mut self, pix: &mut Pixmap, s: &str, r: Rect, style: TextStyle, scale: f32) {
        let line_h = style.line_height();
        let mut buf = Buffer::new(&mut self.fonts, Metrics::new(style.size * scale, line_h * scale));
        buf.set_wrap(Wrap::None);
        buf.set_ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1)));
        buf.set_size(Some(r.w * scale), Some(line_h * scale));
        let weight = if style.bold { Weight::BOLD } else { Weight::NORMAL };
        buf.set_text(s, &Attrs::new().family(Family::SansSerif).weight(weight), Shaping::Advanced, None);

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

fn rounded(r: Rect, radius: f32) -> Option<tiny_skia::Path> {
    let rad = radius.min(r.w / 2.0).min(r.h / 2.0);
    let (x0, y0, x1, y1) = (r.x, r.y, r.x + r.w, r.y + r.h);
    let mut pb = PathBuilder::new();
    pb.move_to(x0 + rad, y0);
    pb.line_to(x1 - rad, y0);
    pb.quad_to(x1, y0, x1, y0 + rad);
    pb.line_to(x1, y1 - rad);
    pb.quad_to(x1, y1, x1 - rad, y1);
    pb.line_to(x0 + rad, y1);
    pb.quad_to(x0, y1, x0, y1 - rad);
    pb.line_to(x0, y0 + rad);
    pb.quad_to(x0, y0, x0 + rad, y0);
    pb.close();
    pb.finish()
}

fn fill(pix: &mut Pixmap, r: Rect, radius: f32, color: Rgba, t: Transform) {
    if let Some(path) = rounded(r, radius) {
        pix.fill_path(&path, &paint(color), FillRule::Winding, t, None);
    }
}

fn stroke(pix: &mut Pixmap, r: Rect, radius: f32, color: Rgba, width: f32, t: Transform) {
    if let Some(path) = rounded(r.inset(width / 2.0), radius) {
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

    #[test]
    fn zero_size_is_none_not_a_panic() {
        let mut r = Renderer::new(Theme::load(None));
        let view = View { selected: None, selected_workspace: None };
        assert!(r.draw(&Scene::default(), &view, 0, 10, 1.0).is_none());
        assert!(r.draw(&Scene::default(), &view, 10, 10, 1.0).is_some());
    }
}
