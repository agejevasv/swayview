//! Draws a `Scene` into a pixmap with tiny-skia and cosmic-text.

use cosmic_text::{
    Attrs, Buffer, Ellipsize, EllipsizeHeightLimit, Family, FontSystem, Metrics, Shaping, SwashCache, Weight,
    Wrap,
};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, PremultipliedColorU8, Stroke, Transform};

use crate::color::Rgba;
use crate::fonts;
use crate::layout::{HEADER, Hit, Scene, WinItem};
use crate::model::Rect;
use crate::theme::Theme;

// Around the windows; window colors come from the sway theme.
const BACKDROP: Rgba = Rgba(0x101216e0);
const WS_FILL: Rgba = Rgba(0x16181dff);
const WS_BORDER: Rgba = Rgba(0x3a3f4bff);
const WS_VISIBLE: Rgba = Rgba(0x6b7385ff);
const LABEL: Rgba = Rgba(0xdde1e8ff);
const OUTPUT_NAME: Rgba = Rgba(0x8a93a5ff);
/// Alpha of the app line relative to the title line.
const APP_FADE: u8 = 0xb0;
/// Accent colors apps are hashed into, about 30° apart in hue: red, orange,
/// yellow, lime, green, teal, cyan, blue, indigo, purple, magenta, pink.
const ACCENTS: [Rgba; 12] = [
    Rgba(0xe06c75ff),
    Rgba(0xe8915aff),
    Rgba(0xe5c07bff),
    Rgba(0xb5d468ff),
    Rgba(0x98c379ff),
    Rgba(0x5fc9a4ff),
    Rgba(0x56b6c2ff),
    Rgba(0x61afefff),
    Rgba(0x8a8cf0ff),
    Rgba(0xc678ddff),
    Rgba(0xe87fd0ff),
    Rgba(0xf78fb3ff),
];

const PAD: f32 = 6.0;
/// Width of the app color stripe along a window's left edge.
const STRIPE: f32 = 3.0;
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
    const fn new(size: f32, bold: bool) -> Self {
        TextStyle { size, bold, color: LABEL }
    }

    fn color(self, color: Rgba) -> Self {
        TextStyle { color, ..self }
    }

    fn line_height(self) -> f32 {
        self.size * LINE_HEIGHT
    }
}

const TITLE: TextStyle = TextStyle::new(14.0, true);
const APP: TextStyle = TextStyle::new(12.0, false);
const WS_LABEL: TextStyle = TextStyle::new(16.0, true);
const OUTPUT_LABEL: TextStyle = TextStyle::new(13.0, false);

#[derive(Debug)]
pub struct View {
    pub selected: Option<usize>,
    pub hover: Option<Hit>,
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
        pix.fill(BACKDROP.into());
        let t = Transform::from_scale(scale, scale);
        let theme = &self.theme;
        self.text.draw(
            &mut pix,
            &scene.output_name,
            scene.output_label,
            OUTPUT_LABEL.color(OUTPUT_NAME),
            scale,
        );

        for (i, ws) in scene.workspaces.iter().enumerate() {
            let hovered = view.hover == Some(Hit::Workspace(i));
            let border = if hovered {
                theme.focused.indicator
            } else if ws.focused {
                theme.focused.border
            } else if ws.urgent {
                theme.urgent.background
            } else if ws.visible {
                WS_VISIBLE
            } else {
                WS_BORDER
            };
            let width = if ws.focused || ws.urgent { 2.0 } else { 1.0 };
            fill(&mut pix, ws.rect, 6.0, WS_FILL, t);
            stroke(&mut pix, ws.rect, 6.0, border, width, t);
            let label = if hovered || ws.focused || ws.urgent { border } else { LABEL };
            let r = Rect::new(ws.header.x + 2.0, ws.header.y, ws.header.w - 4.0, HEADER);
            self.text.draw(&mut pix, &ws.name, r, WS_LABEL.color(label), scale);
        }

