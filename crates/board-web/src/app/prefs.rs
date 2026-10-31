//! 最後に使った色・図形などを、このブラウザに覚えておく(次に開いたときも同じ設定で始まる)。

use std::cell::RefCell;

use super::App;

const KEY: &str = "colabboard.prefs";

thread_local! {
    /// 最後に保存した内容(変わったときだけ書き込む)。
    static SAVED: RefCell<String> = const { RefCell::new(String::new()) };
}

fn storage(app: &App) -> Option<web_sys::Storage> {
    app.window.local_storage().ok().flatten()
}

impl App {
    /// 保存した設定を読み込む。
    pub(super) fn load_prefs(&mut self) {
        if let Some(s) = storage(self).and_then(|s| s.get_item(KEY).ok().flatten()) {
            self.editor.load_prefs(&s);
        }
        SAVED.with(|v| *v.borrow_mut() = self.editor.save_prefs());
        // ツールバーの図形ボタンの絵を、覚えていた図形にする。
        if let Some(svg) = self
            .document
            .get_element_by_id("tool-shape")
            .and_then(|b| b.first_element_child())
        {
            svg.set_inner_html(&super::toolbar::icon(self.editor.shape_kind));
        }
    }

    /// 設定が変わっていたら保存する(毎フレーム呼ぶ)。
    pub(super) fn save_prefs(&mut self) {
        let now = self.editor.save_prefs();
        let changed = SAVED.with(|v| *v.borrow() != now);
        if !changed {
            return;
        }
        if let Some(s) = storage(self) {
            let _ = s.set_item(KEY, &now);
        }
        SAVED.with(|v| *v.borrow_mut() = now);
    }
}
