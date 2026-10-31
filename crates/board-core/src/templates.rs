//! テンプレート(よく使う配置をまとめて置く)。
//!
//! どのテンプレートも普通の要素(フレーム・付箋・図形・コネクタ・表)でできているので、
//! 置いた後は自由に編集できる。各テンプレートには「進め方」があり、
//! 手順ごとにタイマー・投票・AI をすぐ始められる(`Template::steps`)。
//!
//! 振り返りやワークショップの手法は広く知られた進め方を参考に、文言と配置は独自に構成している。

use glam::Vec2;

use crate::board::Board;
use crate::color::{Rgba8, STICKY_COLORS};
use crate::connector::{Cap, Connector, Endpoint, PathStyle};
use crate::element::{Element, ElementId};
use crate::geom::Rect;
use crate::shape::{Shape, ShapeKind, TextAlign};

const Y: Rgba8 = STICKY_COLORS[0];
const O: Rgba8 = STICKY_COLORS[1];
const P: Rgba8 = STICKY_COLORS[2];
const V: Rgba8 = STICKY_COLORS[3];
const B: Rgba8 = STICKY_COLORS[4];
const G: Rgba8 = STICKY_COLORS[5];
const GRAY: Rgba8 = Rgba8::hex(0xdadce0);
const WHITE: Rgba8 = Rgba8::WHITE;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Template {
    // 振り返り
    Kpt,
    WentWell,
    Ywt,
    StartStopContinue,
    MadSadGlad,
    FourLs,
    Starfish,
    Daki,
    Kalm,
    PlusDelta,
    RoseBudThorn,
    LikedWishedWondered,
    Sailboat,
    HotAirBalloon,
    ThreeLittlePigs,
    Timeline,
    Unspoken,
    Esvp,
    Weather,
    // ワークショップ
    Icebreaker,
    HopesConcerns,
    Brainstorm,
    Affinity,
    Crazy8s,
    HowMightWe,
    OneTwoFourAll,
    LeanCoffee,
    DecisionJam,
    SixHats,
    Pmi,
    FiveWhys,
    Journey,
    EmpathyMap,
    Persona,
    Grow,
    Drucker,
    // 戦略・企画
    LeanCanvas,
    BusinessModelCanvas,
    Swot,
    Pest,
    StakeholderMap,
    AsIsToBe,
    PreMortem,
    Okr,
    // 計画・進行
    Kickoff,
    Roadmap,
    StoryMap,
    SprintPlanning,
    Moscow,
    PriorityMatrix,
    Matrix,
    Kanban,
    Meeting,
}

/// テンプレートの分類(選ぶ画面の見出し)。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    Retro,
    Workshop,
    Strategy,
    Planning,
}

impl Category {
    pub const ALL: [Category; 4] = [
        Category::Retro,
        Category::Workshop,
        Category::Strategy,
        Category::Planning,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Category::Retro => "振り返り",
            Category::Workshop => "ワークショップ",
            Category::Strategy => "戦略・企画",
            Category::Planning => "計画・進行",
        }
    }
}

/// 進め方の 1 手順で、ファシリテーション機能のどれを使うか。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepAction {
    /// 説明だけ。
    None,
    /// 分のタイマーを始める。
    Timer(u32),
    /// 1 人あたりの票数で投票を始める。
    Vote(u32),
    /// 投票を締め切る。
    CloseVote,
    /// AI でグループ分けする。
    AiCluster,
    /// AI で要約する。
    AiSummary,
}

/// 進め方の 1 手順。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Step {
    pub label: &'static str,
    pub action: StepAction,
}

impl Step {
    /// 手順の説明(「各自で書く(5 分)」のように使う機能を添える)。
    pub fn describe(&self) -> String {
        match self.action {
            StepAction::Timer(m) => format!("{}({m} 分)", self.label),
            StepAction::Vote(n) => format!("{}(1 人 {n} 票)", self.label),
            _ => self.label.to_string(),
        }
    }
}

use StepAction::{AiCluster, AiSummary, CloseVote, Timer, Vote};

/// 振り返りの定番の進め方。
const RETRO_STEPS: &[Step] = &[
    Step {
        label: "各自で付箋に書く",
        action: Timer(5),
    },
    Step {
        label: "1 人ずつ読み上げて共有する",
        action: Timer(10),
    },
    Step {
        label: "AI で似た意見をまとめる",
        action: AiCluster,
    },
    Step {
        label: "話したいものに投票",
        action: Vote(3),
    },
    Step {
        label: "投票を締め切って上位から話す",
        action: CloseVote,
    },
    Step {
        label: "決まったことを要約する",
        action: AiSummary,
    },
];

/// たとえ(帆船など)を使う振り返り。
const METAPHOR_STEPS: &[Step] = &[
    Step {
        label: "たとえの意味を確認する",
        action: StepAction::None,
    },
    Step {
        label: "各自で付箋を貼る",
        action: Timer(7),
    },
    Step {
        label: "エリアごとに読み上げる",
        action: Timer(10),
    },
    Step {
        label: "手を打ちたいものに投票",
        action: Vote(3),
    },
    Step {
        label: "投票を締め切る",
        action: CloseVote,
    },
    Step {
        label: "次のアクションを要約する",
        action: AiSummary,
    },
];

/// チェックイン(気分を見える化する)。
const CHECKIN_STEPS: &[Step] = &[
    Step {
        label: "自分に近い所に名前の付箋を置く",
        action: Timer(2),
    },
    Step {
        label: "結果を見て一言ずつ話す",
        action: Timer(5),
    },
];

const CANVAS_STEPS: &[Step] = &[
    Step {
        label: "各自で書く",
        action: Timer(15),
    },
    Step {
        label: "ブロックごとに共有",
        action: Timer(15),
    },
    Step {
        label: "検証したい仮説に投票",
        action: Vote(3),
    },
    Step {
        label: "投票を締め切る",
        action: CloseVote,
    },
    Step {
        label: "全体を要約する",
        action: AiSummary,
    },
];

const MATRIX_STEPS: &[Step] = &[
    Step {
        label: "候補を付箋に書く",
        action: Timer(5),
    },
    Step {
        label: "話し合いながら配置する",
        action: Timer(10),
    },
    Step {
        label: "結論を要約する",
        action: AiSummary,
    },
];

const PLAN_STEPS: &[Step] = &[
    Step {
        label: "候補を書き出す",
        action: Timer(10),
    },
    Step {
        label: "優先したいものに投票",
        action: Vote(5),
    },
    Step {
        label: "投票を締め切って並べ替える",
        action: CloseVote,
    },
];

const RESEARCH_STEPS: &[Step] = &[
    Step {
        label: "各自で付箋を書く",
        action: Timer(10),
    },
    Step {
        label: "全員で読み合わせる",
        action: Timer(10),
    },
    Step {
        label: "大事な課題に投票",
        action: Vote(3),
    },
    Step {
        label: "投票を締め切る",
        action: CloseVote,
    },
    Step {
        label: "気づきを要約する",
        action: AiSummary,
    },
];

