//! AWS / Azure / Google Cloud の公式アイコンパック(ZIP)から、構成図の部品に使うアイコンを取り込む。
//!
//! 各社のアイコンは利用条件が独自のため、リポジトリには同梱しない。
//! 使う人が公式サイトから ZIP を入手し(`tools/fetch-cloud-icons.sh` で自動)、このツールで
//! `crates/board-web/assets/cloud-icons/` に 128px の PNG と `index.json` を作る。
//! Web クライアントは `index.json` があればアイコンを表示し、なければ色付きの図形で表す。
//!
//! ```sh
//! cargo run -p cloud-icons --release -- --aws Icon-package.zip --azure Azure_Public_Service_Icons.zip \
//!     --gcp core-products-icons.zip [--out crates/board-web/assets/cloud-icons]
//! ```

mod table;

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use table::Provider;

/// 書き出す PNG の一辺(画面で大きく表示しても粗くならない大きさ)。
const SIZE: u32 = 128;

fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// ZIP 内の 1 ファイル(アイコンの候補)。
#[derive(Clone, Debug)]
struct Entry {
    /// どの ZIP の何番目か。
    archive: usize,
    index: usize,
    path: String,
    /// 比較に使う名前(正規化済み)。
    key: String,
    svg: bool,
    dark: bool,
    size: u32,
}

fn file_stem(path: &str) -> (&str, bool) {
    let name = path.rsplit('/').next().unwrap_or(path);
    let lower = name.to_ascii_lowercase();
    let svg = lower.ends_with(".svg");
    let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name);
    (stem, svg)
}

/// AWS: `Arch_Amazon-EC2_64` / `Res_Users_48_Light` → (名前, サイズ, ダーク版か)
fn aws_key(stem: &str) -> (String, u32, bool) {
    let mut s = stem;
    let mut dark = false;
    for suffix in ["_Light", "_Dark", "_light", "_dark"] {
        if let Some(rest) = s.strip_suffix(suffix) {
            dark = suffix.eq_ignore_ascii_case("_dark");
            s = rest;
        }
    }
    let mut size = 0;
    if let Some((rest, n)) = s.rsplit_once('_')
        && let Ok(v) = n.parse::<u32>()
    {
        size = v;
        s = rest;
    }
    (normalize(s), size, dark)
}

/// Azure: `10021-icon-service-Virtual-Machine` → `virtualmachine`
fn azure_key(stem: &str) -> String {
    let s = stem.trim_start_matches(|c: char| c.is_ascii_digit());
    let s = s.trim_start_matches('-');
    let s = s.strip_prefix("icon-service-").unwrap_or(s);
    normalize(s)
}

fn entries_of(provider: &Provider, archive: usize, names: &[String]) -> Vec<Entry> {
    names
        .iter()
        .enumerate()
        .filter(|(_, p)| {
            let l = p.to_ascii_lowercase();
            (l.ends_with(".svg") || l.ends_with(".png")) && !l.contains("__macosx")
        })
        .map(|(index, path)| {
            let (stem, svg) = file_stem(path);
            let (key, size, dark) = match provider {
                Provider::Aws => aws_key(stem),
                Provider::Azure => (azure_key(stem), 0, false),
                Provider::Gcp => (normalize(path), 0, false),
            };
            Entry {
                archive,
                index,
                path: path.clone(),
                key,
                svg,
                dark,
                size,
            }
        })
        .collect()
}

/// 候補の中からいちばん良いファイルを選ぶ。
fn pick<'a>(provider: &Provider, entries: &'a [Entry], candidates: &[&str]) -> Option<&'a Entry> {
    for c in candidates {
        let c = normalize(c);
        let matches = entries.iter().filter(|e| match provider {
            Provider::Gcp => e.key.contains(&c),
            _ => e.key == c,
        });
        let score = |e: &Entry| {
            let mut s = 0i64;
            if e.svg {
                s += 10_000;
            }
            if !e.dark {
                s += 5_000;
            }
            s += i64::from(e.size.min(256)) * 10;
            if e.path.to_ascii_lowercase().contains("color") {
                s += 100;
            }
            s - e.path.len() as i64
        };
        if let Some(best) = matches.max_by_key(|e| score(e)) {
            return Some(best);
        }
    }
    None
}

