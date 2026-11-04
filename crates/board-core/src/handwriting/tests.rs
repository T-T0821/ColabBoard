use super::*;

/// テスト用の決まった乱数。
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }

    /// -a〜a。
    fn range(&mut self, a: f32) -> f32 {
        (self.next() * 2.0 - 1.0) * a
    }
}

fn glyph(c: char) -> Vec<Vec<Vec2>> {
    decode_glyphs(GLYPHS)
        .into_iter()
        .find(|(g, _)| *g == c)
        .map(|(_, s)| s)
        .unwrap_or_else(|| panic!("見本がありません: {c}"))
}

/// 見本の字形をゆがめて「手書き」にする。`text` を左から書いた線(ワールド座標、1 文字 = 40)。
fn handwrite(text: &str, rng: &mut Rng, mess: f32) -> Vec<Vec<Vec2>> {
    const EM: f32 = 40.0;
    let mut out = Vec::new();
    let mut x = 100.0;
    for c in text.chars() {
        if c == ' ' {
            x += EM * 0.6;
            continue;
        }
        let strokes = glyph(c);
        let (min, max) = {
            let refs: Vec<&[Vec2]> = strokes.iter().map(Vec::as_slice).collect();
            bounds(&refs).unwrap()
        };
        let scale = EM * (1.0 + rng.range(0.1 * mess));
        let rot = rng.range(0.08 * mess);
        let shear = rng.range(0.12 * mess);
        let (sin, cos) = rot.sin_cos();
        let cx = (min.x + max.x) / 2.0;
        let width = (max.x - min.x) * scale;
        for s in &strokes {
            let off = Vec2::new(rng.range(0.03 * mess), rng.range(0.03 * mess));
            let phase = rng.range(3.0);
            let pts: Vec<Vec2> = s
                .iter()
                .enumerate()
                .map(|(k, p)| {
                    let mut q = Vec2::new(p.x - cx, p.y - 0.5) + off;
                    q.x += shear * q.y;
                    q = Vec2::new(q.x * cos - q.y * sin, q.x * sin + q.y * cos);
                    let wobble = 0.012 * mess * ((k as f32) * 0.9 + phase).sin();
                    q += Vec2::new(rng.range(0.008 * mess) + wobble, rng.range(0.008 * mess));
                    Vec2::new(x + width / 2.0 + q.x * scale, 200.0 + q.y * scale)
                })
                .collect();
            out.push(pts);
        }
        x += width + EM * (0.12 + rng.range(0.05));
    }
    out
}

fn read(text: &str, seed: u64, mess: f32) -> String {
    let mut rng = Rng(seed);
    let strokes = handwrite(text, &mut rng, mess);
    let refs: Vec<&[Vec2]> = strokes.iter().map(Vec::as_slice).collect();
    Recognizer::builtin()
        .read_line(&refs)
        .map(|l| l.text())
        .unwrap_or_default()
}

/// 文字単位の一致率(編集距離から)。
fn accuracy(expected: &str, got: &str) -> f32 {
    let a: Vec<char> = expected.chars().collect();
    let b: Vec<char> = got.chars().collect();
    let mut d = (0..=b.len()).collect::<Vec<_>>();
    for i in 1..=a.len() {
        let mut prev = d[0];
        d[0] = i;
        for j in 1..=b.len() {
            let tmp = d[j];
            d[j] = (d[j] + 1)
                .min(d[j - 1] + 1)
                .min(prev + usize::from(a[i - 1] != b[j - 1]));
            prev = tmp;
        }
    }
    1.0 - d[b.len()] as f32 / a.len().max(1) as f32
}

#[test]
fn loads_builtin_glyphs() {
    let r = Recognizer::builtin();
    assert!(r.len() > 1200, "{}", r.len());
    for c in ['あ', 'ア', '日', 'A', '7', 'ー'] {
        assert!(r.protos.iter().any(|p| p.text == c.to_string()), "{c}");
    }
}

#[test]
fn clean_glyph_reads_as_itself() {
    let r = Recognizer::builtin();
    let mut wrong = Vec::new();
    for (c, strokes) in decode_glyphs(GLYPHS) {
        let refs: Vec<&[Vec2]> = strokes.iter().map(Vec::as_slice).collect();
        let top = r.classify(&refs, 0.0, 1);
        if top.first().map(|t| t.0.as_str()) != Some(c.to_string().as_str()) {
            wrong.push((c, top.first().map(|t| t.0.clone()).unwrap_or_default()));
        }
    }
    // 形が同じ文字(ロ/口、ー/一、o/O など)は区別できなくてよい。
    assert!(wrong.len() < 60, "{} 字: {wrong:?}", wrong.len());
}

