//! `graboard`: ボードをコマンドラインで作る・変換する・書き出す。
//!
//! Web クライアントやサーバーと同じ Rust のコア(`board-core`)でファイルを読み書きし、
//! GPU なしの描画(`board-export`)で PNG / PDF にする。サーバーのボードを書き出したり、
//! ファイルを読み込ませたりもできる(`pull` / `push`)。

mod args;

use std::io::{Read, Write};
use std::path::Path;
use std::process::ExitCode;

use args::Args;
use board_core::Board;
use board_core::convert::{self, InputFormat, TextFormat};
use board_export::{FontArc, Format, Options, Target};

/// 標準出力に書く(`| head` などで先に閉じられても落ちない)。
macro_rules! say {
    ($($t:tt)*) => {{
        let _ = writeln!(std::io::stdout(), $($t)*);
    }};
}

fn say_raw(s: &str) {
    let _ = std::io::stdout().write_all(s.as_bytes());
}

const HELP: &str = "\
graboard — グラボードのボードを作る・変換する・書き出す

使い方:
  graboard new [テンプレート] -o 出力          テンプレート(省略で空)から新しいボード
  graboard convert 入力... -o 出力             読み込んで別の形式に(複数なら横に並べる)
  graboard info 入力                           中身のあらまし
  graboard templates                           テンプレートの一覧
  graboard formats                             読み書きできる形式
  graboard pull ボードID -o 出力               サーバーのボードを書き出す(サーバーで描く)
  graboard push 入力... (--board ID | --title 題名)
                                               ファイルをサーバーのボードに読み込む

出力の形式は拡張子で決まる(.graboard / .png / .pdf / .json / .md)。`-o -` は標準出力(--to が必要)。
入力の `-` は標準入力。

書き出しの設定:
  --to 形式            出力の形式(拡張子より優先)
  --from 形式          入力の形式(省略すると拡張子と中身から推測)
  --scale 数           1 単位を何画素にするか(既定 2)
  --padding 数         余白(既定 48)
  --background 色      背景色(#rrggbb)か transparent(既定 白)
  --frame 名前         フレーム 1 つだけ(名前か番号)
  --frames             フレームを 1 つ 1 ページの PDF に(スライドの順)
  --font パス          文字に使うフォント(既定: 同梱の Noto Sans JP)
  --title 題名         PDF の文書名
  --hide-private       個人作業エリアの中身を伏せる

サーバー(pull / push):
  --server URL         サーバーの URL(既定: 環境変数 GRABOARD_URL)
  --token トークン     API トークン(既定: 環境変数 GRABOARD_TOKEN)
  --key 鍵             共有リンクの鍵

例:
  graboard new kpt -o 振り返り.png
  graboard convert flow.mmd -o flow.pdf
  graboard convert 議事録.md 売上.xlsx -o まとめ.graboard
  graboard convert 企画.graboard --frames -o 企画.pdf
  graboard pull 3f2a9c --frames -o 企画.pdf
  graboard push 図.drawio --board 3f2a9c
";

fn main() -> ExitCode {
    let args = match Args::parse(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("graboard: {e}\n`graboard --help` で使い方を表示します");
            return ExitCode::from(2);
        }
    };
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("graboard: {e}");
            ExitCode::FAILURE
        }
    }
}

type Result<T> = std::result::Result<T, String>;

fn run(a: Args) -> Result<()> {
    if a.flag("help") || a.command.is_none() {
        say_raw(HELP);
        return Ok(());
    }
    if a.flag("version") {
        say!("graboard {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    match a.command.as_deref().unwrap_or_default() {
        "help" => {
            say_raw(HELP);
            Ok(())
        }
        "new" => cmd_new(&a),
        "convert" | "render" | "export" => cmd_convert(&a),
        "info" => cmd_info(&a),
        "templates" => cmd_templates(),
        "formats" => cmd_formats(),
        "pull" => cmd_pull(&a),
        "push" => cmd_push(&a),
        other => Err(format!("知らないコマンドです: {other}")),
    }
}

// ---- 出力 ----

/// 出力先の形式。
enum Output {
    Image(Format),
    Text(TextFormat),
}

impl Output {
    fn from_name(s: &str) -> Option<Self> {
        Format::from_name(s)
            .map(Output::Image)
            .or_else(|| TextFormat::from_name(s).map(Output::Text))
    }

    fn name(&self) -> &'static str {
        match self {
            Output::Image(f) => f.extension(),
            Output::Text(f) => f.extension(),
        }
    }
}

