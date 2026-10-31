//! 構成図パネル: AWS サービス・グループ枠のパレットと、図のサンプル挿入。

use std::cell::RefCell;
use std::rc::Rc;

use board_core::diagram::{self, DiagramSpec, catalog, samples};
use board_core::{Element as BoardElement, Rect, Vec2};
use web_sys::{Document, HtmlElement, HtmlSelectElement};

use super::{App, by_id, js_err, listen, on_click};

/// パレットから置くグループ枠の既定の大きさ。
const GROUP_SIZE: Vec2 = Vec2::new(520.0, 340.0);
/// 既存の要素の横に図を置くときの間隔。
const DIAGRAM_GAP: f32 = 200.0;

impl App {
    /// 画面中央のワールド座標。
    fn view_center(&self) -> Vec2 {
        self.camera.screen_to_world(self.camera.viewport * 0.5)
    }

    /// 要素をまとめて追加し、1 回の Undo で消せるようにして選択する。
    fn insert_elements(&mut self, elements: Vec<BoardElement>) {
        if elements.is_empty() {
            return;
        }
        if self.editor.editing().is_some() {
            self.end_text_edit(true);
        }
        self.editor.set_tool(board_core::Tool::Select);
        self.board.end_gesture();
        let ids: Vec<_> = elements.into_iter().map(|e| self.board.push(e)).collect();
        self.board.end_gesture();
        let bounds = self.board.bounds_of(&ids);
        self.editor.set_selection(ids);
        // はみ出す大きさなら全体が見えるようにする。
        if let Some(b) = bounds {
            let (min, max) = self.camera.visible_world_rect();
            if !Rect::new(min, max).contains_rect(&b) {
                self.camera.fit(b.min, b.max, 80.0);
            }
        }
        self.dirty = true;
    }

    /// 図の定義をレイアウトして画面中央に置く。既存の要素と重なるなら、
    /// ボードの右側の空いている所に置く。
    pub(super) fn insert_diagram(&mut self, spec: &DiagramSpec) -> usize {
        let center = self.view_center();
        let board = &mut self.board;
        let mut elements = diagram::build(spec, center, &mut || board.alloc_id());
        let bounds = elements
            .iter()
            .map(BoardElement::bounds)
            .reduce(|a, b| a.union(&b));
        if let (Some(b), Some(all)) = (bounds, self.board.bounds()) {
            let overlaps = self
                .board
                .elements()
                .iter()
                .any(|e| e.bounds().intersects(&b));
            if overlaps {
                let target = Vec2::new(all.max.x + DIAGRAM_GAP + b.size().x * 0.5, all.center().y);
                let d = target - b.center();
                elements.iter_mut().for_each(|e| e.translate(d));
            }
        }
        let n = elements.len();
        self.insert_elements(elements);
        n
    }

    fn insert_service(&mut self, kind_id: &str) {
        let kind = catalog::node_kind(kind_id);
        let mut s = diagram::node_shape(self.board.alloc_id(), kind, "", Vec2::ZERO);
        s.pos = self.view_center() - s.size * 0.5;
        self.insert_elements(vec![s.into()]);
    }

    fn insert_group(&mut self, kind_id: &str) {
        let kind = catalog::group_kind(kind_id);
        let min = self.view_center() - GROUP_SIZE * 0.5;
        let frame = diagram::group_shape(
            self.board.alloc_id(),
            kind,
            "",
            Rect::new(min, min + GROUP_SIZE),
        );
        self.insert_elements(vec![frame.into()]);
    }
}

fn add_option(doc: &Document, select: &HtmlSelectElement, value: &str, text: &str) {
    if let Ok(opt) = doc.create_element("option") {
        let _ = opt.set_attribute("value", value);
        opt.set_text_content(Some(text));
        let _ = select.append_child(&opt);
    }
}

fn category_name(c: catalog::Category) -> &'static str {
    use catalog::Category::*;
    match c {
        Compute => "コンピューティング",
        Storage => "ストレージ",
        Database => "データベース",
        Networking => "ネットワーク",
        Security => "セキュリティ",
        Integration => "アプリ統合",
        Analytics => "分析",
        Management => "管理",
        MachineLearning => "機械学習",
        Frontend => "フロントエンド",
        DevTools => "開発ツール",
        General => "一般",
        Flow => "フローチャート",
    }
}

pub(super) fn install(app: &Rc<RefCell<App>>, doc: &Document) -> Result<(), String> {
    let panel: HtmlElement = by_id(doc, "diagram-panel")?;
    {
        let toggle: HtmlElement = by_id(doc, "diagram-toggle")?;
        let toggle2 = toggle.clone();
        let panel = panel.clone();
        listen(
            toggle.as_ref(),
            "click",
            false,
            move |_: web_sys::MouseEvent| {
                let open = panel.hidden();
                panel.set_hidden(!open);
                let _ = toggle2.set_attribute("aria-expanded", if open { "true" } else { "false" });
            },
        )?;
    }

    // AWS サービス(カテゴリごとに見出しを付ける)。
    let services: HtmlSelectElement = by_id(doc, "diagram-service")?;
    let mut current: Option<(catalog::Category, web_sys::Element)> = None;
    for k in diagram::NODE_KINDS {
        if current.as_ref().map(|(c, _)| *c) != Some(k.category) {
            let group = doc.create_element("optgroup").map_err(js_err)?;
            let _ = group.set_attribute("label", category_name(k.category));
            services.append_child(&group).map_err(js_err)?;
            current = Some((k.category, group));
        }
        let text = match k.title {
            "" => match k.style {
                catalog::NodeStyle::Process => "処理",
                catalog::NodeStyle::Decision => "判断",
                catalog::NodeStyle::Terminal => "開始・終了",
                catalog::NodeStyle::Data => "データ",
                catalog::NodeStyle::Note => "メモ",
                catalog::NodeStyle::Service => k.id,
            },
            t => t,
        };
        if let Some((_, group)) = &current {
            let opt = doc.create_element("option").map_err(js_err)?;
            let _ = opt.set_attribute("value", k.id);
            opt.set_text_content(Some(text));
            group.append_child(&opt).map_err(js_err)?;
        }
    }
    services.set_value("ec2");

    let groups: HtmlSelectElement = by_id(doc, "diagram-group")?;
    for g in diagram::GROUP_KINDS {
        add_option(
            doc,
            &groups,
            g.id,
            if g.title.is_empty() { "枠" } else { g.title },
        );
    }
    groups.set_value("vpc");

    on_click(doc, "diagram-add-service", app, move |a| {
        a.insert_service(&services.value())
    })?;
    on_click(doc, "diagram-add-group", app, move |a| {
        a.insert_group(&groups.value())
    })?;

    // サンプル
    let list: HtmlElement = by_id(doc, "diagram-samples")?;
    for (name, json) in samples::SAMPLES {
        let button = doc.create_element("button").map_err(js_err)?;
        button.set_text_content(Some(name));
        list.append_child(&button).map_err(js_err)?;
        let app = app.clone();
        listen(
            button.as_ref(),
            "click",
            false,
            move |_: web_sys::MouseEvent| {
                if let Ok(spec) = DiagramSpec::from_json(json) {
                    app.borrow_mut().insert_diagram(&spec);
                }
            },
        )?;
    }
    Ok(())
}
