//! 朝会用のボードを作り、昨日・今日・困りごとの列に付箋を並べる例。
//!
//! ```sh
//! GRABOARD_URL=http://localhost:8080 GRABOARD_TOKEN=gbt_… cargo run -p graboard-sdk --example standup
//! ```

use graboard_sdk::{Batch, Client, NewElement, ShapeStyle};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::from_env()?;
    let me = client.me().await?;
    let info = client
        .create_board(&format!("{} さんの朝会", me.name))
        .await?;
    let board = client.board(&info.id);

    let columns = [
        ("昨日やったこと", "#c4f0c2", ["API を追加", "レビュー"]),
        ("今日やること", "#b5e3ff", ["SDK を書く", "ドキュメント"]),
        ("困っていること", "#ffb8c9", ["特になし", "会議が多い"]),
    ];
    let mut batch = Batch::new();
    for (i, (title, color, notes)) in columns.iter().enumerate() {
        let x = i as f32 * 320.0;
        batch.create(NewElement::Shape {
            kind: Some("frame".into()),
            x: x - 40.0,
            y: -80.0,
            w: Some(300.0),
            h: Some(560.0),
            style: ShapeStyle {
                text: Some(title.to_string()),
                ..Default::default()
            },
            meta: Default::default(),
        });
        for (j, note) in notes.iter().enumerate() {
            let mut sticky = NewElement::sticky(x - 20.0, j as f32 * 240.0, *note);
            if let NewElement::Shape { style, .. } = &mut sticky {
                style.fill = Some(color.to_string());
            }
            batch.create(sticky);
        }
    }
    let r = board.apply(batch).await?;
    println!(
        "ボード {} に {} 個の要素を置きました",
        info.id,
        r.created.len()
    );
    Ok(())
}
