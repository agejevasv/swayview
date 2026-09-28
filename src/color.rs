//! RGBA colors, stored as 0xRRGGBBAA.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgba(pub u32);

impl Rgba {
    /// Parses `#rrggbb` or `#rrggbbaa`, the forms sway accepts.
    pub fn parse(s: &str) -> Option<Rgba> {
        let hex = s.strip_prefix('#').unwrap_or(s);
        if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let v = u32::from_str_radix(hex, 16).ok()?;
        match hex.len() {
            6 => Some(Rgba((v << 8) | 0xff)),
            8 => Some(Rgba(v)),
            _ => None,
        }
    }

    pub fn to_bytes(self) -> [u8; 4] {
        self.0.to_be_bytes()
    }

    /// Scales alpha by `f / 255`.
    pub fn fade(self, f: u8) -> Rgba {
        let [r, g, b, a] = self.to_bytes();
        let a = (u16::from(a) * u16::from(f) / 255) as u8;
        Rgba(u32::from_be_bytes([r, g, b, a]))
    }
}

impl From<Rgba> for tiny_skia::Color {
    fn from(c: Rgba) -> Self {
        let [r, g, b, a] = c.to_bytes();
        tiny_skia::Color::from_rgba8(r, g, b, a)
    }
}

impl From<Rgba> for cosmic_text::Color {
    fn from(c: Rgba) -> Self {
        let [r, g, b, a] = c.to_bytes();
        cosmic_text::Color::rgba(r, g, b, a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse() {
        assert_eq!(Rgba::parse("#4c7899"), Some(Rgba(0x4c7899ff)));
        assert_eq!(Rgba::parse("#4c789980"), Some(Rgba(0x4c789980)));
        assert_eq!(Rgba::parse("4c7899"), Some(Rgba(0x4c7899ff)));
        assert_eq!(Rgba::parse("#fff"), None);
        assert_eq!(Rgba::parse("#+fffff"), None);
        assert_eq!(Rgba::parse("$var"), None);
    }

    #[test]
    fn fade() {
        assert_eq!(Rgba(0x112233ff).fade(0x80), Rgba(0x11223380));
        assert_eq!(Rgba(0x11223380).fade(0xff), Rgba(0x11223380));
    }
}
