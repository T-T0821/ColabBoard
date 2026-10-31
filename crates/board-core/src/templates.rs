//! テンプレート(よく使う配置をまとめて置く)。
//!
//! どのテンプレートも普通の要素(フレーム・付箋・図形・コネクタ・表)でできているので、
//! 置いた後は自由に編集できる。

use glam::Vec2;

use crate::board::Board;
use crate::color::{Rgba8, STICKY_COLORS};
use crate::connector::{Cap, Connector, Endpoint, PathStyle};
use crate::element::{Element, ElementId};
use crate::geom::Rect;
use crate::shape::{Shape, ShapeKind, TextAlign};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Template {
    Kpt,
    Kanban,
    Brainstorm,
    Matrix,
    Journey,
    Meeting,
}

impl Template {
    pub const ALL: [Template; 6] = [
        Template::Kpt,
        Template::Kanban,
        Template::Brainstorm,
        Template::Matrix,
        Template::Journey,
        Template::Meeting,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Template::Kpt => "kpt",
            Template::Kanban => "kanban",
            Template::Brainstorm => "brainstorm",
            Template::Matrix => "matrix",
            Template::Journey => "journey",
            Template::Meeting => "meeting",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.name() == s)
    }

    pub fn title(self) -> &'static str {
        match self {
            Template::Kpt => "振り返り(KPT)",
            Template::Kanban => "カンバン",
            Template::Brainstorm => "ブレインストーミング",
            Template::Matrix => "2×2 マトリクス",
            Template::Journey => "カスタマージャーニー",
            Template::Meeting => "定例ミーティング",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Template::Kpt => "Keep / Problem / Try の 3 列",
            Template::Kanban => "ToDo / 作業中 / 完了",
            Template::Brainstorm => "テーマから広げるアイデア",
            Template::Matrix => "重要度 × 緊急度",
            Template::Journey => "段階ごとの行動・感情・課題",
            Template::Meeting => "議題・決定事項・宿題",
        }
    }
}

/// 要素を組み立てる。座標は左上を原点にして置き、最後にまとめて動かす。
struct Builder<'a> {
    board: &'a mut Board,
    elements: Vec<Element>,
}

impl Builder<'_> {
    fn id(&mut self) -> ElementId {
        self.board.alloc_id()
    }

    fn push(&mut self, e: impl Into<Element>) -> ElementId {
        let e = e.into();
        let id = e.id();
        self.elements.push(e);
        id
    }

    fn frame(&mut self, title: &str, pos: Vec2, size: Vec2) -> ElementId {
        let id = self.id();
        let mut f = Shape::frame(id, pos, size);
        f.text = title.into();
        self.push(f)
    }

    fn sticky(&mut self, text: &str, pos: Vec2, color: Rgba8) -> ElementId {
        let id = self.id();
        let mut s = Shape::sticky(id, pos, color);
        s.size = Vec2::new(160.0, 160.0);
        s.font_size = 18.0;
        s.text = text.into();
        self.push(s)
    }

    fn text(&mut self, text: &str, pos: Vec2, width: f32, size: f32) -> ElementId {
        let id = self.id();
        let mut t = Shape::text(id, pos, Vec2::new(width, 10.0));
        t.text = text.into();
        t.font_size = size;
        t.align = TextAlign::Left;
        t.text_color = Rgba8::BLACK;
        t.fit_text_height();
        self.push(t)
    }

    fn shape(
        &mut self,
        kind: ShapeKind,
        text: &str,
        pos: Vec2,
        size: Vec2,
        fill: Rgba8,
    ) -> ElementId {
        let id = self.id();
        let mut s = Shape::new(id, kind, pos, size, fill);
        s.text = text.into();
        self.push(s)
    }

    fn connect(&mut self, from: ElementId, to: ElementId) -> ElementId {
        let id = self.id();
        let mut c = Connector::new(
            id,
            Endpoint::Attached {
                id: from,
                anchor: None,
            },
            Endpoint::Attached {
                id: to,
                anchor: None,
            },
        );
        c.style = PathStyle::Curved;
        c.end_cap = Cap::None;
        self.push(c)
    }

    fn arrow(&mut self, a: Vec2, b: Vec2) -> ElementId {
        let id = self.id();
        let mut c = Connector::new(id, Endpoint::Free(a), Endpoint::Free(b));
        c.style = PathStyle::Straight;
        c.color = Rgba8::hex(0x6b6b6b);
        c.width = 3.0;
        self.push(c)
    }

    /// 横に並んだ列(フレーム)と、各列の付箋。
    fn columns(&mut self, cols: &[(&str, Rgba8, &[&str])], size: Vec2) {
        for (i, (title, color, notes)) in cols.iter().enumerate() {
            let x = i as f32 * (size.x + 40.0);
            self.frame(title, Vec2::new(x, 0.0), size);
            for (j, note) in notes.iter().enumerate() {
                let col = (j % 2) as f32;
                let row = (j / 2) as f32;
                self.sticky(
                    note,
                    Vec2::new(x + 24.0 + col * 184.0, 24.0 + row * 184.0),
                    *color,
                );
            }
        }
    }
}

