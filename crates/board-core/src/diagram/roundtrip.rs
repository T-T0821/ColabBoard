//! 取り込み ⇄ 書き出しの往復で図が崩れないことの確認。

use glam::Vec2;

use super::{Diagram, extract, mermaid, plantuml};
use crate::board::Board;
use crate::templates::{self, Template};

#[derive(Clone, Copy, Debug)]
enum Format {
    Mermaid,
    PlantUml,
}

const FORMATS: [Format; 2] = [Format::Mermaid, Format::PlantUml];

fn write(f: Format, d: &Diagram) -> String {
    match f {
        Format::Mermaid => mermaid::write(d),
        Format::PlantUml => plantuml::write(d),
    }
}

fn parse(f: Format, text: &str) -> Diagram {
    match f {
        Format::Mermaid => mermaid::parse_diagram(text),
        Format::PlantUml => plantuml::parse_diagram(text),
    }
    .unwrap_or_else(|e| panic!("{f:?} を読めません: {e}\n{text}"))
}

/// 図をボードに置いて、もう一度読み取る。
fn through_board(d: &Diagram) -> Diagram {
    let mut board = Board::new();
    let els = d.build(Vec2::ZERO, &mut || board.alloc_id());
    board.push_many(els);
    extract::extract(&board, &[]).unwrap()
}

/// 文字 → ボード → 文字 → ボード → 文字 で同じ文字になる(ID は名前から付け直す)。
fn assert_text_round_trip(f: Format, text: &str) -> String {
    let first = write(f, &through_board(&parse(f, text)));
    let again = write(f, &through_board(&parse(f, &first)));
    assert_eq!(first, again, "{f:?} の往復で変わりました");
    first
}

#[test]
fn templates_survive_board_text_board() {
    for t in [
        Template::UmlClass,
        Template::Er,
        Template::Sequence,
        Template::UseCase,
        Template::StateMachine,
        Template::Activity,
    ] {
        let mut board = Board::new();
        templates::insert(&mut board, t, Vec2::ZERO);
        let d = extract::extract(&board, &[]).unwrap();
        for f in FORMATS {
            let text = write(f, &d);
            let back = through_board(&parse(f, &text));
            assert_eq!(write(f, &back), text, "{t:?} を {f:?} で往復");
        }
    }
}

#[test]
fn mermaid_flowchart_round_trip() {
    let text = assert_text_round_trip(
        Format::Mermaid,
        "flowchart LR
    A([開始]) --> B{ログイン済み?}
    B -->|はい| C[ダッシュボード]
    B -- いいえ --> D[(ユーザーDB)]
    subgraph auth [認証]
        E(\"ログイン<br>画面\")
    end
    D -.-> E
    C --- F>メモ]
    E <--> C",
    );
    assert!(text.contains("subgraph"));
    assert!(text.contains("-.->"));
    assert!(text.contains("---"));
    assert!(text.contains("<-->"));
    assert!(text.contains("ログイン<br>画面"));
}

#[test]
fn mermaid_class_er_sequence_round_trip() {
    let class = assert_text_round_trip(
        Format::Mermaid,
        "classDiagram
    class Animal {
        <<interface>>
        +move() void
    }
    class Duck {
        -name: String
        +swim() void
    }
    Animal <|.. Duck : 実装
    Pond o-- Duck",
    );
    assert!(class.contains("<<interface>>"));
    assert!(class.contains("..|>"));
    assert!(class.contains("o--"));
    let er = assert_text_round_trip(
        Format::Mermaid,
        "erDiagram
    CUSTOMER {
        int id PK
        string name
    }
    ORDER {
        int id PK
        int customer_id FK
    }
    CUSTOMER ||--o{ ORDER : places",
    );
    assert!(er.contains("||--o{"));
    assert!(er.contains("int customer_id FK"));
    let seq = assert_text_round_trip(
        Format::Mermaid,
        "sequenceDiagram
    participant U as 利用者
    U->>API: ログイン
    API->>API: 照合
    API-->>U: トークン",
    );
    assert!(seq.contains("-->>"));
    assert!(seq.contains("API->>API: 照合"));
}

#[test]
fn plantuml_round_trip() {
    let comp = assert_text_round_trip(
        Format::PlantUml,
        "@startuml
actor 利用者 as U
package \"Web\" {
  [フロント] as F
  component API
}
database \"顧客DB\" as DB
hexagon \"認証済み?\" as Q
note \"メモ\" as N
U -> F : 操作
F --> Q
Q --> API
API ..> DB : SQL
N .. API
@enduml",
    );
    assert!(comp.contains("actor \"利用者\""));
    assert!(comp.contains("database \"顧客DB\""));
    assert!(comp.contains("hexagon"));
    assert!(comp.contains("note \"メモ\""));
    assert!(comp.contains("..>"));
    let class = assert_text_round_trip(
        Format::PlantUml,
        "@startuml
class \"注文\" as O {
  - id: int
  --
  + total(): int
}
entity users {
  PK  id : int
  name : text
}
O *-- Line : 1..*
users ||--o{ O
@enduml",
    );
    assert!(class.contains("allowmixing") || !class.contains("rectangle"));
    assert!(class.contains("*--"));
    assert!(class.contains("||--o{"));
    let seq = assert_text_round_trip(
        Format::PlantUml,
        "@startuml
participant \"画面\" as S
S -> API : POST /login
API --> S : トークン
@enduml",
    );
    assert!(seq.contains("API --> p1 : トークン"));
}

#[test]
fn mermaid_and_plantuml_convert_into_each_other() {
    // Mermaid で読んだ図を PlantUML に書き出し、読み直しても同じ図になる。
    let m = mermaid::parse_diagram(
        "classDiagram
    class A {
        -x: int
        +f() void
    }
    A <|-- B
    A *-- C : 部品",
    )
    .unwrap();
    let via = plantuml::parse_diagram(&plantuml::write(&m)).unwrap();
    assert_eq!(mermaid::write(&via), mermaid::write(&m));
}

#[test]
fn plain_names_become_ids() {
    let text = assert_text_round_trip(
        Format::PlantUml,
        "@startuml\nrectangle Web\nnote \"Memo\" as M\nM .. Web\n@enduml",
    );
    assert!(text.contains("rectangle Web\n"));
    assert!(text.contains("note \"Memo\" as Memo\n"));
}
