//! ColabBoard の再現可能なベンチマーク。
//!
//! ```sh
//! cargo run -p board-bench --release                         # 1 万 / 10 万 / 100 万要素
//! cargo run -p board-bench --release -- --sizes 100000 --json bench.json
//! ```
//!
//! ボードは `Board::demo`(決定的な乱数で並べた付箋・四角形・楕円)で作るので、
//! 同じマシンなら何度測っても同じ条件になる。各項目は複数回測って中央値を出す。
//! 「素朴」の列は全要素を順に調べる方法(空間インデックス導入前の実装)。

use std::fmt::Write as _;
use std::hint::black_box;
use std::time::{Duration, Instant};

use board_core::{Board, Camera, Element, Rect, Vec2};
use board_render::{ShapeInstance, VisibleBlocks, push_instances};

/// 依存を増やさないための小さな擬似乱数(SplitMix64)。
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn unit(&mut self) -> f32 {
        (self.next() >> 40) as f32 / (1u64 << 24) as f32
    }

    fn point_in(&mut self, r: &Rect) -> Vec2 {
        r.min + (r.max - r.min) * Vec2::new(self.unit(), self.unit())
    }
}

/// `f` を `runs` 回測って中央値を返す。
fn median(runs: usize, mut f: impl FnMut()) -> Duration {
    let mut v: Vec<Duration> = (0..runs.max(1))
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed()
        })
        .collect();
    v.sort();
    v[v.len() / 2]
}

/// 1 回あたりの平均時間(`iters` 回まとめて測る)。
fn per_iter(runs: usize, iters: usize, mut f: impl FnMut(usize)) -> Duration {
    median(runs, || {
        for i in 0..iters {
            f(i);
        }
    }) / iters as u32
}

struct Row {
    n: usize,
    name: &'static str,
    value: Duration,
    baseline: Option<Duration>,
    note: String,
}

fn fmt_dur(d: Duration) -> String {
    let us = d.as_secs_f64() * 1e6;
    if us < 1.0 {
        format!("{:.0} ns", us * 1000.0)
    } else if us < 1000.0 {
        format!("{us:.1} µs")
    } else if us < 1_000_000.0 {
        format!("{:.1} ms", us / 1000.0)
    } else {
        format!("{:.2} s", us / 1e6)
    }
}

fn fmt_count(n: usize) -> String {
    match n {
        n if n >= 1_000_000 && n % 1_000_000 == 0 => format!("{}M", n / 1_000_000),
        n if n >= 1000 && n % 1000 == 0 => format!("{}k", n / 1000),
        n => n.to_string(),
    }
}

/// 1920×1080 の画面を、ボード中央・等倍で見ているカメラ。
fn camera_at(center: Vec2, zoom: f32) -> Camera {
    let mut c = Camera::new(Vec2::new(1920.0, 1080.0));
    c.zoom = zoom;
    c.origin = center - c.viewport / zoom * 0.5;
    c
}

fn view_rect(c: &Camera) -> Rect {
    let (a, b) = c.visible_world_rect();
    Rect::new(a, b)
}