impl Template {
    pub const ALL: [Template; 53] = [
        Template::Kpt,
        Template::WentWell,
        Template::Ywt,
        Template::StartStopContinue,
        Template::MadSadGlad,
        Template::FourLs,
        Template::Starfish,
        Template::Daki,
        Template::Kalm,
        Template::PlusDelta,
        Template::RoseBudThorn,
        Template::LikedWishedWondered,
        Template::Sailboat,
        Template::HotAirBalloon,
        Template::ThreeLittlePigs,
        Template::Timeline,
        Template::Unspoken,
        Template::Esvp,
        Template::Weather,
        Template::Icebreaker,
        Template::HopesConcerns,
        Template::Brainstorm,
        Template::Affinity,
        Template::Crazy8s,
        Template::HowMightWe,
        Template::OneTwoFourAll,
        Template::LeanCoffee,
        Template::DecisionJam,
        Template::SixHats,
        Template::Pmi,
        Template::FiveWhys,
        Template::Journey,
        Template::EmpathyMap,
        Template::Persona,
        Template::Grow,
        Template::Drucker,
        Template::LeanCanvas,
        Template::BusinessModelCanvas,
        Template::Swot,
        Template::Pest,
        Template::StakeholderMap,
        Template::AsIsToBe,
        Template::PreMortem,
        Template::Okr,
        Template::Kickoff,
        Template::Roadmap,
        Template::StoryMap,
        Template::SprintPlanning,
        Template::Moscow,
        Template::PriorityMatrix,
        Template::Matrix,
        Template::Kanban,
        Template::Meeting,
    ];

    /// (名前, 表示名, 説明, 分類)
    fn info(self) -> (&'static str, &'static str, &'static str, Category) {
        use Category::*;
        match self {
            Template::Kpt => ("kpt", "KPT", "Keep / Problem / Try の 3 列", Retro),
            Template::WentWell => (
                "went-well",
                "良かった・改善・アクション",
                "いちばん素朴な 3 列の振り返り",
                Retro,
            ),
            Template::Ywt => (
                "ywt",
                "YWT",
                "やったこと・わかったこと・次にやること",
                Retro,
            ),
            Template::StartStopContinue => (
                "start-stop-continue",
                "Start / Stop / Continue",
                "始める・やめる・続けること",
                Retro,
            ),
            Template::MadSadGlad => (
                "mad-sad-glad",
                "Mad / Sad / Glad",
                "怒・哀・喜で気持ちから振り返る",
                Retro,
            ),
            Template::FourLs => ("4l", "4L", "Liked / Learned / Lacked / Longed for", Retro),
            Template::Starfish => (
                "starfish",
                "スターフィッシュ",
                "増やす・続ける・始める・減らす・やめる",
                Retro,
            ),
            Template::Daki => ("daki", "DAKI", "Drop / Add / Keep / Improve", Retro),
            Template::Kalm => ("kalm", "KALM", "Keep / Add / More / Less", Retro),
            Template::PlusDelta => (
                "plus-delta",
                "プラス / デルタ",
                "良かった点と変えたい点の 2 列",
                Retro,
            ),
            Template::RoseBudThorn => (
                "rose-bud-thorn",
                "バラ・つぼみ・トゲ",
                "うまくいったこと・芽・痛み",
                Retro,
            ),
            Template::LikedWishedWondered => (
                "liked-wished-wondered",
                "Liked / Wished / Wondered",
                "好き・願い・疑問で振り返る",
                Retro,
            ),
            Template::Sailboat => ("sailboat", "帆船", "追い風・錨・岩・島のたとえで", Retro),
            Template::HotAirBalloon => (
                "hot-air-balloon",
                "熱気球",
                "押し上げる熱気と重い砂袋、嵐と晴れ空",
                Retro,
            ),
            Template::ThreeLittlePigs => (
                "three-little-pigs",
                "三匹の子ぶた",
                "わら・木・レンガの家で丈夫さを測る",
                Retro,
            ),
            Template::Timeline => (
                "timeline",
                "タイムライン",
                "期間を時系列でたどり出来事と気分を並べる",
                Retro,
            ),
            Template::Unspoken => (
                "unspoken",
                "言いにくいこと",
                "部屋の中の象・死んだ魚・吐き出したいこと",
                Retro,
            ),
            Template::Esvp => (
                "esvp",
                "ESVP チェックイン",
                "探検家・買い物客・休暇中・囚人で参加姿勢を知る",
                Retro,
            ),
            Template::Weather => (
                "weather",
                "天気チェックイン",
                "今の気分を晴れ〜嵐で表す",
                Retro,
            ),
            Template::Icebreaker => (
                "icebreaker",
                "アイスブレイク",
                "チェックインの問いと自己紹介",
                Workshop,
            ),
            Template::HopesConcerns => (
                "hopes-concerns",
                "期待と不安",
                "始める前に期待と心配を出し合う",
                Workshop,
            ),
            Template::Brainstorm => (
                "brainstorm",
                "ブレインストーミング",
                "テーマから広げるアイデア",
                Workshop,
            ),
            Template::Affinity => (
                "affinity",
                "親和図(KJ 法)",
                "付箋を似たものどうしでまとめる",
                Workshop,
            ),
            Template::Crazy8s => (
                "crazy-8s",
                "Crazy 8s",
                "8 分で 8 案をスケッチする",
                Workshop,
            ),
            Template::HowMightWe => (
                "how-might-we",
                "How Might We",
                "課題を「どうすれば〜できる?」に言い換える",
                Workshop,
            ),
            Template::OneTwoFourAll => (
                "1-2-4-all",
                "1-2-4-全体",
                "1 人→2 人→4 人→全体で考えを育てる",
                Workshop,
            ),
            Template::LeanCoffee => (
                "lean-coffee",
                "リーンコーヒー",
                "議題を投票で選び、時間を区切って話す",
                Workshop,
            ),
            Template::DecisionJam => (
                "decision-jam",
                "決断ジャム",
                "問題出しから実行まで短時間で決める",
                Workshop,
            ),
            Template::SixHats => (
                "six-hats",
                "6 つの帽子",
                "事実・感情・慎重・楽観・創造・進行の視点",
                Workshop,
            ),
            Template::Pmi => (
                "pmi",
                "PMI",
                "Plus / Minus / Interesting で案を評価",
                Workshop,
            ),
            Template::FiveWhys => (
                "five-whys",
                "なぜなぜ分析",
                "「なぜ?」を 5 回くり返して真因へ",
                Workshop,
            ),
            Template::Journey => (
                "journey",
                "カスタマージャーニー",
                "段階ごとの行動・感情・課題",
                Workshop,
            ),
            Template::EmpathyMap => (
                "empathy-map",
                "共感マップ",
                "言う・考える・する・感じる",
                Workshop,
            ),
            Template::Persona => (
                "persona",
                "ペルソナ",
                "想定するユーザー像を 1 枚に",
                Workshop,
            ),
            Template::Grow => (
                "grow",
                "GROW モデル",
                "目標・現状・選択肢・意思で考える",
                Workshop,
            ),
            Template::Drucker => (
                "drucker",
                "ドラッカー風エクササイズ",
                "得意・貢献・価値観・期待を伝え合う",
                Workshop,
            ),
            Template::LeanCanvas => (
                "lean-canvas",
                "リーンキャンバス",
                "新規事業の仮説を 1 枚に",
                Strategy,
            ),
            Template::BusinessModelCanvas => (
                "business-model-canvas",
                "ビジネスモデルキャンバス",
                "9 つの要素で事業を整理",
                Strategy,
            ),
            Template::Swot => ("swot", "SWOT 分析", "強み・弱み・機会・脅威", Strategy),
            Template::Pest => (
                "pest",
                "PEST 分析",
                "政治・経済・社会・技術の外部環境",
                Strategy,
            ),
            Template::StakeholderMap => (
                "stakeholder-map",
                "関係者マップ",
                "影響力 × 関心で関わり方を決める",
                Strategy,
            ),
            Template::AsIsToBe => (
                "as-is-to-be",
                "現状と理想",
                "As-Is / To-Be とギャップ・打ち手",
                Strategy,
            ),
            Template::PreMortem => (
                "pre-mortem",
                "プレモーテム",
                "失敗したと仮定して原因と予防策を出す",
                Strategy,
            ),
            Template::Okr => ("okr", "OKR", "目標と主要な成果", Strategy),
            Template::Kickoff => (
                "kickoff",
                "キックオフ",
                "目的・成功の定義・やらないこと・体制",
                Planning,
            ),
            Template::Roadmap => ("roadmap", "ロードマップ", "四半期ごとの計画", Planning),
            Template::StoryMap => (
                "story-map",
                "ストーリーマップ",
                "ユーザーの行動に沿ってリリースを切る",
                Planning,
            ),
            Template::SprintPlanning => (
                "sprint-planning",
                "スプリント計画",
                "ゴール・バックログ・キャパシティ",
                Planning,
            ),
            Template::Moscow => (
                "moscow",
                "MoSCoW",
                "Must / Should / Could / Won't で仕分け",
                Planning,
            ),
            Template::PriorityMatrix => (
                "priority-matrix",
                "優先度マトリクス",
                "効果 × 手間で優先度を決める",
                Planning,
            ),
            Template::Matrix => ("matrix", "2×2 マトリクス", "重要度 × 緊急度", Planning),
            Template::Kanban => ("kanban", "カンバン", "ToDo / 作業中 / 完了", Planning),
            Template::Meeting => (
                "meeting",
                "定例ミーティング",
                "議題・決定事項・宿題",
                Planning,
            ),
        }
    }

