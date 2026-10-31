//! 付箋を色ごとの列に並べるプラグイン。
//!
//! 選択中の付箋(なければボードの全付箋)を、色ごとに 1 列ずつ並べ直す。
//!
//! ```sh
//! cargo build -p graboard-plugin --example tidy_by_color --target wasm32-unknown-unknown --release
//! # → target/wasm32-unknown-unknown/release/examples/tidy_by_color.wasm
//! ```

use std::collections::BTreeMap;

use graboard_plugin::prelude::*;

const GAP: f32 = 24.0;

fn manifest() -> PluginManifest {
    PluginManifest::new("tidy-by-color", "色ごとに並べる")
        .version("0.1.0")
        .description("付箋を色ごとの列に並べ直します。選択中の付箋があればそれだけ、なければ全部が対象です。")
        .permission(Permission::BoardWrite)
        .command("tidy", "付箋を色ごとに並べる")
}

fn run(ctx: &mut Context) -> Result<RunOutput, String> {
    let mut notes = ctx.selected()?;
    notes.retain(|e| e.kind_name() == "sticky");
    if notes.is_empty() {
        notes = ctx.query(&ElementQuery {
            kind: Some("sticky".into()),
            ..Default::default()
        })?;
    }
    notes.retain(|e| !e.locked);
    if notes.is_empty() {
        return Ok(RunOutput::message("並べる付箋がありません"));
    }

    // 左上と、色ごとの組(色の初出順を保つため、色ごとに最初に出てきた位置で並べる)。
    let (mut x0, mut y0) = (f32::MAX, f32::MAX);
    let mut groups: BTreeMap<String, Vec<&ElementData>> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    for e in &notes {
        if let ElementBody::Shape { x, y, fill, .. } = &e.body {
            x0 = x0.min(*x);
            y0 = y0.min(*y);
            if !groups.contains_key(fill) {
                order.push(fill.clone());
            }
            groups.entry(fill.clone()).or_default().push(e);
        }
    }

    let mut ops = Vec::new();
    let mut x = x0;
    for color in &order {
        let mut y = y0;
        let mut col_w: f32 = 0.0;
        let mut col = groups[color].clone();
        // 列の中は元の上から順。
        col.sort_by(|a, b| {
            pos(a)
                .1
                .total_cmp(&pos(b).1)
                .then(pos(a).0.total_cmp(&pos(b).0))
        });
        for e in col {
            let (_, _, w, h) = pos(e);
            ops.push(Op::Update {
                id: e.id.clone(),
                set: ElementPatch {
                    x: Some(x),
                    y: Some(y),
                    ..Default::default()
                },
            });
            y += h + GAP;
            col_w = col_w.max(w);
        }
        x += col_w + GAP * 2.0;
    }
    let ids: Vec<String> = notes.iter().map(|e| e.id.clone()).collect();
    ctx.apply(ops)?;
    ctx.log(format!("{} 枚 / {} 色", notes.len(), order.len()));
    Ok(RunOutput::message(format!(
        "{} 枚の付箋を {} 色に並べました",
        notes.len(),
        order.len()
    ))
    .select(ids))
}

fn pos(e: &ElementData) -> (f32, f32, f32, f32) {
    match &e.body {
        ElementBody::Shape { x, y, w, h, .. } => (*x, *y, *w, *h),
        _ => (0.0, 0.0, 0.0, 0.0),
    }
}

graboard_plugin::plugin!(manifest, run);

#[cfg(test)]
mod tests {
    use super::*;
    use graboard_plugin::board_core::Board;
    use graboard_plugin::board_core::ops;
    use graboard_plugin::testing;

    #[test]
    fn groups_by_color() {
        let mut board = Board::new();
        let mk = |x: f32, y: f32, fill: &str| Op::Create {
            element: NewElement::Shape {
                kind: Some("sticky".into()),
                x,
                y,
                w: None,
                h: None,
                style: ShapeStyle {
                    fill: Some(fill.into()),
                    ..Default::default()
                },
                meta: Default::default(),
            },
        };
        ops::apply(
            &mut board,
            &[
                mk(500.0, 900.0, "#fff3a3"),
                mk(0.0, 0.0, "#b5e3ff"),
                mk(900.0, 100.0, "#fff3a3"),
                mk(300.0, 300.0, "#b5e3ff"),
            ],
        )
        .unwrap();
        let m = manifest();
        m.validate().unwrap();
        let out = testing::run(&m, run, board, testing::input("tidy"));
        let r = out.result.unwrap();
        assert!(r.message.unwrap().contains("4 枚の付箋を 2 色"));
        let all = ops::query(&out.board, &ElementQuery::default());
        let xs: std::collections::BTreeSet<i32> = all
            .iter()
            .map(|e| match e.body {
                ElementBody::Shape { x, .. } => x as i32,
                _ => -1,
            })
            .collect();
        // 2 列になる。
        assert_eq!(xs.len(), 2);
        assert_eq!(out.logs.len(), 1);
    }
}
