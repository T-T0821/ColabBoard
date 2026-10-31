//! ブラウザ用クライアント。canvas に wgpu で描画し、パン/ズームを受け付ける。

#[cfg(target_arch = "wasm32")]
mod app;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
    let _ = console_log::init_with_level(log::Level::Info);
    wasm_bindgen_futures::spawn_local(async {
        if let Err(e) = app::run().await {
            log::error!("{e}");
            app::show_error(&e);
        }
    });
}

/// `"#rrggbb"` を色にする。
pub fn parse_hex(s: &str) -> Option<board_core::Rgba8> {
    let s = s.strip_prefix('#').unwrap_or(s);
    if s.len() != 6 {
        return None;
    }
    u32::from_str_radix(s, 16).ok().map(board_core::Rgba8::hex)
}

/// 表示用: ズーム率を `"125%"` のように整形する。
pub fn format_zoom(zoom: f32) -> String {
    format!("{:.0}%", zoom * 100.0)
}

/// 表示用: 要素数を `"100,000"` のように整形する。
pub fn format_count(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(format_zoom(1.25), "125%");
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(100_000), "100,000");
        assert_eq!(format_count(1_234_567), "1,234,567");
        assert_eq!(parse_hex("#fff3a3"), Some(board_core::Rgba8::hex(0xfff3a3)));
        assert_eq!(parse_hex("zz"), None);
    }
}
