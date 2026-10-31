use bytemuck::{Pod, Zeroable};

/// sRGB の 8bit RGBA。GPU にはそのまま `u32` として渡す。
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Pod, Zeroable)]
pub struct Rgba8(pub [u8; 4]);

impl Rgba8 {
    pub const TRANSPARENT: Self = Self([0, 0, 0, 0]);
    pub const BLACK: Self = Self([0x1f, 0x1f, 0x1f, 0xff]);
    pub const WHITE: Self = Self([0xff, 0xff, 0xff, 0xff]);

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self([r, g, b, 0xff])
    }

    /// `0xRRGGBB` 形式から作る。
    pub const fn hex(rgb: u32) -> Self {
        Self::rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
    }

    /// `"#rrggbbaa"` 形式の文字列。
    pub fn to_hex(self) -> String {
        let [r, g, b, a] = self.0;
        format!("#{r:02x}{g:02x}{b:02x}{a:02x}")
    }

    /// `"#rrggbb"` か `"#rrggbbaa"` を読む。
    pub fn parse_hex(s: &str) -> Option<Self> {
        let s = s.strip_prefix('#').unwrap_or(s);
        if !matches!(s.len(), 6 | 8) || !s.is_ascii() {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&s[i..i + 2], 16).ok();
        let a = if s.len() == 8 { byte(6)? } else { 0xff };
        Some(Self([byte(0)?, byte(2)?, byte(4)?, a]))
    }

    /// GPU の `unpack4x8unorm` で読める並び(リトルエンディアンで R が最下位)。
    pub fn to_u32(self) -> u32 {
        u32::from_le_bytes(self.0)
    }
}

/// 付箋の定番色。
pub const STICKY_COLORS: [Rgba8; 6] = [
    Rgba8::hex(0xfff3a3),
    Rgba8::hex(0xffd2a8),
    Rgba8::hex(0xffb8c9),
    Rgba8::hex(0xd3c2ff),
    Rgba8::hex(0xb5e3ff),
    Rgba8::hex(0xc4f0c2),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_and_pack() {
        let c = Rgba8::hex(0x112233);
        assert_eq!(c.0, [0x11, 0x22, 0x33, 0xff]);
        assert_eq!(c.to_u32(), 0xff33_2211);
    }
}