    pub fn name(self) -> &'static str {
        self.info().0
    }

    pub fn from_name(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.name() == s)
    }

    pub fn title(self) -> &'static str {
        self.info().1
    }

    pub fn description(self) -> &'static str {
        self.info().2
    }

    pub fn category(self) -> Category {
        self.info().3
    }

    /// おすすめの進め方。各手順からタイマー・投票・AI をすぐ始められる。
    pub fn steps(self) -> &'static [Step] {
        use Template::*;
        match self {
            Kpt | WentWell | Ywt | StartStopContinue | MadSadGlad | FourLs | Starfish | Daki
            | Kalm | PlusDelta | RoseBudThorn | LikedWishedWondered | Unspoken => RETRO_STEPS,
            Sailboat | HotAirBalloon | ThreeLittlePigs => METAPHOR_STEPS,
            Timeline => &[
                Step {
                    label: "各自で出来事を時系列に貼る",
                    action: Timer(10),
                },
                Step {
                    label: "良かった・つらかったを書き足す",
                    action: Timer(5),
                },
                Step {
                    label: "通しで読み上げる",
                    action: Timer(10),
                },
                Step {
                    label: "学びを要約する",
                    action: AiSummary,
                },
            ],
            Esvp | Weather => CHECKIN_STEPS,
            Icebreaker => &[
                Step {
                    label: "1 人 1 分で自己紹介",
                    action: Timer(1),
                },
                Step {
                    label: "付箋に答えを書く",
                    action: Timer(3),
                },
            ],
            HopesConcerns => &[
                Step {
                    label: "期待と不安を各自で書く",
                    action: Timer(5),
                },
                Step {
                    label: "読み上げる",
                    action: Timer(10),
                },
                Step {
                    label: "AI で似たものをまとめる",
                    action: AiCluster,
                },
                Step {
                    label: "扱いたい不安に投票",
                    action: Vote(2),
                },
                Step {
                    label: "投票を締め切る",
                    action: CloseVote,
                },
            ],
            Brainstorm => &[
                Step {
                    label: "質より量。各自でアイデアを出す",
                    action: Timer(8),
                },
                Step {
                    label: "AI で似たアイデアをまとめる",
                    action: AiCluster,
                },
                Step {
                    label: "良いと思うものに投票",
                    action: Vote(5),
                },
                Step {
                    label: "投票を締め切る",
                    action: CloseVote,
                },
            ],
            Affinity => &[
                Step {
                    label: "事実・意見を付箋に書き出す",
                    action: Timer(10),
                },
                Step {
                    label: "AI でグループ分け",
                    action: AiCluster,
                },
                Step {
                    label: "グループ名を見直して話し合う",
                    action: Timer(10),
                },
                Step {
                    label: "まとめを要約する",
                    action: AiSummary,
                },
            ],
            Crazy8s => &[
                Step {
                    label: "課題を確認する",
                    action: StepAction::None,
                },
                Step {
                    label: "1 枠 1 分で 8 案を描く",
                    action: Timer(8),
                },
                Step {
                    label: "1 人ずつ紹介する",
                    action: Timer(10),
                },
                Step {
                    label: "良い案に投票",
                    action: Vote(3),
                },
                Step {
                    label: "投票を締め切る",
                    action: CloseVote,
                },
            ],
            HowMightWe => &[
                Step {
                    label: "課題を書き出す",
                    action: Timer(5),
                },
                Step {
                    label: "「どうすれば〜?」に言い換える",
                    action: Timer(5),
                },
                Step {
                    label: "取り組む問いに投票",
                    action: Vote(3),
                },
                Step {
                    label: "投票を締め切る",
                    action: CloseVote,
                },
                Step {
                    label: "アイデアを出す",
                    action: Timer(8),
                },
            ],
            OneTwoFourAll => &[
                Step {
                    label: "1 人で考える",
                    action: Timer(1),
                },
                Step {
                    label: "2 人で話す",
                    action: Timer(2),
                },
                Step {
                    label: "4 人で話す",
                    action: Timer(4),
                },
                Step {
                    label: "全体で共有する",
                    action: Timer(5),
                },
                Step {
                    label: "出た考えを要約する",
                    action: AiSummary,
                },
            ],
            LeanCoffee => &[
                Step {
                    label: "話したい議題を書く",
                    action: Timer(3),
                },
                Step {
                    label: "議題に投票",
                    action: Vote(2),
                },
                Step {
                    label: "投票を締め切って順番を決める",
                    action: CloseVote,
                },
                Step {
                    label: "1 つの議題を話す",
                    action: Timer(8),
                },
                Step {
                    label: "話した内容を要約する",
                    action: AiSummary,
                },
            ],
            DecisionJam => &[
                Step {
                    label: "うまくいっていることを書く",
                    action: Timer(4),
                },
                Step {
                    label: "問題を書く",
                    action: Timer(4),
                },
                Step {
                    label: "取り組む問題に投票",
                    action: Vote(3),
                },
                Step {
                    label: "投票を締め切る",
                    action: CloseVote,
                },
                Step {
                    label: "解決策を出す",
                    action: Timer(5),
                },
                Step {
                    label: "解決策に投票",
                    action: Vote(3),
                },
                Step {
                    label: "投票を締め切ってやることを決める",
                    action: CloseVote,
                },
            ],
            SixHats => &[
                Step {
                    label: "帽子ごとに順番に考える(1 色)",
                    action: Timer(3),
                },
                Step {
                    label: "全員で同じ色の視点を共有",
                    action: Timer(5),
                },
                Step {
                    label: "青の帽子でまとめる",
                    action: AiSummary,
                },
            ],
            Pmi => &[
                Step {
                    label: "Plus・Minus・Interesting を書く",
                    action: Timer(6),
                },
                Step {
                    label: "読み上げる",
                    action: Timer(8),
                },
                Step {
                    label: "判断を要約する",
                    action: AiSummary,
                },
            ],
            FiveWhys => &[
                Step {
                    label: "問題を 1 文で書く",
                    action: Timer(2),
                },
                Step {
                    label: "「なぜ?」をくり返して掘り下げる",
                    action: Timer(15),
                },
                Step {
                    label: "真因と対策をまとめる",
                    action: AiSummary,
                },
            ],
            Journey | EmpathyMap | Persona => RESEARCH_STEPS,
            Grow => &[
                Step {
                    label: "目標を決める",
                    action: Timer(5),
                },
                Step {
                    label: "現状を書き出す",
                    action: Timer(5),
                },
                Step {
                    label: "選択肢を広げる",
                    action: Timer(8),
                },
                Step {
                    label: "やることに投票",
                    action: Vote(2),
                },
                Step {
                    label: "投票を締め切って決める",
                    action: CloseVote,
                },
            ],
            Drucker => &[
                Step {
                    label: "自分の行を書く",
                    action: Timer(10),
                },
                Step {
                    label: "1 人ずつ発表する",
                    action: Timer(15),
                },
                Step {
                    label: "ずれていた期待を要約する",
                    action: AiSummary,
                },
            ],
            LeanCanvas | BusinessModelCanvas | Swot | Pest => CANVAS_STEPS,
            StakeholderMap | PriorityMatrix | Matrix => MATRIX_STEPS,
            AsIsToBe => &[
                Step {
                    label: "現状と理想を書く",
                    action: Timer(10),
                },
                Step {
                    label: "ギャップを話し合う",
                    action: Timer(10),
                },
                Step {
                    label: "打ち手に投票",
                    action: Vote(3),
                },
                Step {
                    label: "投票を締め切る",
                    action: CloseVote,
                },
            ],
            PreMortem => &[
                Step {
                    label: "失敗した理由を各自で想像する",
                    action: Timer(7),
                },
                Step {
                    label: "AI で原因をまとめる",
                    action: AiCluster,
                },
                Step {
                    label: "起こりやすいものに投票",
                    action: Vote(3),
                },
                Step {
                    label: "投票を締め切る",
                    action: CloseVote,
                },
                Step {
                    label: "予防策を書く",
                    action: Timer(10),
                },
            ],
            Okr => &[
                Step {
                    label: "目標(O)の案を出す",
                    action: Timer(5),
                },
                Step {
                    label: "目標に投票",
                    action: Vote(2),
                },
                Step {
                    label: "投票を締め切る",
                    action: CloseVote,
                },
                Step {
                    label: "主要な成果(KR)を書く",
                    action: Timer(10),
                },
            ],
            Kickoff => &[
                Step {
                    label: "各ブロックを全員で埋める",
                    action: Timer(20),
                },
                Step {
                    label: "不安なリスクに投票",
                    action: Vote(3),
                },
                Step {
                    label: "投票を締め切る",
                    action: CloseVote,
                },
                Step {
                    label: "合意したことを要約する",
                    action: AiSummary,
                },
            ],
            Roadmap | SprintPlanning | Kanban | StoryMap | Moscow => PLAN_STEPS,
            Meeting => &[
                Step {
                    label: "議題を確認",
                    action: Timer(2),
                },
                Step {
                    label: "議論",
                    action: Timer(25),
                },
                Step {
                    label: "決定事項と宿題をまとめる",
                    action: AiSummary,
                },
            ],
        }
    }
}

