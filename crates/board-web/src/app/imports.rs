//! 表計算・CSV の取り込み(読み取りは [`crate::sheets`]、並べるのは `board_core::tabular`)。

use board_core::Vec2;
use board_core::tabular::{self, Sheet};

use super::App;

pub use crate::sheets::{decode_text, is_csv, is_spreadsheet, read_spreadsheet};

impl App {
    /// シートを `at` を中心に並べる。何も置けなければ `false`。
    pub(super) fn insert_sheets(&mut self, sheets: Vec<Sheet>, at: Vec2) -> bool {
        if !self.can_edit() {
            return false;
        }
        let sticky = self.editor.sticky_color;
        self.board.end_gesture();
        let ids = tabular::insert(&mut self.board, sheets, at, sticky);
        self.board.end_gesture();
        if ids.is_empty() {
            return false;
        }
        self.editor.set_selection(ids);
        self.dirty = true;
        true
    }

    /// CSV / TSV の文字列を置く。
    pub(super) fn insert_csv(&mut self, text: &str, at: Vec2) -> bool {
        let sheet = Sheet {
            name: String::new(),
            rows: tabular::parse(text),
        };
        self.insert_sheets(vec![sheet], at)
    }
}
