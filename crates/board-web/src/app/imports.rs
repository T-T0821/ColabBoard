//! 表計算・CSV・Mermaid・PlantUML の取り込み(表の読み取りは [`crate::sheets`])。

use board_core::Vec2;
use board_core::tabular::{self, Sheet};

use super::App;

pub use crate::sheets::{decode_text, is_csv, is_spreadsheet, read_spreadsheet};

/// 位置をそのまま写す取り込み(draw.io・Miro)。
type PlacedImport =
    fn(&mut board_core::Board, &str, Vec2) -> Result<Vec<board_core::ElementId>, String>;

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

    /// Mermaid / PlantUML / draw.io / Miro の文字列なら図にして置く。どれでもなければ `false`。
    pub(super) fn insert_diagram_text(&mut self, text: &str) -> bool {
        use board_core::diagram::{mermaid, plantuml};
        use board_core::{drawio, miro};
        // draw.io と Miro は位置をそのまま写す。
        let placed: Option<PlacedImport> = if drawio::looks_like_drawio(text) {
            Some(drawio::import)
        } else if miro::looks_like_miro(text) {
            Some(miro::import)
        } else {
            None
        };
        if let Some(import) = placed {
            if !self.can_edit() {
                return true;
            }
            let at = self
                .cursor_world
                .unwrap_or_else(|| self.camera.screen_to_world(self.camera.viewport * 0.5));
            self.board.end_gesture();
            match import(&mut self.board, text, at) {
                Ok(ids) => {
                    self.board.end_gesture();
                    self.editor.set_selection(ids);
                    self.dirty = true;
                }
                Err(e) => super::show_error(&e),
            }
            return true;
        }
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

/// Mermaid / PlantUML / draw.io / Miro(JSON)のファイルか。
pub fn is_diagram_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or_default();
    matches!(
        ext,
        "mmd"
            | "mermaid"
            | "puml"
            | "plantuml"
            | "pu"
            | "iuml"
            | "wsd"
            | "drawio"
            | "dio"
            | "xml"
            | "svg"
            | "json"
    )
}