/// 要素を組み立てる。座標は左上を原点にして置き、最後にまとめて動かす。
struct Builder<'a> {
    board: &'a mut Board,
    elements: Vec<Element>,
}

/// 列: (見出し, 書き方の例, 付箋の色)
type Col<'a> = (&'a str, &'a str, Rgba8);
/// キャンバスの区画: (見出し, 書き方の例, 付箋の色, [x, y, 幅, 高さ](格子の単位))
type Cell<'a> = (&'a str, &'a str, Rgba8, [f32; 4]);

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

    fn link(&mut self, from: ElementId, to: ElementId, style: PathStyle, cap: Cap) -> ElementId {
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
        c.style = style;
        c.end_cap = cap;
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

    /// 書き方の例を 1 枚ずつ置いた列。列が多いほど細くする。
    fn cols(&mut self, cols: &[Col<'_>]) {
        let w = match cols.len() {
            0..=3 => 400.0,
            4 => 360.0,
            _ => 300.0,
        };
        let mut x = 0.0;
        for (title, hint, color) in cols {
            self.frame(title, Vec2::new(x, 0.0), Vec2::new(w, 600.0));
            if !hint.is_empty() {
                self.sticky(hint, Vec2::new(x + 24.0, 24.0), *color);
            }
            x += w + 40.0;
        }
    }

    /// 区画(フレーム)を格子に並べたキャンバス。`unit` は格子 1 つの大きさ。
    fn canvas(&mut self, unit: Vec2, cells: &[Cell<'_>]) {
        const GAP: f32 = 48.0;
        for (title, hint, color, [x, y, w, h]) in cells {
            let pos = Vec2::new(x * (unit.x + GAP), y * (unit.y + GAP));
            let size = Vec2::new(w * unit.x + (w - 1.0) * GAP, h * unit.y + (h - 1.0) * GAP);
            self.frame(title, pos, size);
            if !hint.is_empty() {
                self.sticky(hint, pos + Vec2::splat(24.0), *color);
            }
        }
    }

    /// 2 軸の 4 象限。象限は左上・右上・左下・右下の順。
    fn axes(&mut self, title: &str, x_label: &str, y_label: &str, quads: [(&str, Rgba8); 4]) {
        let s = 800.0;
        self.frame(title, Vec2::ZERO, Vec2::splat(s));
        self.arrow(Vec2::new(40.0, s * 0.5), Vec2::new(s - 40.0, s * 0.5));
        self.arrow(Vec2::new(s * 0.5, s - 40.0), Vec2::new(s * 0.5, 40.0));
        self.text(x_label, Vec2::new(s - 200.0, s * 0.5 + 8.0), 160.0, 18.0);
        self.text(y_label, Vec2::new(s * 0.5 + 12.0, 40.0), 160.0, 18.0);
        let at = [
            Vec2::new(60.0, 100.0),
            Vec2::new(s * 0.5 + 60.0, 100.0),
            Vec2::new(60.0, s * 0.5 + 60.0),
            Vec2::new(s * 0.5 + 60.0, s * 0.5 + 60.0),
        ];
        for ((t, c), p) in quads.into_iter().zip(at) {
            self.sticky(t, p, c);
        }
    }

    /// 表(長方形のセルを同じグループにまとめたもの)。表の編集機能で行や列を足せる。
    fn table(&mut self, title: &str, corner: &str, cols: &[&str], rows: &[&str], cell: Vec2) {
        let head = 160.0;
        let w = head + cell.x * cols.len() as f32;
        let h = cell.y * (rows.len() + 1) as f32;
        self.frame(title, Vec2::ZERO, Vec2::new(w + 48.0, h + 48.0));
        let group = self.board.alloc_id().0;
        for r in 0..=rows.len() {
            for c in 0..=cols.len() {
                let x = 24.0
                    + if c == 0 {
                        0.0
                    } else {
                        head + cell.x * (c - 1) as f32
                    };
                let width = if c == 0 { head } else { cell.x };
                let height = if r == 0 { 72.0 } else { cell.y };
                let y = 24.0
                    + if r == 0 {
                        0.0
                    } else {
                        72.0 + cell.y * (r - 1) as f32
                    };
                let id = self.id();
                let mut s = Shape::rect(
                    id,
                    Vec2::new(x, y),
                    Vec2::new(width, height),
                    if r == 0 || c == 0 {
                        Rgba8::hex(0xf1f3f4)
                    } else {
                        WHITE
                    },
                );
                s.stroke = Rgba8::hex(0xb8bcc2);
                s.stroke_width = 1.0;
                s.corner_radius = 0.0;
                s.font_size = 16.0;
                s.meta.group = group;
                s.text = match (r, c) {
                    (0, 0) => corner.to_string(),
                    (_, 0) => rows[r - 1].to_string(),
                    (0, c) => cols[c - 1].to_string(),
                    _ => String::new(),
                };
                self.push(s);
            }
        }
    }
}

fn build(t: Template, b: &mut Builder<'_>) {
    use Template::*;
    match t {
        // ---- 振り返り ----
        Kpt => b.columns(
            &[
                ("Keep(続けること)", G, &["うまくいったこと"]),
                ("Problem(問題)", P, &["困ったこと"]),
                ("Try(次に試すこと)", B, &["やってみたいこと"]),
            ],
            Vec2::new(400.0, 560.0),
        ),
        WentWell => b.cols(&[
            ("良かったこと", "うまくいったこと・感謝", G),
            ("改善したいこと", "もやもやしたこと", O),
            ("アクション", "誰が・いつまでに・何を", B),
        ]),
        Ywt => b.cols(&[
            ("Y: やったこと", "取り組んだこと", Y),
            ("W: わかったこと", "気づき・学び", G),
            ("T: 次にやること", "次の一手", B),
        ]),
        StartStopContinue => b.cols(&[
            ("Start(始めること)", "新しく試したいこと", B),
            ("Stop(やめること)", "効果がなかったこと", P),
            ("Continue(続けること)", "うまくいっていること", G),
        ]),
        MadSadGlad => b.cols(&[
            ("Mad(腹が立った)", "イライラしたこと", P),
            ("Sad(残念だった)", "がっかりしたこと", V),
            ("Glad(うれしかった)", "よかったこと", G),
        ]),
        FourLs => b.cols(&[
            ("Liked(良かった)", "好きだったこと", G),
            ("Learned(学んだ)", "新しく知ったこと", B),
            ("Lacked(足りなかった)", "不足していたもの", O),
            ("Longed for(欲しかった)", "あったらよかったもの", V),
        ]),
        Starfish => b.cols(&[
            ("もっとやる", "効果があるので増やす", G),
            ("続ける", "今のままでよい", B),
            ("始める", "まだやっていないこと", Y),
            ("減らす", "効果が薄いので控える", O),
            ("やめる", "役に立っていない", P),
        ]),
        Daki => b.cols(&[
            ("Drop(捨てる)", "やめてよいこと", P),
            ("Add(加える)", "新しく取り入れたいこと", B),
            ("Keep(続ける)", "続けたいこと", G),
            ("Improve(良くする)", "もっと良くできること", Y),
        ]),
        Kalm => b.cols(&[
            ("Keep(続ける)", "今のまま続けたい", G),
            ("Add(加える)", "新しく始めたい", B),
            ("More(増やす)", "もっとやりたい", Y),
            ("Less(減らす)", "少し控えたい", O),
        ]),
        PlusDelta => b.cols(&[
            ("+ プラス(良かった点)", "またやりたいこと", G),
            ("Δ デルタ(変えたい点)", "次はこう変える", O),
        ]),
        RoseBudThorn => b.cols(&[
            ("バラ(うまくいったこと)", "誇れること", P),
            ("つぼみ(伸びそうなこと)", "これからの芽", G),
            ("トゲ(痛かったこと)", "困ったこと", V),
        ]),
        LikedWishedWondered => b.cols(&[
            ("Liked(好きだった)", "良かったこと", G),
            ("Wished(こうだったら)", "願っていること", B),
            ("Wondered(疑問に思った)", "気になっていること", V),
        ]),
        Sailboat => {
            let unit = Vec2::new(360.0, 320.0);
            b.canvas(
                unit,
                &[
                    (
                        "追い風(助けになったもの)",
                        "前に進めてくれたこと",
                        G,
                        [0.0, 0.0, 1.0, 2.0],
                    ),
                    (
                        "錨(足を引っ張ったもの)",
                        "遅くした原因",
                        O,
                        [1.0, 1.0, 1.0, 1.0],
                    ),
                    (
                        "島(目指すゴール)",
                        "たどり着きたい姿",
                        B,
                        [2.0, 0.0, 1.0, 1.0],
                    ),
                    (
                        "岩(この先のリスク)",
                        "ぶつかりそうなこと",
                        P,
                        [2.0, 1.0, 1.0, 1.0],
                    ),
                ],
            );
            let w = unit.x + 48.0;
            b.shape(
                ShapeKind::Triangle,
                "チーム",
                Vec2::new(w + 80.0, 60.0),
                Vec2::new(unit.x - 160.0, 200.0),
                Rgba8::hex(0xe8f0fe),
            );
        }
        HotAirBalloon => b.canvas(
            Vec2::new(400.0, 320.0),
            &[
                (
                    "熱気(押し上げてくれたもの)",
                    "チームを上向かせたこと",
                    O,
                    [0.0, 0.0, 1.0, 1.0],
                ),
                (
                    "晴れ空(これからの追い風)",
                    "期待できること",
                    B,
                    [1.0, 0.0, 1.0, 1.0],
                ),
                (
                    "砂袋(重荷になっているもの)",
                    "手放したいこと",
                    V,
                    [0.0, 1.0, 1.0, 1.0],
                ),
                ("嵐(この先の心配)", "備えたいこと", P, [1.0, 1.0, 1.0, 1.0]),
            ],
        ),
        ThreeLittlePigs => b.cols(&[
            ("わらの家(すぐ壊れそう)", "危うい仕組み", Y),
            ("木の家(補強したい)", "もう少し強くしたい", O),
            ("レンガの家(しっかりしている)", "頼れる仕組み", G),
        ]),
        Timeline => b.table(
            "タイムライン",
            "",
            &["1 週目", "2 週目", "3 週目", "4 週目"],
            &["出来事", "良かったこと", "つらかったこと", "気分(1〜5)"],
            Vec2::new(240.0, 150.0),
        ),
        Unspoken => b.cols(&[
            ("部屋の中の象", "皆気づいているのに話題にしないこと", GRAY),
            ("死んだ魚", "引きずっている過去のこと", B),
            ("吐き出したいこと", "言えずにいた本音", O),
        ]),
        Esvp => b.cols(&[
            ("探検家", "新しいことを持ち帰りたい", G),
            ("買い物客", "役立つものがあれば拾いたい", B),
            ("休暇中", "通常業務から離れられてうれしい", Y),
            ("囚人", "正直、来たくなかった", GRAY),
        ]),
        Weather => b.cols(&[
            ("晴れ", "絶好調", Y),
            ("くもり", "まあまあ", GRAY),
            ("雨", "ちょっと疲れ気味", B),
            ("嵐", "かなり大変", V),
        ]),

        // ---- ワークショップ ----
        Icebreaker => b.columns(
            &[
                ("チェックイン: 今の気分をひとことで", Y, &["名前: ひとこと"]),
                ("最近うれしかったこと", O, &[]),
                ("今日期待していること", B, &[]),
            ],
            Vec2::new(400.0, 560.0),
        ),
        HopesConcerns => b.cols(&[
            ("期待(うまくいったらうれしいこと)", "実現したいこと", G),
            ("不安(心配なこと)", "気がかりなこと", P),
        ]),
        Brainstorm => {
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
                b.link(theme, note, PathStyle::Curved, Cap::None);
            }
        }
        Affinity => {
            b.frame("書き出す場所", Vec2::ZERO, Vec2::new(600.0, 800.0));
            for i in 0..6 {
                let p = Vec2::new(40.0 + (i % 3) as f32 * 184.0, 40.0 + (i / 3) as f32 * 184.0);
                b.sticky("事実・意見", p, Y);
            }
            for (i, name) in ["グループ A", "グループ B", "グループ C"]
                .iter()
                .enumerate()
            {
                b.frame(
                    name,
                    Vec2::new(680.0, i as f32 * 280.0),
                    Vec2::new(560.0, 232.0),
                );
            }
        }
        Crazy8s => {
            let cells: Vec<String> = (1..=8).map(|i| format!("案 {i}")).collect();
            let cells: Vec<Cell<'_>> = cells
                .iter()
                .enumerate()
                .map(|(i, t)| {
                    (
                        t.as_str(),
                        "",
                        Y,
                        [(i % 4) as f32, (i / 4) as f32, 1.0, 1.0],
                    )
                })
                .collect();
            b.canvas(Vec2::new(300.0, 300.0), &cells);
        }
        HowMightWe => b.cols(&[
            ("課題・困りごと", "ユーザーが困っていること", P),
            ("どうすれば〜できる?", "どうすれば〇〇を△△できるか?", B),
            ("アイデア", "問いへの答え", Y),
        ]),
        OneTwoFourAll => b.cols(&[
            ("1 人で(1 分)", "まず自分の考え", Y),
            ("2 人で(2 分)", "ペアで深める", O),
            ("4 人で(4 分)", "共通点と違い", B),
            ("全体で(5 分)", "いちばん響いた考え", G),
        ]),
        LeanCoffee => b.cols(&[
            ("話したいこと", "議題を 1 枚に 1 つ", Y),
            ("話している", "", B),
            ("話し終えた", "", G),
        ]),
        DecisionJam => b.cols(&[
            ("うまくいっていること", "良い点", G),
            ("問題", "困っていること", P),
            ("どうすれば〜?", "問題を問いに", B),
            ("解決策", "すぐ試せる案", Y),
            ("まずやること", "誰が・いつ", O),
        ]),
        SixHats => b.cols(&[
            ("白: 事実・情報", "わかっていること", WHITE),
            ("赤: 感情・直感", "好き嫌い・勘", P),
            ("黒: 慎重・リスク", "うまくいかない理由", GRAY),
            ("黄: 楽観・利点", "良くなる理由", Y),
            ("緑: 創造・新案", "別のやり方", G),
            ("青: 進行・まとめ", "次に考えること", B),
        ]),
        Pmi => b.cols(&[
            ("Plus(良い点)", "得られること", G),
            ("Minus(悪い点)", "失うこと", P),
            ("Interesting(興味深い点)", "どちらとも言えない発見", V),
        ]),
        FiveWhys => {
            let problem = b.sticky("問題", Vec2::new(0.0, 0.0), P);
            let mut prev = problem;
            for i in 1..=5 {
                let note = b.sticky(&format!("なぜ? {i}"), Vec2::new(i as f32 * 240.0, 0.0), Y);
                b.link(prev, note, PathStyle::Straight, Cap::Arrow);
                prev = note;
            }
            let cause = b.sticky("真因", Vec2::new(5.0 * 240.0, 240.0), O);
            b.link(prev, cause, PathStyle::Straight, Cap::Arrow);
            let fix = b.sticky("対策", Vec2::new(4.0 * 240.0, 240.0), G);
            b.link(cause, fix, PathStyle::Straight, Cap::Arrow);
        }
        Journey => b.table(
            "カスタマージャーニー",
            "段階",
            &["認知", "検討", "購入", "利用", "継続"],
            &["行動", "感情", "課題", "アイデア"],
            Vec2::new(200.0, 110.0),
        ),
        EmpathyMap => b.canvas(
            Vec2::new(400.0, 300.0),
            &[
                ("言っていること", "口にした言葉", Y, [0.0, 0.0, 1.0, 1.0]),
                ("考えていること", "頭の中の声", B, [1.0, 0.0, 1.0, 1.0]),
                ("していること", "行動・振る舞い", G, [0.0, 1.0, 1.0, 1.0]),
                ("感じていること", "気持ち", P, [1.0, 1.0, 1.0, 1.0]),
                ("悩み・痛み", "障害・不安", V, [0.0, 2.0, 1.0, 1.0]),
                ("得たいもの", "望む結果", O, [1.0, 2.0, 1.0, 1.0]),
            ],
        ),
        Persona => b.canvas(
            Vec2::new(340.0, 260.0),
            &[
                (
                    "名前・ひとことで",
                    "例: 忙しい共働きの親",
                    Y,
                    [0.0, 0.0, 1.0, 1.0],
                ),
                ("基本情報", "年齢・仕事・暮らし", B, [1.0, 0.0, 2.0, 1.0]),
                ("目標", "達成したいこと", G, [0.0, 1.0, 1.0, 1.0]),
                ("困りごと", "不満・不安", P, [1.0, 1.0, 1.0, 1.0]),
                (
                    "行動・習慣",
                    "よく使う道具・情報源",
                    O,
                    [2.0, 1.0, 1.0, 1.0],
                ),
            ],
        ),
        Grow => b.cols(&[
            ("Goal(目標)", "どうなりたい?", B),
            ("Reality(現状)", "今どうなっている?", Y),
            ("Options(選択肢)", "何ができる?", G),
            ("Will(やること)", "何をいつやる?", O),
        ]),
        Drucker => b.table(
            "ドラッカー風エクササイズ",
            "メンバー",
            &[
                "得意なこと",
                "どう貢献するか",
                "大切にしている価値",
                "期待されていると思うこと",
            ],
            &["メンバー A", "メンバー B", "メンバー C"],
            Vec2::new(240.0, 140.0),
        ),

        // ---- 戦略・企画 ----
        LeanCanvas => b.canvas(
            Vec2::new(260.0, 260.0),
            &[
                ("課題", "", Y, [0.0, 0.0, 1.0, 2.0]),
                ("ソリューション", "", Y, [1.0, 0.0, 1.0, 1.0]),
                ("主要指標", "", Y, [1.0, 1.0, 1.0, 1.0]),
                ("独自の価値提案", "", Y, [2.0, 0.0, 1.0, 2.0]),
                ("圧倒的な優位性", "", Y, [3.0, 0.0, 1.0, 1.0]),
                ("チャネル", "", Y, [3.0, 1.0, 1.0, 1.0]),
                ("顧客セグメント", "", Y, [4.0, 0.0, 1.0, 2.0]),
                ("コスト構造", "", Y, [0.0, 2.0, 2.5, 1.0]),
                ("収益の流れ", "", Y, [2.5, 2.0, 2.5, 1.0]),
            ],
        ),
        BusinessModelCanvas => b.canvas(
            Vec2::new(260.0, 260.0),
            &[
                ("パートナー", "", Y, [0.0, 0.0, 1.0, 2.0]),
                ("主要活動", "", Y, [1.0, 0.0, 1.0, 1.0]),
                ("リソース", "", Y, [1.0, 1.0, 1.0, 1.0]),
                ("価値提案", "", Y, [2.0, 0.0, 1.0, 2.0]),
                ("顧客との関係", "", Y, [3.0, 0.0, 1.0, 1.0]),
                ("チャネル", "", Y, [3.0, 1.0, 1.0, 1.0]),
                ("顧客セグメント", "", Y, [4.0, 0.0, 1.0, 2.0]),
                ("コスト構造", "", Y, [0.0, 2.0, 2.5, 1.0]),
                ("収益の流れ", "", Y, [2.5, 2.0, 2.5, 1.0]),
            ],
        ),
        Swot => b.canvas(
            Vec2::new(420.0, 340.0),
            &[
                ("強み(内部・プラス)", "得意なこと", G, [0.0, 0.0, 1.0, 1.0]),
                (
                    "弱み(内部・マイナス)",
                    "苦手なこと",
                    P,
                    [1.0, 0.0, 1.0, 1.0],
                ),
                (
                    "機会(外部・プラス)",
                    "追い風になる変化",
                    B,
                    [0.0, 1.0, 1.0, 1.0],
                ),
                (
                    "脅威(外部・マイナス)",
                    "向かい風になる変化",
                    O,
                    [1.0, 1.0, 1.0, 1.0],
                ),
            ],
        ),
        Pest => b.canvas(
            Vec2::new(420.0, 340.0),
            &[
                (
                    "政治(Politics)",
                    "法律・規制・政策",
                    V,
                    [0.0, 0.0, 1.0, 1.0],
                ),
                ("経済(Economy)", "景気・為替・物価", Y, [1.0, 0.0, 1.0, 1.0]),
                (
                    "社会(Society)",
                    "人口・価値観・流行",
                    P,
                    [0.0, 1.0, 1.0, 1.0],
                ),
                (
                    "技術(Technology)",
                    "新しい技術・普及",
                    B,
                    [1.0, 1.0, 1.0, 1.0],
                ),
            ],
        ),
        StakeholderMap => b.axes(
            "関係者マップ",
            "関心 →",
            "↑ 影響力",
            [
                ("満足を保つ(影響大・関心小)", O),
                ("密に連携する(影響大・関心大)", P),
                ("様子を見る(影響小・関心小)", GRAY),
                ("こまめに知らせる(影響小・関心大)", B),
            ],
        ),
        AsIsToBe => b.cols(&[
            ("現状(As-Is)", "今の姿", GRAY),
            ("理想(To-Be)", "ありたい姿", B),
            ("ギャップ", "埋めるべき差", O),
            ("打ち手", "差を埋める行動", G),
        ]),
        PreMortem => b.cols(&[
            (
                "失敗したとしたら、その理由は?",
                "1 年後に失敗していた。なぜ?",
                P,
            ),
            ("前兆として現れそうなこと", "早めに気づくサイン", O),
            ("今からできる予防策", "先に打てる手", G),
        ]),
        Okr => b.canvas(
            Vec2::new(360.0, 280.0),
            &[
                (
                    "目標(Objective)",
                    "わくわくする定性的な目標",
                    B,
                    [0.0, 0.0, 3.0, 1.0],
                ),
                ("主要な成果 1", "数値で測れる成果", Y, [0.0, 1.0, 1.0, 1.0]),
                ("主要な成果 2", "数値で測れる成果", Y, [1.0, 1.0, 1.0, 1.0]),
                ("主要な成果 3", "数値で測れる成果", Y, [2.0, 1.0, 1.0, 1.0]),
            ],
        ),

        // ---- 計画・進行 ----
        Kickoff => b.canvas(
            Vec2::new(360.0, 300.0),
            &[
                ("目的(なぜやるのか)", "", B, [0.0, 0.0, 1.0, 1.0]),
                ("成功の定義", "どうなったら成功?", G, [1.0, 0.0, 1.0, 1.0]),
                ("やらないこと", "範囲の外", GRAY, [2.0, 0.0, 1.0, 1.0]),
                ("体制・役割", "誰が何をするか", Y, [0.0, 1.0, 1.0, 1.0]),
                ("リスクと不安", "心配なこと", P, [1.0, 1.0, 1.0, 1.0]),
                ("マイルストーン", "いつまでに何を", O, [2.0, 1.0, 1.0, 1.0]),
            ],
        ),
        Roadmap => b.table(
            "ロードマップ",
            "テーマ",
            &["Q1", "Q2", "Q3", "Q4"],
            &["プロダクト", "技術", "組織"],
            Vec2::new(260.0, 200.0),
        ),
        StoryMap => {
            let acts = ["見つける", "試す", "使い続ける", "広める"];
            let w = acts.len() as f32 * 200.0;
            for (i, a) in acts.iter().enumerate() {
                let x = i as f32 * 200.0;
                b.sticky(a, Vec2::new(x, 0.0), B);
                b.sticky("タスク", Vec2::new(x, 200.0), Y);
            }
            b.frame(
                "リリース 1(最小限)",
                Vec2::new(-24.0, 420.0),
                Vec2::new(w + 8.0, 220.0),
            );
            b.frame(
                "リリース 2",
                Vec2::new(-24.0, 700.0),
                Vec2::new(w + 8.0, 220.0),
            );
        }
        SprintPlanning => b.canvas(
            Vec2::new(340.0, 240.0),
            &[
                (
                    "スプリントゴール",
                    "このスプリントで達成すること",
                    B,
                    [0.0, 0.0, 3.0, 1.0],
                ),
                (
                    "バックログ候補",
                    "候補のストーリー",
                    Y,
                    [0.0, 1.0, 1.0, 2.0],
                ),
                ("今回やること", "", G, [1.0, 1.0, 1.0, 2.0]),
                ("キャパシティ・休み", "使える時間", O, [2.0, 1.0, 1.0, 1.0]),
                (
                    "リスク・依存",
                    "待ちになりそうなこと",
                    P,
                    [2.0, 2.0, 1.0, 1.0],
                ),
            ],
        ),
        Moscow => b.cols(&[
            ("Must(必須)", "ないと成り立たない", P),
            ("Should(すべき)", "重要だが後でもよい", O),
            ("Could(できれば)", "あるとうれしい", B),
            ("Won't(今回はやらない)", "今回は見送る", GRAY),
        ]),
        PriorityMatrix => b.axes(
            "効果 × 手間",
            "手間 →",
            "↑ 効果",
            [
                ("すぐやる(効果大・手間小)", G),
                ("計画してやる(効果大・手間大)", B),
                ("余裕があれば(効果小・手間小)", Y),
                ("やらない(効果小・手間大)", GRAY),
            ],
        ),
        Matrix => b.axes(
            "重要度 × 緊急度",
            "緊急度 →",
            "↑ 重要度",
            [
                ("重要・急ぎでない", B),
                ("重要・急ぎ", P),
                ("重要でない・急ぎでない", G),
                ("重要でない・急ぎ", Y),
            ],
        ),
        Kanban => b.columns(
            &[
                ("ToDo", Y, &["タスク A", "タスク B"]),
                ("作業中", O, &["タスク C"]),
                ("完了", G, &["タスク D"]),
            ],
            Vec2::new(400.0, 640.0),
        ),
        Meeting => {
            b.columns(
                &[
                    ("議題", B, &["話したいこと"]),
                    ("決定事項", G, &[]),
                    ("宿題(誰が・いつまで)", O, &[]),
                ],
                Vec2::new(420.0, 520.0),
            );
            b.text(
                "日付: ____  参加者: ____",
                Vec2::new(0.0, -100.0),
                900.0,
                22.0,
            );
        }
    }
}

fn shape_bounds(elements: &[Element]) -> Rect {
    elements
        .iter()
        .filter(|e| !matches!(e, Element::Connector(_)))
        .map(|e| match e {
            // フレームの見出しは枠の上にはみ出す。
            Element::Shape(s) if s.kind == ShapeKind::Frame => s.bounds().union(&s.text_rect()),
            _ => e.bounds(),
        })
        .reduce(|a, b| a.union(&b))
        .unwrap_or(Rect::new(Vec2::ZERO, Vec2::ZERO))
}

/// 上に見出しと進め方を置く。
fn header(t: Template, b: &mut Builder<'_>) {
    let content = shape_bounds(&b.elements);
    let width = content.size().x.max(640.0);
    let mut lines = vec![format!("進め方  {}", t.description())];
    lines.extend(
        t.steps()
            .iter()
            .enumerate()
            .map(|(i, s)| format!("{}. {}", i + 1, s.describe())),
    );
    let steps = b.text(&lines.join("\n"), Vec2::ZERO, width, 16.0);
    let title = b.text(t.title(), Vec2::ZERO, width, 32.0);
    let h = |b: &Builder<'_>, id: ElementId| {
        b.elements
            .iter()
            .find(|e| e.id() == id)
            .map_or(0.0, |e| e.bounds().size().y)
    };
    let steps_h = h(b, steps);
    let title_h = h(b, title);
    let steps_y = content.min.y - 32.0 - steps_h;
    let title_y = steps_y - 8.0 - title_h;
    for e in &mut b.elements {
        let y = if e.id() == steps {
            steps_y
        } else if e.id() == title {
            title_y
        } else {
            continue;
        };
        let d = Vec2::new(content.min.x, y) - e.bounds().min;
        e.translate(d);
    }
}

