//! ビルドしたクライアントを、配信用にあらかじめ圧縮しておく。
//!
//! `precompress <dir>` で、`dir` の下の圧縮が効くファイル(.wasm .js .css .html .svg .json .ttf …)
//! それぞれに `.br`(brotli 最高圧縮)と `.gz`(gzip)を並べて書き出す。サーバーはブラウザが
//! 受け取れる形式を選んでそのまま返す(リクエストのたびに圧縮しない)。
//! `trunk build --release` の後に自動で走る(`crates/board-web/Trunk.toml` のフック)。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// 圧縮する拡張子。PNG・JPEG・woff2 などは元から圧縮済みなので除く。
const EXTENSIONS: &[&str] = &[
    "wasm", "js", "mjs", "css", "html", "svg", "json", "ttf", "otf", "txt", "md",
];
/// これより小さいファイルは圧縮しても得がない。
const MIN_BYTES: u64 = 1024;

fn main() {
    let Some(dir) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("使い方: precompress <ディレクトリ>");
        std::process::exit(2);
    };
    let mut files = Vec::new();
    collect(&dir, &mut files);
    let started = Instant::now();
    let (mut before, mut after) = (0u64, 0u64);
    // 大きなファイル(wasm)ほど時間がかかるので、ファイルごとにスレッドを分ける。
    let results: Vec<_> = std::thread::scope(|s| {
        let handles: Vec<_> = files
            .iter()
            .map(|path| {
                s.spawn(move || compress(path).map_err(|e| format!("{}: {e}", path.display())))
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    let mut failed = false;
    for r in results {
        match r {
            Ok((raw, br)) => {
                before += raw;
                after += br;
            }
            Err(e) => {
                eprintln!("圧縮できません: {e}");
                failed = true;
            }
        }
    }
    println!(
        "precompress: {} ファイル {:.1} MB → brotli {:.1} MB({:.1} 秒)",
        files.len(),
        before as f64 / 1e6,
        after as f64 / 1e6,
        started.elapsed().as_secs_f64()
    );
    if failed {
        std::process::exit(1);
    }
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            collect(&path, out);
        } else if meta.len() >= MIN_BYTES
            && path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| EXTENSIONS.contains(&e))
        {
            out.push(path);
        }
    }
}

/// `path.br` と `path.gz` を書く。戻り値は(元の大きさ, brotli の大きさ)。
fn compress(path: &Path) -> std::io::Result<(u64, u64)> {
    let raw = std::fs::read(path)?;
    let mut br = Vec::new();
    {
        let params = brotli::enc::BrotliEncoderParams {
            quality: 11,
            lgwin: 24,
            ..Default::default()
        };
        brotli::BrotliCompress(&mut raw.as_slice(), &mut br, &params)?;
    }
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    gz.write_all(&raw)?;
    let gz = gz.finish()?;
    let with_ext = |ext: &str| {
        let mut p = path.as_os_str().to_owned();
        p.push(ext);
        PathBuf::from(p)
    };
    std::fs::write(with_ext(".br"), &br)?;
    std::fs::write(with_ext(".gz"), &gz)?;
    Ok((raw.len() as u64, br.len() as u64))
}