/// SVG を一辺 `SIZE` の正方形の PNG にする(縦横比は保って中央に置く)。
fn render_svg(data: &[u8]) -> Result<Vec<u8>, String> {
    let opt = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(data, &opt).map_err(|e| e.to_string())?;
    let size = tree.size();
    let scale = (SIZE as f32 / size.width()).min(SIZE as f32 / size.height());
    let dx = (SIZE as f32 - size.width() * scale) * 0.5;
    let dy = (SIZE as f32 - size.height() * scale) * 0.5;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(SIZE, SIZE).ok_or("画像を作れません")?;
    let transform = resvg::tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, dx, dy);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    pixmap.encode_png().map_err(|e| e.to_string())
}

struct Archive {
    zip: zip::ZipArchive<File>,
    names: Vec<String>,
}

fn open(path: &Path) -> Result<Archive, String> {
    let file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let zip = zip::ZipArchive::new(file).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut zip = zip;
    let names = (0..zip.len())
        .map(|i| {
            zip.by_index(i)
                .ok()
                .and_then(|f| f.name().ok().map(|n| n.to_string()))
                .unwrap_or_default()
        })
        .collect();
    Ok(Archive { zip, names })
}

fn read(archive: &mut Archive, index: usize) -> Result<Vec<u8>, String> {
    let mut f = archive.zip.by_index(index).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).map_err(|e| e.to_string())?;
    Ok(buf)
}

struct Args {
    inputs: Vec<(Provider, PathBuf)>,
    out: PathBuf,
}

fn parse_args() -> Result<Args, String> {
    let mut inputs = Vec::new();
    let mut out = PathBuf::from("crates/board-web/assets/cloud-icons");
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        let mut value = || it.next().ok_or(format!("{a} の後にパスが必要です"));
        match a.as_str() {
            "--aws" => inputs.push((Provider::Aws, PathBuf::from(value()?))),
            "--azure" => inputs.push((Provider::Azure, PathBuf::from(value()?))),
            "--gcp" => inputs.push((Provider::Gcp, PathBuf::from(value()?))),
            "--out" => out = PathBuf::from(value()?),
            "-h" | "--help" => {
                return Err(
                    "使い方: cloud-icons [--aws ZIP] [--azure ZIP] [--gcp ZIP]... [--out DIR]"
                        .into(),
                );
            }
            other => return Err(format!("不明な引数: {other}")),
        }
    }
    if inputs.is_empty() {
        return Err("ZIP を 1 つ以上指定してください(--aws / --azure / --gcp)".into());
    }
    Ok(Args { inputs, out })
}