/// テンプレートを中心 `center` に置く。置いた要素の ID を返す。
pub fn insert(board: &mut Board, t: Template, center: Vec2) -> Vec<ElementId> {
    let mut b = Builder {
        board,
        elements: Vec::new(),
    };
    build(t, &mut b);
    header(t, &mut b);
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
            assert!(ids.len() >= 4, "{t:?}");
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
            assert!(!t.steps().is_empty(), "{t:?}");
        }
    }

    #[test]
    fn names_are_unique_and_every_category_is_used() {
        let mut names: Vec<_> = Template::ALL.iter().map(|t| t.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), Template::ALL.len());
        for c in Category::ALL {
            assert!(
                Template::ALL.iter().filter(|t| t.category() == c).count() >= 5,
                "{c:?}"
            );
        }
    }

    #[test]
    fn frames_do_not_overlap() {
        for t in Template::ALL {
            let mut b = Board::new();
            let ids = insert(&mut b, t, Vec2::ZERO);
            let frames: Vec<Rect> = ids
                .iter()
                .filter_map(|id| b.get(*id)?.as_shape())
                .filter(|s| s.kind == ShapeKind::Frame)
                .map(|s| s.bounds().union(&s.text_rect()))
                .collect();
            for (i, a) in frames.iter().enumerate() {
                for c in &frames[i + 1..] {
                    let inter = a.min.max(c.min);
                    let max = a.max.min(c.max);
                    let outer_contains = (a.min.cmple(c.min).all() && a.max.cmpge(c.max).all())
                        || (c.min.cmple(a.min).all() && c.max.cmpge(a.max).all());
                    assert!(
                        outer_contains || inter.x >= max.x || inter.y >= max.y,
                        "{t:?}: {a:?} と {c:?} が重なる"
                    );
                }
            }
        }
    }

    #[test]
    fn tables_are_editable() {
        for (t, shape) in [
            (Template::Journey, (5, 6)),
            (Template::Timeline, (5, 5)),
            (Template::Roadmap, (4, 5)),
            (Template::Drucker, (4, 5)),
        ] {
            let mut b = Board::new();
            let ids = insert(&mut b, t, Vec2::ZERO);
            let g = crate::table::grid_of(&b, &ids).expect("表として編集できる");
            assert_eq!((g.rows(), g.cols()), shape, "{t:?}");
        }
    }
}