#[test]
fn reads_handwritten_words() {
    let cases = [
        "ありがとう",
        "かいぎ",
        "テスト",
        "ミーティング",
        "日本",
        "会議室",
        "来週までに",
        "Hello",
        "2026",
        "OK",
    ];
    let mut total = 0.0;
    for (k, t) in cases.iter().enumerate() {
        let got = read(t, 7 + k as u64, 1.0);
        let acc = accuracy(t, &got);
        total += acc;
        eprintln!("{t} → {got} ({acc:.2})");
    }
    let mean = total / cases.len() as f32;
    assert!(mean > 0.8, "平均 {mean:.2}");
}

#[test]
fn splits_into_lines() {
    let mut rng = Rng(3);
    let mut a = handwrite("あい", &mut rng, 0.5);
    let b: Vec<Vec<Vec2>> = handwrite("うえ", &mut rng, 0.5)
        .into_iter()
        .map(|s| s.into_iter().map(|p| p + Vec2::new(0.0, 80.0)).collect())
        .collect();
    let n_a = a.len();
    a.extend(b);
    let refs: Vec<&[Vec2]> = a.iter().map(Vec::as_slice).collect();
    let rows = lines(&refs);
    assert_eq!(rows.len(), 2);
    assert!(rows[0].iter().all(|&i| i < n_a));
    assert!(rows[1].iter().all(|&i| i >= n_a));
}

#[test]
fn scribble_is_rejected() {
    // ぐるぐる書いた線は文字にしない。
    let mut rng = Rng(11);
    let pts: Vec<Vec2> = (0..200)
        .map(|i| {
            let t = i as f32 * 0.37;
            Vec2::new(
                100.0 + t * 3.0 + 30.0 * t.sin(),
                200.0 + 30.0 * (t * 1.7).cos() + rng.range(4.0),
            )
        })
        .collect();
    let stroke = Stroke {
        id: ElementId(1),
        points: pts,
        color: crate::color::Rgba8::hex(0),
        width: 2.0,
        highlighter: false,
        meta: Default::default(),
    };
    let items = recognize(&[&stroke], Recognizer::builtin());
    assert!(items.is_empty(), "{items:?}");
}

#[test]
fn recognize_returns_stroke_ids() {
    let mut rng = Rng(5);
    let strokes: Vec<Stroke> = handwrite("はい", &mut rng, 0.6)
        .into_iter()
        .enumerate()
        .map(|(i, points)| Stroke {
            id: ElementId(100 + i as u64),
            points,
            color: crate::color::Rgba8::hex(0),
            width: 2.0,
            highlighter: false,
            meta: Default::default(),
        })
        .collect();
    let refs: Vec<&Stroke> = strokes.iter().collect();
    let items = recognize(&refs, Recognizer::builtin());
    assert_eq!(items.len(), 1, "{items:?}");
    assert_eq!(items[0].text, "はい");
    let mut ids = items[0].strokes.clone();
    ids.sort_by_key(|i| i.0);
    assert_eq!(ids, strokes.iter().map(|s| s.id).collect::<Vec<_>>());
}

#[test]
fn teaching_learns_my_handwriting() {
    // 見本と大きく違う書き方の「あ」(縦に細長く、斜めに傾けた字)を覚えさせる。
    let mine = |seed: u64| -> Vec<Vec<Vec2>> {
        let mut rng = Rng(seed);
        handwrite("あ", &mut rng, 0.3)
            .into_iter()
            .map(|s| {
                s.into_iter()
                    .map(|p| Vec2::new(p.x * 0.5 + (p.y - 200.0) * 0.6, p.y * 1.4))
                    .collect()
            })
            .collect()
    };
    let base = Recognizer::builtin();
    let first = mine(1);
    let refs: Vec<&[Vec2]> = first.iter().map(Vec::as_slice).collect();
    let samples = base.teach(&refs, "あ").expect("区切れる");
    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0].text, "あ");
    // JSON で保存して読み戻せる。
    let json = serde_json::to_string(&samples).unwrap();
    let back: Vec<Sample> = serde_json::from_str(&json).unwrap();
    let learned = Recognizer::with_samples(&back);
    let again = mine(2);
    let refs: Vec<&[Vec2]> = again.iter().map(Vec::as_slice).collect();
    assert_eq!(learned.classify(&refs, 0.0, 1)[0].0, "あ");
}