fn build(t: Template, b: &mut Builder<'_>) {
    match t {
        Template::Kpt => b.columns(
            &[
                ("Keep(続けること)", STICKY_COLORS[5], &["うまくいったこと"]),
                ("Problem(問題)", STICKY_COLORS[2], &["困ったこと"]),
                ("Try(次に試すこと)", STICKY_COLORS[4], &["やってみたいこと"]),
            ],
            Vec2::new(400.0, 560.0),
        ),
        Template::Kanban => b.columns(
            &[
                ("ToDo", STICKY_COLORS[0], &["タスク A", "タスク B"]),
                ("作業中", STICKY_COLORS[1], &["タスク C"]),
                ("完了", STICKY_COLORS[5], &["タスク D"]),
            ],
            Vec2::new(400.0, 640.0),
        ),
        Template::Brainstorm => {
            let center = Vec2::new(500.0, 380.0);
            let size = Vec2::new(240.0, 140.0);
            let theme = b.shape(
                ShapeKind::Ellipse,
                "テーマ",
                center - size * 0.5,
                size,
                Rgba8::hex(0xe8f0fe),
            );
            for i in 0..6 {
                let a = std::f32::consts::TAU * i as f32 / 6.0 - std::f32::consts::FRAC_PI_2;
                let p = center + Vec2::new(a.cos() * 420.0, a.sin() * 300.0) - Vec2::splat(80.0);
                let note = b.sticky("アイデア", p, STICKY_COLORS[i % STICKY_COLORS.len()]);
                b.connect(theme, note);
            }
        }
        Template::Matrix => {
            let s = 800.0;
            b.frame("重要度 × 緊急度", Vec2::ZERO, Vec2::splat(s));
            b.arrow(Vec2::new(40.0, s * 0.5), Vec2::new(s - 40.0, s * 0.5));
            b.arrow(Vec2::new(s * 0.5, s - 40.0), Vec2::new(s * 0.5, 40.0));
            b.text("緊急度 →", Vec2::new(s - 180.0, s * 0.5 + 8.0), 140.0, 18.0);
            b.text("↑ 重要度", Vec2::new(s * 0.5 + 12.0, 40.0), 140.0, 18.0);
            let labels = [
                ("重要・急ぎでない", Vec2::new(60.0, 100.0), STICKY_COLORS[4]),
                (
                    "重要・急ぎ",
                    Vec2::new(s * 0.5 + 60.0, 100.0),
                    STICKY_COLORS[2],
                ),
                (
                    "重要でない・急ぎでない",
                    Vec2::new(60.0, s * 0.5 + 60.0),
                    STICKY_COLORS[5],
                ),
                (
                    "重要でない・急ぎ",
                    Vec2::new(s * 0.5 + 60.0, s * 0.5 + 60.0),
                    STICKY_COLORS[0],
                ),
            ];
            for (t, p, c) in labels {
                b.sticky(t, p, c);
            }
        }
        Template::Journey => {
            let stages = ["認知", "検討", "購入", "利用", "継続"];
            let rows = ["段階", "行動", "感情", "課題", "アイデア"];
            let cell = Vec2::new(200.0, 110.0);
            let head = 140.0;
            let w = head + cell.x * stages.len() as f32;
            let h = cell.y * rows.len() as f32;
            b.frame(
                "カスタマージャーニー",
                Vec2::ZERO,
                Vec2::new(w + 48.0, h + 48.0),
            );
            let group = b.board.alloc_id().0;
            for (r, row) in rows.iter().enumerate() {
                for c in 0..=stages.len() {
                    let x = 24.0
                        + if c == 0 {
                            0.0
                        } else {
                            head + cell.x * (c - 1) as f32
                        };
                    let width = if c == 0 { head } else { cell.x };
                    let id = b.id();
                    let mut s = Shape::rect(
                        id,
                        Vec2::new(x, 24.0 + cell.y * r as f32),
                        Vec2::new(width, cell.y),
                        if r == 0 || c == 0 {
                            Rgba8::hex(0xf1f3f4)
                        } else {
                            Rgba8::WHITE
                        },
                    );
                    s.stroke = Rgba8::hex(0xb8bcc2);
                    s.stroke_width = 1.0;
                    s.corner_radius = 0.0;
                    s.font_size = 16.0;
                    s.meta.group = group;
                    s.text = match (r, c) {
                        (_, 0) => row.to_string(),
                        (0, c) => stages[c - 1].to_string(),
                        _ => String::new(),
                    };
                    b.push(s);
                }
            }
        }
        Template::Meeting => {
            let size = Vec2::new(420.0, 520.0);
            b.columns(
                &[
                    ("議題", STICKY_COLORS[4], &["話したいこと"]),
                    ("決定事項", STICKY_COLORS[5], &[]),
                    ("宿題(誰が・いつまで)", STICKY_COLORS[1], &[]),
                ],
                size,
            );
            b.text(
                "定例ミーティング  日付: ____  参加者: ____",
                Vec2::new(0.0, -120.0),
                900.0,
                28.0,
            );
        }
    }
}