fn bench_size(n: usize, shared: bool, rows: &mut Vec<Row>) {
    let runs = if n >= 1_000_000 { 3 } else { 7 };
    let mut push = |name, value, baseline, note: String| {
        rows.push(Row {
            n,
            name,
            value,
            baseline,
            note,
        })
    };

    // ---- 作成 ----
    let mut board = None;
    let t = median(runs.min(3), || board = Some(black_box(Board::demo(n))));
    let board = board.unwrap();
    push("ボード作成(ダミー)", t, None, String::new());

    let extent = board.bounds().unwrap();
    let center = extent.center();
    let mut rng = Rng(0xC01AB);

    // ---- GPU へ送るインスタンス列の作成(ボードが変わったときの CPU 負荷) ----
    let mut staging: Vec<ShapeInstance> = Vec::with_capacity(n);
    let t = median(runs, || {
        staging.clear();
        for e in board.elements() {
            push_instances(e, &mut staging);
        }
        black_box(&staging);
    });
    push(
        "インスタンス列の作成",
        t,
        None,
        format!("{} MB", staging.len() * 40 / 1_000_000),
    );

    // ---- ブロック単位のカリング(描画命令を出す前に、画面外のブロックを捨てる) ----
    let mut blocks = VisibleBlocks::default();
    let t = median(runs, || blocks.rebuild(&staging));
    push("カリング用ブロックの作成", t, None, String::new());
    for (label, zoom) in [("等倍", 1.0f32), ("10%", 0.1)] {
        let cam = camera_at(center, zoom);
        let mut drawn = 0u64;
        let mut ranges = 0usize;
        let t = per_iter(runs, 200, |_| {
            let r = blocks.visible_ranges(&cam);
            drawn = r.iter().map(|r| (r.end - r.start) as u64).sum();
            ranges = r.len();
            black_box(r);
        });
        let name = if zoom == 1.0 {
            "毎フレームのカリング(等倍)"
        } else {
            "毎フレームのカリング(10%)"
        };
        push(
            name,
            t,
            None,
            format!(
                "{label}: GPU に渡す {} / {} インスタンス({ranges} 回の描画命令)",
                drawn,
                staging.len()
            ),
        );
    }

    // ---- ヒットテスト(ポインタ移動ごと) ----
    let points: Vec<Vec2> = (0..2000).map(|_| rng.point_in(&extent)).collect();
    let tol = 4.0;
    let fast = per_iter(runs, points.len(), |i| {
        black_box(board.hit_test(points[i], tol));
    });
    let slow_iters = if n >= 1_000_000 { 50 } else { 200 };
    let slow = per_iter(runs, slow_iters, |i| {
        black_box(
            board
                .elements()
                .iter()
                .rev()
                .find(|e| e.hit(points[i], tol)),
        );
    });
    push("ヒットテスト(1 回)", fast, Some(slow), String::new());

    // ---- 画面内の要素(文字の準備など、毎フレーム) ----
    let cam = camera_at(center, 1.0);
    let view = view_rect(&cam);
    let mut count = 0;
    let fast = per_iter(runs, 50, |_| {
        let v = board.query_visible(&view);
        count = v.len();
        black_box(v);
    });
    let slow = per_iter(runs, 5, |_| {
        let v: Vec<&Element> = board
            .elements()
            .iter()
            .filter(|e| e.bounds().intersects(&view))
            .collect();
        black_box(v);
    });
    push(
        "画面内の要素を集める(等倍)",
        fast,
        Some(slow),
        format!("{count} 個"),
    );

    // ---- 範囲選択 ----
    let sel = Rect::new(center - Vec2::splat(1500.0), center + Vec2::splat(1500.0));
    let fast = per_iter(runs, 50, |_| {
        black_box(board.query_inside(&sel));
    });
    let slow = per_iter(runs, 5, |_| {
        let v: Vec<_> = board
            .elements()
            .iter()
            .filter(|e| sel.contains_rect(&e.bounds()))
            .map(Element::id)
            .collect();
        black_box(v);
    });
    push("範囲選択(3000×3000)", fast, Some(slow), String::new());
    drop(board);

    if !shared {
        return;
    }

    // ---- 共同編集ボード(yrs): 初回同期の速さ ----
    let mut doc = None;
    let t = median(runs.min(3), || {
        doc = Some(black_box(Board::demo_shared(n, 1)))
    });
    let mut doc = doc.unwrap();
    push("共同編集ボード作成(yrs)", t, None, String::new());

    let mut state = Vec::new();
    let t = median(runs, || state = doc.encode_diff(None).unwrap());
    push(
        "全体のエンコード(サーバー → 参加者)",
        t,
        None,
        format!("{:.1} MB", state.len() as f64 / 1e6),
    );

    let t = median(runs.min(3), || {
        let mut b = Board::with_client_id(2);
        b.apply_remote(&state).unwrap();
        black_box(b);
    });
    push("初回同期の取り込み(参加者側)", t, None, String::new());

    // ---- ドラッグ中の 1 回の更新(yrs 書き込み + ビュー + 空間インデックス) ----
    let ids: Vec<_> = doc.elements().iter().map(Element::id).step_by(97).collect();
    let t = per_iter(runs, ids.len().min(500), |i| {
        doc.update(ids[i], |e| e.translate(Vec2::new(1.0, 0.5)));
        black_box(doc.take_outgoing());
    });
    push(
        "要素 1 個の移動(ドラッグ 1 フレーム)",
        t,
        None,
        String::new(),
    );
}

fn main() {
    let mut sizes = vec![10_000, 100_000, 1_000_000];
    let mut json_path = None;
    let mut shared_max = 100_000;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--sizes" => {
                sizes = args
                    .next()
                    .expect("--sizes 10000,100000")
                    .split(',')
                    .map(|s| s.trim().replace('_', "").parse().expect("数値"))
                    .collect();
            }
            "--json" => json_path = args.next(),
            "--shared-max" => {
                shared_max = args.next().and_then(|s| s.parse().ok()).expect("数値");
            }
            "-h" | "--help" => {
                println!(
                    "使い方: board-bench [--sizes 10000,100000,1000000] [--shared-max 100000] [--json out.json]"
                );
                return;
            }
            "--bench" => {} // `cargo bench` から渡される
            other => panic!("不明な引数: {other}"),
        }
    }
    if cfg!(debug_assertions) {
        eprintln!("警告: デバッグビルドです。`--release` を付けて測ってください。");
    }

    let mut rows = Vec::new();
    for &n in &sizes {
        eprintln!("{} 要素を計測中…", fmt_count(n));
        bench_size(n, n <= shared_max, &mut rows);
    }

    // Markdown の表。
    let mut md = String::new();
    let _ = writeln!(md, "| 要素数 | 項目 | 時間 | 素朴な方法 | 速さ | 備考 |");
    let _ = writeln!(md, "| ---: | --- | ---: | ---: | ---: | --- |");
    for r in &rows {
        let (base, ratio) = match r.baseline {
            Some(b) => (
                fmt_dur(b),
                format!("{:.0}×", b.as_secs_f64() / r.value.as_secs_f64().max(1e-12)),
            ),
            None => (String::new(), String::new()),
        };
        let _ = writeln!(
            md,
            "| {} | {} | {} | {} | {} | {} |",
            fmt_count(r.n),
            r.name,
            fmt_dur(r.value),
            base,
            ratio,
            r.note
        );
    }
    print!("{md}");

    if let Some(path) = json_path {
        let mut js = String::from("[\n");
        for (i, r) in rows.iter().enumerate() {
            let _ = writeln!(
                js,
                "  {{\"n\": {}, \"name\": {:?}, \"ns\": {}, \"baseline_ns\": {}, \"note\": {:?}}}{}",
                r.n,
                r.name,
                r.value.as_nanos(),
                r.baseline
                    .map_or("null".to_string(), |b| b.as_nanos().to_string()),
                r.note,
                if i + 1 == rows.len() { "" } else { "," }
            );
        }
        js.push(']');
        std::fs::write(&path, js).expect("JSON を書けません");
        eprintln!("{path} に保存しました");
    }
}
