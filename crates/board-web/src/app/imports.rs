//! 表計算・CSV・Mermaid・PlantUML の取り込み(表の読み取りは [`crate::sheets`])。

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

    /// Mermaid / PlantUML の文字列なら図にして置く。どちらでもなければ `false`。
    pub(super) fn insert_diagram_text(&mut self, text: &str) -> bool {
        use board_core::diagram::{mermaid, plantuml};
        let parsed = if mermaid::looks_like_mermaid(text) {
            mermaid::parse(text)
        } else if plantuml::looks_like_plantuml(text) {
            plantuml::parse(text)
        } else {
            return false;
        };
        if !self.can_edit() {
            return true;
        }
        match parsed {
            Ok(spec) => {
                self.insert_diagram(&spec);
            }
            Err(e) => super::show_error(&e),
        }
        true
    }
}

/// Mermaid / PlantUML のファイルか。
pub fn is_diagram_file(name: &str) -> bool {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    matches!(
        ext.as_str(),
        "mmd" | "mermaid" | "puml" | "plantuml" | "pu" | "iuml" | "wsd"
    )
}