#[test]
fn teaching_splits_by_expected_text() {
    let mut rng = Rng(9);
    let strokes = handwrite("いい", &mut rng, 0.5);
    let refs: Vec<&[Vec2]> = strokes.iter().map(Vec::as_slice).collect();
    let samples = Recognizer::builtin().teach(&refs, "いい").unwrap();
    assert_eq!(samples.len(), 2);
    assert!(samples.iter().all(|s| s.text == "い"));
    assert!(samples.iter().all(|s| s.strokes.len() == 2));
}

/// 精度の確認(`cargo test -p board-core handwriting::tests::evaluate -- --ignored --nocapture`)。
#[test]
#[ignore]
fn evaluate() {
    let words = [
        "ありがとうございます",
        "よろしくおねがいします",
        "かいぎのもくてき",
        "アイデアをだす",
        "スケジュール",
        "プロジェクト",
        "マーケティング",
        "来週の会議",
        "売上目標",
        "新しい商品",
        "東京駅",
        "今日の予定",
        "Hello World",
        "TODO",
        "2026/11/01",
        "API",
        "Design",
    ];
    for mess in [0.5, 1.0, 1.5] {
        let mut total = 0.0;
        let mut n = 0;
        for (k, w) in words.iter().enumerate() {
            for seed in 0..3u64 {
                let got = read(w, 1000 + seed * 31 + k as u64, mess);
                let acc = accuracy(w, &got);
                if seed == 0 {
                    eprintln!("[{mess}] {w} → {got} ({acc:.2})");
                }
                total += acc;
                n += 1;
            }
        }
        eprintln!("== 乱れ {mess}: 平均 {:.3}", total / n as f32);
    }
}

#[test]
fn ink_reads_text_next_to_shapes() {
    // 大きな四角の横に「OK」と書く。四角は図形、「OK」は文字になる(「O」を丸にしない)。
    let mut rng = Rng(21);
    let mut strokes: Vec<Stroke> = Vec::new();
    let push = |points: Vec<Vec2>, strokes: &mut Vec<Stroke>| {
        let id = ElementId(500 + strokes.len() as u64);
        strokes.push(Stroke {
            id,
            points,
            color: crate::color::Rgba8::hex(0),
            width: 2.0,
            highlighter: false,
            meta: Default::default(),
        });
    };
    let rect: Vec<Vec2> = [
        (0.0, 0.0),
        (300.0, 0.0),
        (300.0, 200.0),
        (0.0, 200.0),
        (0.0, 0.0),
    ]
    .windows(2)
    .flat_map(|w| {
        (0..20).map(move |k| {
            let t = k as f32 / 20.0;
            Vec2::new(
                w[0].0 + (w[1].0 - w[0].0) * t,
                400.0 + w[0].1 + (w[1].1 - w[0].1) * t,
            )
        })
    })
    .collect();
    push(rect, &mut strokes);
    for s in handwrite("OK", &mut rng, 0.5) {
        push(s, &mut strokes);
    }
    let refs: Vec<&Stroke> = strokes.iter().collect();
    let items = recognize_ink(&refs, Recognizer::builtin());
    let texts: Vec<&InkKind> = items
        .iter()
        .map(|i| &i.kind)
        .filter(|k| matches!(k, InkKind::Text(_)))
        .collect();
    assert_eq!(texts, [&InkKind::Text("OK".into())], "{items:?}");
    assert!(
        items
            .iter()
            .any(|i| matches!(i.kind, InkKind::Shape(_)) && i.strokes == [ElementId(500)])
    );
}

#[test]
fn far_apart_words_are_separate_items() {
    let mut rng = Rng(8);
    let mut strokes = handwrite("会議", &mut rng, 0.4);
    strokes.extend(
        handwrite("OK", &mut rng, 0.4)
            .into_iter()
            .map(|s| s.into_iter().map(|p| p + Vec2::new(300.0, 0.0)).collect()),
    );
    let strokes: Vec<Stroke> = strokes
        .into_iter()
        .enumerate()
        .map(|(i, points)| Stroke {
            id: ElementId(i as u64 + 1),
            points,
            color: crate::color::Rgba8::hex(0),
            width: 3.0,
            highlighter: false,
            meta: Default::default(),
        })
        .collect();
    let refs: Vec<&Stroke> = strokes.iter().collect();
    let mut texts: Vec<String> = recognize(&refs, Recognizer::builtin())
        .into_iter()
        .map(|t| t.text)
        .collect();
    texts.sort();
    assert_eq!(texts, ["OK", "会議"]);
}
