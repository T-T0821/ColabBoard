//! 重いファイルを本体のスレッドの外で処理する部品(Web Worker で動く別の wasm)。
//!
//! - **PDF**: hayro(Pure Rust のラスタライザ)で各ページを JPEG にする。数百ページでも、
//!   ページごとの仕事に分けて複数の Worker で並行に描くので、最初のページからすぐ表示できる。
//! - **画像**: 大きな写真のデコードと縮小。
//! - **表**: CSV(Shift_JIS も)と Excel などの表計算ファイルの読み取り。巨大なファイルは
//!   ボードに置ける行数だけを読み、残りは数えるだけにする。
//!
//! hayro と calamine はクライアントの wasm を数 MB 大きくするので、本体とは別の wasm にしてある。
//! 本体は重いファイルが来たときに初めてこの Worker を起こす(初回表示では読み込まない)。
//! 何個の Worker を並べ、どの仕事をどれに渡すかは本体側(`board-web` の `file_worker`)が決める。
//!
//! 本体も画像の縮小([`picture::prepare_image`])だけはこのクレートを使う(`worker` 機能なし)。

pub mod picture;

#[cfg(feature = "worker")]
pub mod pdf;
#[cfg(feature = "worker")]
pub mod table;
#[cfg(all(feature = "worker", target_arch = "wasm32"))]
mod worker;

/// PDF から取り込む最大ページ数。
pub const MAX_PAGES: usize = 300;

/// PDF のページを画像にするときの長辺(ピクセル)。ページが多いほど小さくして、
/// ボードの大きさ(同期する量)を抑える。
pub fn page_pixels(pages: usize) -> u32 {
    match pages {
        0..=20 => 1400,
        21..=80 => 1100,
        _ => 900,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn many_pages_are_rendered_smaller() {
        assert_eq!(super::page_pixels(1), 1400);
        assert!(super::page_pixels(300) < super::page_pixels(50));
    }
}