fn extension(path: &str) -> Option<&str> {
    Path::new(path).extension().and_then(|e| e.to_str())
}

/// `-o` と `--to` から出力先と形式を決める。
fn output(a: &Args) -> Result<(String, Output)> {
    let path = a
        .value("output")
        .ok_or("出力先を -o で指定してください")?
        .to_string();
    let name = match a.value("to") {
        Some(t) => t.to_string(),
        None if path == "-" => {
            return Err("標準出力に書くときは --to で形式を指定してください".into());
        }
        None => extension(&path)
            .ok_or_else(|| {
                format!("出力の形式が分かりません(拡張子か --to を付けてください): {path}")
            })?
            .to_string(),
    };
    let out = Output::from_name(&name)
        .ok_or_else(|| format!("書き出せない形式です: {name}(png / pdf / graboard / json / md)"))?;
    Ok((path, out))
}

fn write_file(path: &str, bytes: &[u8]) -> Result<()> {
    if path == "-" {
        std::io::stdout()
            .write_all(bytes)
            .map_err(|e| format!("書き出せません: {e}"))
    } else {
        std::fs::write(path, bytes).map_err(|e| format!("{path} に書けません: {e}"))
    }
}

fn render_options(a: &Args) -> Result<Options> {
    let mut o = Options::default();
    if let Some(s) = a.value("scale") {
        o.scale = s
            .parse::<f32>()
            .ok()
            .filter(|v| *v > 0.0 && *v <= 16.0)
            .ok_or("--scale は 0〜16 の数にしてください")?;
    }
    if let Some(p) = a.value("padding") {
        o.padding = p
            .parse::<f32>()
            .ok()
            .filter(|v| *v >= 0.0)
            .ok_or("--padding は 0 以上の数にしてください")?;
    }
    match a.value("background") {
        None => {}
        Some("transparent" | "none") => o.background = None,
        Some(c) => {
            o.background = Some(
                board_core::ops::parse_color(c).ok_or_else(|| format!("色が読めません: {c}"))?,
            );
        }
    }
    o.hide_private = a.flag("hide-private");
    Ok(o)
}

fn target(a: &Args) -> Target {
    if a.flag("frames") {
        Target::Frames
    } else if let Some(f) = a.value("frame") {
        Target::Frame(f.to_string())
    } else {
        Target::Board
    }
}

#[cfg(feature = "bundled-font")]
fn bundled_font() -> Option<FontArc> {
    static BYTES: &[u8] = include_bytes!("../../board-web/assets/fonts/NotoSansJP-Regular.ttf");
    FontArc::try_from_slice(BYTES).ok()
}

#[cfg(not(feature = "bundled-font"))]
fn bundled_font() -> Option<FontArc> {
    None
}

fn font(a: &Args) -> Result<Option<FontArc>> {
    if let Some(p) = a.value("font") {
        return board_export::load_font(Path::new(p)).map(Some);
    }
    if let Some(p) = std::env::var_os("GRABOARD_FONT") {
        return board_export::load_font(Path::new(&p)).map(Some);
    }
    let found = bundled_font().or_else(|| board_export::find_font(&[]));
    if found.is_none() {
        eprintln!(
            "graboard: 日本語のフォントが見つからないので、文字を線で表します(--font で指定できます)"
        );
    }
    Ok(found)
}

/// ボードを `path` に書き出す。
fn save(a: &Args, board: &Board, path: &str, out: &Output, title: &str) -> Result<()> {
    let bytes = match out {
        Output::Text(f) => {
            let bytes = convert::save(board, *f)?;
            if *f == TextFormat::Markdown && bytes.iter().all(u8::is_ascii_whitespace) {
                return Err("マークダウンにする文字がありません".into());
            }
            bytes
        }
        Output::Image(f) => {
            let title = a.value("title").unwrap_or(title);
            board_export::export(
                board,
                *f,
                &target(a),
                &render_options(a)?,
                font(a)?.as_ref(),
                title,
            )
            .map_err(|e| e.to_string())?
        }
    };
    write_file(path, &bytes)?;
    if path != "-" {
        eprintln!(
            "{path} に書き出しました({}、{})",
            out.name(),
            size(bytes.len())
        );
    }
    Ok(())
}

