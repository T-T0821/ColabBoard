//! ボードの統計を出すプラグイン(読むだけ)。
//!
//! 要素の種類ごとの数と、付箋の色ごとの数を数える。結果はメッセージと `data` で返す。

use std::collections::BTreeMap;

use graboard_plugin::prelude::*;
use graboard_plugin::serde_json::json;

fn manifest() -> PluginManifest {
    PluginManifest::new("board-stats", "ボードの統計")
        .version("0.1.0")
        .description("要素の種類ごとの数と、付箋の色ごとの数を数えます。ボードは変えません。")
        .permission(Permission::BoardRead)
        .command("count", "要素を数える")
}

fn run(ctx: &mut Context) -> Result<RunOutput, String> {
    let all = ctx.elements()?;
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut colors: BTreeMap<String, usize> = BTreeMap::new();
    let mut chars = 0;
    for e in &all {
        *kinds.entry(e.kind_name().to_string()).or_default() += 1;
        if let ElementBody::Shape {
            kind, fill, text, ..
        } = &e.body
        {
            chars += text.chars().count();
            if kind == "sticky" {
                *colors.entry(fill.clone()).or_default() += 1;
            }
        }
    }
    let stickies = kinds.get("sticky").copied().unwrap_or(0);
    Ok(RunOutput {
        message: Some(format!(
            "要素 {} 個(付箋 {} 枚、{} 色)、文字 {} 文字",
            all.len(),
            stickies,
            colors.len(),
            chars
        )),
        data: Some(
            json!({"total": all.len(), "kinds": kinds, "sticky_colors": colors, "chars": chars}),
        ),
        ..Default::default()
    })
}

graboard_plugin::plugin!(manifest, run);

#[cfg(test)]
mod tests {
    use super::*;
    use graboard_plugin::board_core::{Board, ops};
    use graboard_plugin::testing;

    #[test]
    fn counts_without_writing() {
        let mut board = Board::new();
        ops::apply(
            &mut board,
            &[
                Op::Create {
                    element: NewElement::sticky(0.0, 0.0, "あいう"),
                },
                Op::Create {
                    element: NewElement::shape("rect", 0.0, 0.0, 10.0, 10.0),
                },
            ],
        )
        .unwrap();
        let m = manifest();
        let out = testing::run(&m, run, board, testing::input("count"));
        let r = out.result.unwrap();
        assert_eq!(r.data.unwrap()["total"], 2);
        assert!(r.message.unwrap().contains("文字 3 文字"));
    }

    #[test]
    fn cannot_write() {
        fn sneaky(ctx: &mut Context) -> Result<RunOutput, String> {
            ctx.apply(vec![Op::Delete { id: "1".into() }])?;
            Ok(RunOutput::default())
        }
        let out = testing::run(&manifest(), sneaky, Board::new(), testing::input("count"));
        assert!(out.result.unwrap_err().contains("権限"));
    }
}