fn provider_name(p: &Provider) -> &'static str {
    match p {
        Provider::Aws => "aws",
        Provider::Azure => "azure",
        Provider::Gcp => "gcp",
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    std::fs::create_dir_all(&args.out).map_err(|e| e.to_string())?;

    let mut archives = Vec::new();
    let mut per_provider: Vec<(Provider, Vec<Entry>, String)> = Vec::new();
    for (provider, path) in args.inputs {
        let a = open(&path)?;
        let entries = entries_of(&provider, archives.len(), &a.names);
        println!(
            "{}: {} ({} 個の画像)",
            provider_name(&provider),
            path.display(),
            entries.len()
        );
        let source = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        archives.push(a);
        // 同じ提供元の ZIP が複数あればまとめる。
        match per_provider
            .iter_mut()
            .find(|(p, _, _)| provider_name(p) == provider_name(&provider))
        {
            Some((_, es, src)) => {
                es.extend(entries);
                src.push_str(&format!(", {source}"));
            }
            None => per_provider.push((provider, entries, source)),
        }
    }

    let mut nodes = Vec::new();
    let mut groups = Vec::new();
    let mut sources = BTreeMap::new();
    let mut missing = Vec::new();
    for (provider, entries, source) in &per_provider {
        sources.insert(provider_name(provider), source.clone());
        for (kind, candidates) in table::table(provider) {
            let Some(e) = pick(provider, entries, candidates) else {
                missing.push(*kind);
                continue;
            };
            let data = read(&mut archives[e.archive], e.index)?;
            let png = if e.svg {
                match render_svg(&data) {
                    Ok(p) => p,
                    Err(err) => {
                        eprintln!("  {kind}: {} を描けません: {err}", e.path);
                        missing.push(*kind);
                        continue;
                    }
                }
            } else {
                data
            };
            let (file, list) = match kind.strip_prefix("group:") {
                Some(g) => (format!("group_{g}.png"), &mut groups),
                None => (format!("{kind}.png"), &mut nodes),
            };
            std::fs::write(args.out.join(&file), png).map_err(|e| e.to_string())?;
            list.push(kind.trim_start_matches("group:").to_string());
        }
    }

    let index = serde_json::json!({
        "size": SIZE,
        "nodes": nodes,
        "groups": groups,
        "sources": sources,
        "note": "各社の公式アイコンです。利用条件は各社のサイトを確認してください。このディレクトリはリポジトリに含めません。",
    });
    std::fs::write(
        args.out.join("index.json"),
        serde_json::to_string_pretty(&index).unwrap_or_default(),
    )
    .map_err(|e| e.to_string())?;
    println!(
        "{} 個の部品と {} 個の枠のアイコンを {} に書き出しました",
        nodes.len(),
        groups.len(),
        args.out.display()
    );
    if !missing.is_empty() {
        println!(
            "見つからなかったもの(色付きの図形で表示されます): {}",
            missing.join(", ")
        );
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use board_core::diagram::catalog;

    #[test]
    fn table_refers_to_known_kinds() {
        for p in [Provider::Aws, Provider::Azure, Provider::Gcp] {
            for (kind, cands) in table::table(&p) {
                assert!(!cands.is_empty());
                match kind.strip_prefix("group:") {
                    Some(g) => assert_eq!(catalog::group_kind(g).id, g, "{g}"),
                    None => assert_eq!(catalog::node_kind(kind).id, *kind, "{kind}"),
                }
            }
        }
    }

    #[test]
    fn every_cloud_service_has_an_icon_rule() {
        let all: Vec<&str> = [table::AWS, table::AZURE, table::GCP]
            .iter()
            .flat_map(|t| t.iter().map(|(k, _)| *k))
            .collect();
        for k in catalog::NODE_KINDS {
            if k.style == catalog::NodeStyle::Service {
                assert!(all.contains(&k.id), "{} にアイコンの対応がない", k.id);
            }
        }
    }

    #[test]
    fn keys_strip_size_and_variant() {
        assert_eq!(
            aws_key("Arch_Amazon-EC2_64"),
            ("archamazonec2".into(), 64, false)
        );
        assert_eq!(aws_key("Res_Users_48_Dark"), ("resusers".into(), 48, true));
        assert_eq!(
            aws_key("Virtual-private-cloud-VPC_32").0,
            "virtualprivatecloudvpc"
        );
        assert_eq!(
            azure_key("10021-icon-service-Virtual-Machine"),
            "virtualmachine"
        );
        assert_eq!(azure_key("Virtual-Machine"), "virtualmachine");
    }

    #[test]
    fn picks_svg_light_and_largest() {
        let names: Vec<String> = [
            "Architecture-Service-Icons/Arch_Compute/32/Arch_Amazon-EC2_32.svg",
            "Architecture-Service-Icons/Arch_Compute/64/Arch_Amazon-EC2_64.png",
            "Architecture-Service-Icons/Arch_Compute/64/Arch_Amazon-EC2_64.svg",
            "Architecture-Service-Icons/Arch_Compute/64/Arch_Amazon-EC2-Auto-Scaling_64.svg",
            "Resource-Icons/Res_General-Icons/Res_48_Dark/Res_Users_48_Dark.svg",
            "Resource-Icons/Res_General-Icons/Res_48_Light/Res_Users_48_Light.svg",
            "__MACOSX/._Arch_Amazon-EC2_64.svg",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let es = entries_of(&Provider::Aws, 0, &names);
        let ec2 = pick(&Provider::Aws, &es, &["Arch_Amazon-EC2"]).unwrap();
        assert!(ec2.path.ends_with("64/Arch_Amazon-EC2_64.svg"));
        let users = pick(&Provider::Aws, &es, &["Res_Users"]).unwrap();
        assert!(users.path.contains("Light"));
        assert!(pick(&Provider::Aws, &es, &["Arch_AWS-Lambda"]).is_none());

        let gcp: Vec<String> = [
            "core/Compute Engine/SVG/ComputeEngine-512-color-rgb.svg",
            "core/Compute Engine/PNG/ComputeEngine-512-color-rgb.png",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let es = entries_of(&Provider::Gcp, 0, &gcp);
        assert!(pick(&Provider::Gcp, &es, &["ComputeEngine"]).unwrap().svg);
    }

    #[test]
    fn renders_svg_to_png() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="32"><rect width="64" height="32" fill="#ED7100"/></svg>"##;
        let png = render_svg(svg).unwrap();
        assert_eq!(&png[1..4], b"PNG");
        assert!(render_svg(b"not svg").is_err());
    }
}