fn size(n: usize) -> String {
    match n {
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.0} KB", n as f64 / (1 << 10) as f64),
        n => format!("{n} バイト"),
    }
}

// ---- 入力 ----

fn read_input(path: &str) -> Result<Vec<u8>> {
    if path == "-" {
        let mut buf = Vec::new();
        std::io::stdin()
            .read_to_end(&mut buf)
            .map_err(|e| format!("標準入力を読めません: {e}"))?;
        Ok(buf)
    } else {
        std::fs::read(path).map_err(|e| format!("{path} を読めません: {e}"))
    }
}

fn input_format(a: &Args, path: &str, bytes: &[u8]) -> Result<InputFormat> {
    if let Some(f) = a.value("from") {
        return InputFormat::from_name(f).ok_or_else(|| format!("読めない形式です: {f}"));
    }
    let name = (path != "-").then_some(path);
    InputFormat::detect(name, bytes)
        .or_else(|| {
            board_core::convert::load(bytes)
                .ok()
                .map(|_| InputFormat::Graboard)
        })
        .ok_or_else(|| format!("{path} の形式が分かりません(--from で指定してください)"))
}

/// 入力をすべて読んで 1 枚のボードにする。
fn load_inputs(a: &Args) -> Result<Board> {
    if a.positional.is_empty() {
        return Err("入力のファイルを指定してください".into());
    }
    let mut board = Board::new();
    for path in &a.positional {
        let bytes = read_input(path)?;
        let format = input_format(a, path, &bytes)?;
        let ids =
            convert::import(&mut board, format, &bytes).map_err(|e| format!("{path}: {e}"))?;
        eprintln!(
            "{path}: {}として {} 個の要素を読み込みました",
            format.name(),
            ids.len()
        );
    }
    Ok(board)
}

/// 出力の題名(PDF の文書名): 最初の入力のファイル名。
fn stem(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| *s != "-")
        .unwrap_or("graboard")
        .to_string()
}

// ---- コマンド ----

fn cmd_new(a: &Args) -> Result<()> {
    let (path, out) = output(a)?;
    let (board, title) = match a.positional.first() {
        Some(name) => {
            let t = convert::find_template(name).ok_or_else(|| {
                format!("テンプレート「{name}」がありません(`graboard templates` で一覧)")
            })?;
            (convert::from_template(t), t.title().to_string())
        }
        None => {
            if matches!(out, Output::Image(_)) {
                return Err("空のボードは画像にできません。テンプレートを指定してください".into());
            }
            (Board::new(), String::new())
        }
    };
    save(a, &board, &path, &out, &title)
}

fn cmd_convert(a: &Args) -> Result<()> {
    let (path, out) = output(a)?;
    let board = load_inputs(a)?;
    save(a, &board, &path, &out, &stem(&a.positional[0]))
}

fn cmd_info(a: &Args) -> Result<()> {
    let board = load_inputs(a)?;
    let s = convert::summary(&board);
    say!("要素: {}", s.elements);
    for (kind, n) in &s.kinds {
        say!("  {kind}: {n}");
    }
    if s.images > 0 {
        say!("画像: {}", s.images);
    }
    if let Some(b) = s.bounds {
        say!(
            "範囲: ({:.0}, {:.0}) 〜 ({:.0}, {:.0})  {:.0} × {:.0}",
            b.min.x,
            b.min.y,
            b.max.x,
            b.max.y,
            b.size().x,
            b.size().y
        );
    }
    if !s.frames.is_empty() {
        say!("フレーム(スライドの順):");
        for (i, f) in s.frames.iter().enumerate() {
            say!("  {}. {}", i + 1, if f.is_empty() { "(無題)" } else { f });
        }
    }
    Ok(())
}

fn cmd_templates() -> Result<()> {
    use board_core::templates::{Category, Template};
    for c in Category::ALL {
        say!("{}", c.title());
        for t in Template::ALL.into_iter().filter(|t| t.category() == c) {
            say!("  {:<22} {}  — {}", t.name(), t.title(), t.description());
        }
    }
    Ok(())
}