        for (i, win) in scene.windows.iter().enumerate() {
            let selected = view.selected == Some(i);
            let class = if win.focused {
                theme.focused
            } else if win.urgent {
                theme.urgent
            } else if selected {
                theme.focused_inactive
            } else {
                theme.unfocused
            };
            let (border, width) = if selected {
                (accent(&win.app), 3.0)
            } else {
                (class.border, if win.focused { 2.0 } else { 1.0 })
            };
            fill(&mut pix, win.rect, 4.0, class.background, t);
            // Inside the border, so a thick selection outline does not hide it.
            let stripe = Rect::new(win.rect.x + width, win.rect.y + width, STRIPE, win.rect.h - 2.0 * width);
            fill(&mut pix, stripe, 1.5, accent(&win.app), t);
            stroke(&mut pix, win.rect, 4.0, border, width, t);

            let inner = win.rect.inset(PAD);
            let inner = Rect::new(inner.x + STRIPE, inner.y, inner.w - STRIPE, inner.h);
            if inner.w < MIN_TEXT_WIDTH {
                continue;
            }
            let (first, second) = lines(win);
            let (title, app) = (TITLE.color(class.text), APP.color(class.text.fade(APP_FADE)));
            if inner.h >= title.line_height() {
                let r = Rect::new(inner.x, inner.y, inner.w, title.line_height());
                self.text.draw(&mut pix, first, r, title, scale);
            }
            if let Some(second) = second
                && inner.h >= title.line_height() + app.line_height()
            {
                let r = Rect::new(inner.x, inner.y + title.line_height(), inner.w, app.line_height());
                self.text.draw(&mut pix, &second, r, app, scale);
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

/// The two text lines of a window: the title, which tells windows of one app
/// apart, then the app name and state tags. Without a title the app goes first.
fn lines(win: &WinItem) -> (&str, Option<String>) {
    let (first, rest) = if win.title.is_empty() {
        (win.app.as_str(), win.tags.clone())
    } else {
        (win.title.as_str(), std::iter::once(win.app.as_str()).chain(win.tags.iter().copied()).collect())
    };
    (first, (!rest.is_empty()).then(|| rest.join(" · ")))
}

/// A stable color per app, ignoring case (`Slack` and `slack` match).
fn accent(app: &str) -> Rgba {
    // FNV-1a: stable across runs and builds, unlike `DefaultHasher`.
    let hash = app
        .bytes()
        .fold(0x811c_9dc5_u32, |h, b| (h ^ u32::from(b.to_ascii_lowercase())).wrapping_mul(0x0100_0193));
    ACCENTS[hash as usize % ACCENTS.len()]
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
        WinItem { id: ConId(1), app, title, rect: Rect::default(), focused: false, urgent: false, tags }
    }

    #[test]
    fn text_lines() {
        assert_eq!(lines(&win("htop", "foot", vec![])), ("htop", Some("foot".into())));
        assert_eq!(
            lines(&win("htop", "foot", vec!["float", "sticky"])),
            ("htop", Some("foot · float · sticky".into()))
        );
        assert_eq!(lines(&win("", "foot", vec![])), ("foot", None));
        assert_eq!(lines(&win("", "foot", vec!["float"])), ("foot", Some("float".into())));
    }

    #[test]
    fn accents_are_stable_and_ignore_case() {
        assert_eq!(accent("Alacritty"), accent("alacritty"));
        assert_eq!(accent("firefox"), accent("firefox"));
        // Apps that shared a color with the smaller palette.
        assert_ne!(accent("Alacritty"), accent("brave-browser"));
        // Not all apps share one color.
        let apps = ["foot", "firefox", "code", "Slack", "discord", "Alacritty", "pavucontrol"];
        let distinct: std::collections::HashSet<_> = apps.iter().map(|a| accent(a).0).collect();
        assert!(distinct.len() > 2);
    }

    #[test]
    fn zero_size_is_none_not_a_panic() {
        let mut r = Renderer::new(Theme::default());
        let view = View { selected: None, hover: None };
        assert!(r.draw(&Scene::default(), &view, 0, 10, 1.0).is_none());
        assert!(r.draw(&Scene::default(), &view, 10, 10, 1.0).is_some());
    }
}