/// テンプレートを中心 `center` に置く。置いた要素の ID を返す。
pub fn insert(board: &mut Board, t: Template, center: Vec2) -> Vec<ElementId> {
    let mut b = Builder {
        board,
        elements: Vec::new(),
    };
    build(t, &mut b);
    let Builder { board, elements } = b;
    let mut elements = elements;
    // 外接矩形の中心を `center` に合わせる(コネクタの経路は後で計算されるので図形で測る)。
    let bounds = elements
        .iter()
        .filter(|e| !matches!(e, Element::Connector(_)))
        .map(Element::bounds)
        .reduce(|a, b| a.union(&b))
        .unwrap_or(Rect::new(Vec2::ZERO, Vec2::ZERO));
    let d = center - bounds.center();
    for e in &mut elements {
        e.translate(d);
    }
    let ids = elements.iter().map(Element::id).collect();
    board.push_many(elements);
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_template_inserts_centered() {
        for t in Template::ALL {
            let mut b = Board::new();
            let before = b.len();
            let ids = insert(&mut b, t, Vec2::new(1000.0, 500.0));
            assert!(ids.len() >= 3, "{t:?}");
            assert_eq!(b.len(), before + ids.len());
            let shapes: Vec<_> = ids
                .iter()
                .filter(|id| !matches!(b.get(**id), Some(Element::Connector(_))))
                .copied()
                .collect();
            let r = b.bounds_of(&shapes).unwrap();
            assert!(
                (r.center() - Vec2::new(1000.0, 500.0)).length() < 1.0,
                "{t:?}"
            );
            assert_eq!(Template::from_name(t.name()), Some(t));
        }
    }

    #[test]
    fn journey_is_a_table() {
        let mut b = Board::new();
        let ids = insert(&mut b, Template::Journey, Vec2::ZERO);
        let g = crate::table::grid_of(&b, &ids).expect("表として編集できる");
        assert_eq!((g.rows(), g.cols()), (5, 6));
    }
}