fn cmd_formats() -> Result<()> {
    say!("読み込み(--from):");
    for (f, exts) in [
        (InputFormat::Graboard, ".graboard(画像も含めて欠けずに保存)"),
        (
            InputFormat::Json,
            ".json(要素の一覧か REST API と同じ {\"ops\": [...]})",
        ),
        (InputFormat::Markdown, ".md .txt"),
        (InputFormat::Mermaid, ".mmd .mermaid"),
        (InputFormat::PlantUml, ".puml .plantuml .pu"),
        (InputFormat::DrawIo, ".drawio .dio .xml"),
        (InputFormat::Miro, "Miro の書き出し(.json)"),
        (InputFormat::Csv, ".csv .tsv(Shift_JIS も可)"),
        (InputFormat::Spreadsheet, ".xlsx .xls .ods"),
    ] {
        say!("  {:<10} {exts}", f.name());
    }
    say!("書き出し(-o の拡張子か --to):");
    say!("  graboard   .graboard");
    say!("  png        画像");
    say!("  pdf        PDF(--frames で 1 フレーム 1 ページ)");
    say!("  json       要素の一覧(画像の中身は含まない)");
    say!("  md         マークダウン(文字の内容)");
    Ok(())
}

// ---- サーバー ----

fn client(a: &Args) -> Result<graboard_sdk::Client> {
    let url = a
        .value("server")
        .map(str::to_string)
        .or_else(|| std::env::var("GRABOARD_URL").ok())
        .ok_or("サーバーの URL を --server か環境変数 GRABOARD_URL で指定してください")?;
    let token = a
        .value("token")
        .map(str::to_string)
        .or_else(|| std::env::var("GRABOARD_TOKEN").ok());
    match token {
        Some(t) => graboard_sdk::Client::new(&url, t),
        None => graboard_sdk::Client::anonymous(&url),
    }
    .map_err(|e| e.to_string())
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())
}

fn cmd_pull(a: &Args) -> Result<()> {
    let id = a
        .positional
        .first()
        .ok_or("書き出すボードの ID を指定してください")?;
    let (path, out) = output(a)?;
    let client = client(a)?;
    let mut board = client.board(id);
    if let Some(k) = a.value("key") {
        board = board.with_key(k);
    }
    let mut q: Vec<(&str, String)> = Vec::new();
    for key in ["scale", "padding", "background", "frame"] {
        if let Some(v) = a.value(key) {
            q.push((key, v.to_string()));
        }
    }
    if a.flag("frames") {
        q.push(("pages", "frames".into()));
    }
    let bytes = runtime()?
        .block_on(board.export(out.name(), &q))
        .map_err(|e| e.to_string())?;
    write_file(&path, &bytes)?;
    if path != "-" {
        eprintln!(
            "{path} に書き出しました({}、{})",
            out.name(),
            size(bytes.len())
        );
    }
    Ok(())
}

fn cmd_push(a: &Args) -> Result<()> {
    if a.positional.is_empty() {
        return Err("読み込ませるファイルを指定してください".into());
    }
    let client = client(a)?;
    let rt = runtime()?;
    let id = match (a.value("board"), a.value("title")) {
        (Some(id), _) => id.to_string(),
        (None, Some(title)) => {
            let info = rt
                .block_on(client.create_board(title))
                .map_err(|e| e.to_string())?;
            eprintln!("ボード「{}」を作りました(ID: {})", info.title, info.id);
            info.id
        }
        (None, None) => {
            return Err("--board(既存のボード)か --title(新しいボード)を指定してください".into());
        }
    };
    let mut board = client.board(&id);
    if let Some(k) = a.value("key") {
        board = board.with_key(k);
    }
    for path in &a.positional {
        let bytes = read_input(path)?;
        let format = input_format(a, path, &bytes)?;
        let created = rt
            .block_on(board.import(Some(format.name()), bytes))
            .map_err(|e| format!("{path}: {e}"))?;
        eprintln!(
            "{path}: {} 個の要素をボード {id} に読み込みました",
            created.len()
        );
    }
    say!("{id}");
    Ok(())
}
