# プラグイン(WASM)

グラボードには WebAssembly のプラグインで機能を足せる。プラグインはサーバーで動き、
ボードの要素を読んだり、作る・変える・消すことができる。たとえば次のようなことに使う。

- 付箋を色や文字で並べ替える・まとめる
- ボードの内容を数える・チェックする(重複した付箋、空の付箋など)
- テンプレートや図を組み立てて置く
- 付箋の文字を一括で置き換える

付属の例:

| プラグイン | 権限 | 内容 |
| --- | --- | --- |
| `tidy-by-color`(色ごとに並べる) | 読む・編集 | 選択中の付箋(なければ全部)を色ごとの列に並べ直す |
| `board-stats`(ボードの統計) | 読む | 要素の種類ごとの数と付箋の色ごとの数を返す |
| `github-issues`(GitHub の課題を付箋に) | 読む・編集・通信(`api.github.com`)・保存 | 付箋に書いた `owner/repo` の開いている課題を付箋にする。最後に使ったリポジトリを覚える |

## 安全のための仕組み

- 実行は [wasmi](https://github.com/wasmi-labs/wasmi)(Rust 製の WebAssembly インタープリター)。
  プラグインに渡すのは `graboard.call` と `graboard.response` の 2 つの関数だけで、
  ファイル・ネットワーク・時計・乱数(WASI)はない。ほかの関数を読み込むモジュールは入れられない。
- 命令数(燃料、既定 20 億)とメモリ(既定 64 MB)に上限がある。無限ループや使いすぎは止める。
- ホスト関数の呼び出しは 1 回の実行で 1 万回まで、1 回のやり取りは 16 MB まで。
- 権限はマニフェストで宣言する。宣言していないことはできない(下の「権限のサンドボックス」)。
- `board:write` のプラグインは、実行する人がそのボードの編集者以上のときだけ実行できる。
- 書き込みは使い捨てのボードに溜め、**実行が最後まで成功したときだけ**、CRDT の更新として
  そのボードを開いている全員に配る。途中で止まったりエラーを返したりしたら何も変わらない。
  1 回の `apply` も、1 つでも失敗した操作があれば何も変えない。
- ユーザーが入れたプラグインは本人しか使えない。全員に使わせたいものはサーバーの管理者が `PLUGIN_DIR` に置く。

## 権限のサンドボックス

プラグインが使える権限は 4 つ。実際に使えるのは
**「マニフェストで宣言したもの」∩「実行する人が許可したもの」∩「サーバーの管理者が認めたもの」** だけ。

| 権限 | 名前 | できること | 許可 |
| --- | --- | --- | --- |
| ボードを読む | `board:read` | 要素を読む | 既定で許可済み(取り消せる) |
| ボードを編集する | `board:write` | 要素を作る・変える・消す(実行する人が編集者以上のときだけ) | 既定で許可済み(取り消せる) |
| 外部と通信する | `network` | マニフェストの `hosts` に書いた先にだけ HTTP で問い合わせる | **初めて実行する前に確認する** |
| データを保存する | `storage` | プラグイン専用の保存領域(実行する人ごと)を読み書きする | **初めて実行する前に確認する** |

- 通信と保存を求めるプラグインは、実行する人が許可するかどうかを決めるまで実行できない
  (`403`)。画面では「プラグイン」のパネルに確認の欄が出る。権限ごと・接続先ごとに選べ、
  許可しなかったものはプラグインから使えない(呼ぶとエラーが返る)。
- 新しい版で権限や接続先が増えたら、増えた分だけまた確認する。
- 決めたあとも「権限の設定」から変えられる。「最初に戻す」で確認前の状態に戻る。
- ログインしていない人は許可できないので、通信・保存を求めるプラグインは実行できない。

### 通信(`network`)

- 通信はプラグインではなくサーバーが代わりに行う。プラグインには、ソケットも DNS もない。
- 接続先はマニフェストの `hosts` に書く(`api.example.com`、`*.example.com`、ポートを付けるなら
  `example.com:8443`。最大 16 個)。ここに当てはまらない URL には、許可があっても送らない。
- `https://` だけ。リダイレクトはたどらない(3xx をそのまま返す)。
- サーバーの `HTTPS_PROXY` などのプロキシは使わず、直接つなぐ(プロキシの先で内部のアドレスに向けられないように)。
- 内部のアドレス(`127.0.0.1`、`10.0.0.0/8`、`192.168.0.0/16`、`169.254.169.254` などのメタデータ、
  `fc00::/7` など)には接続しない。名前を引いた結果が内部のアドレスでも断る。
- 1 回の実行で 20 回まで、1 回 10 秒・合計 30 秒まで。送る本文は 1 MB、受け取る本文は 4 MB まで。
  `host`・`content-length`・`connection` など接続の仕組みに関わるヘッダーは付けられない。
- どこに接続したかは実行結果の `fetches` に残り、画面にも出る。

### 保存領域(`storage`)

- キーと JSON の値の組を、実行する人 × プラグインごとに保存する(1 MB・1000 件まで)。
- 書き込みはボードと同じく、**実行が最後まで成功したときだけ** 残る。
- 保存の許可を取り消すと、保存したデータも消える。プラグインを消したときも消える。

### サーバーの管理者の設定

| 環境変数 | 既定 | 内容 |
| --- | --- | --- |
| `PLUGIN_NETWORK` | `1` | `0` で通信を全部止める(`network` を求めるプラグインは通信なしで動く) |
| `PLUGIN_NETWORK_HOSTS` | なし | 接続してよい先の上限(`api.github.com,*.example.com`)。これに含まれない宣言は確認もせず使えない |
| `PLUGIN_NETWORK_INSECURE` | `0` | `1` で `http://` と内部のアドレスも許す(手元での開発用) |
| `PLUGIN_STORAGE` | `1` | `0` で保存領域を使えなくする |

## 入れ方と実行のしかた

### サーバーのプラグイン(管理者)

`PLUGIN_DIR` に `.wasm` を置いて起動する。全員が使える。

```sh
PLUGIN_DIR=/srv/graboard/plugins board-server
```

各自がプラグインを入れるのを禁止するには `USER_PLUGINS=0`。

### 画面から

- 実行: ボードの右上の「プラグイン」を押すと、使えるプラグインとコマンドが並ぶ。押すと選択中の要素(なければボード全体)に実行し、結果のメッセージを出す。
- 入れる・消す: ボード一覧の下の「開発者向け: API トークンとプラグイン」で `.wasm` を選ぶ。

### 自分のプラグイン(API から)

API トークン(ボードを限らない `write`)かログインで、`.wasm` をそのまま送る。同じ ID なら置き換わる。

```sh
curl -H "Authorization: Bearer $TOKEN" -H 'content-type: application/wasm' \
  --data-binary @tidy_by_color.wasm $URL/api/v1/plugins
```

| メソッド | パス | 内容 |
| --- | --- | --- |
| `GET` | `/api/v1/plugins` | 使えるプラグイン(サーバーのもの + 自分のもの)。マニフェスト、`source`(`server` / `user`)、自分の許可 `grant`、まだ決めていないもの `pending`、サーバーの設定で使えないもの `denied` |
| `POST` | `/api/v1/plugins` | 入れる(本文は `.wasm`、8 MB まで)。1 人 30 個まで |
| `DELETE` | `/api/v1/plugins/{id}` | 自分のプラグインを消す(許可と保存したデータも消える) |
| `PUT` | `/api/v1/plugins/{id}/grant` | 許可を決める(`{"allowed": ["board:read", "network"], "hosts": ["api.github.com"]}`)。書かなかった権限・接続先は許可しない |
| `DELETE` | `/api/v1/plugins/{id}/grant` | 許可を確認前に戻す |
| `DELETE` | `/api/v1/plugins/{id}/storage` | 保存領域を空にする |
| `POST` | `/api/v1/boards/{board}/plugins/{id}/run` | 実行する |

実行のリクエストと結果:

```json
{"command": "tidy", "args": {}, "selection": ["5f3a00000001", "5f3a00000002"]}
```

```json
{
  "output": {"message": "6 枚の付箋を 3 色に並べました", "select": ["…"], "data": null},
  "ops": 6,
  "logs": ["6 枚 / 3 色"],
  "fuel_used": 3763489,
  "fetches": [{"method": "GET", "url": "https://api.github.com/repos/octo/demo/issues", "status": 200}]
}
```

失敗したときは 400 と `{"error": "プラグインの実行に失敗しました: 命令数の上限を超えました…"}` のような理由を返す。

## Rust で書く

`graboard-plugin` クレートを使う。

```toml
[package]
name = "hello"
version = "0.1.0"
edition = "2024"

[lib]
crate-type = ["cdylib"]

[dependencies]
graboard-plugin = { git = "https://github.com/T-T0821/ColabBoard" }

[profile.release]
opt-level = "s"
lto = true
```

```rust
use graboard_plugin::prelude::*;

fn manifest() -> PluginManifest {
    PluginManifest::new("hello", "あいさつ")
        .version("0.1.0")
        .description("実行した人の名前で付箋を置きます")
        .permission(Permission::BoardWrite) // board:read も付く
        .command("hello", "あいさつの付箋を置く")
}

fn run(ctx: &mut Context) -> Result<RunOutput, String> {
    let name = ctx.user().map(|u| u.name.clone()).unwrap_or_else(|| "だれか".into());
    let r = ctx.apply(vec![Op::Create {
        element: NewElement::sticky(0.0, 0.0, format!("{name} さん、こんにちは")),
    }])?;
    Ok(RunOutput::message("付箋を置きました").select(r.created))
}

graboard_plugin::plugin!(manifest, run);
```

```sh
rustup target add wasm32-unknown-unknown
cargo build --target wasm32-unknown-unknown --release
# → target/wasm32-unknown-unknown/release/hello.wasm
```

### `Context` でできること

| メソッド | 権限 | 内容 |
| --- | --- | --- |
| `command()` / `args()` / `selection()` | - | 実行されたコマンド、呼び出し側が渡した引数、選択中の要素の ID |
| `board()` / `user()` / `can_write()` | - | ボードの ID と名前、実行した人、書き込めるか |
| `elements()` / `query(&ElementQuery)` / `selected()` / `get(id)` | 読む | 要素を読む([要素の形](api.md#要素の形)は REST API と同じ) |
| `apply(Vec<Op>)` | 編集 | 作る・変える・消す([一括操作](api.md#一括操作)と同じ。`$0` で同じ呼び出しの新しい要素を参照できる) |
| `log(msg)` | - | ログに残す(実行結果の `logs` に入る。200 行まで) |
| `can(Permission)` / `hosts()` | - | この実行で使える権限か(宣言していても許可されていなければ `false`)、接続してよい先 |
| `fetch(FetchRequest)` / `get_json(url)` | 通信 | HTTP で問い合わせる(`FetchRequest::get(url).header("accept", "…")`、`FetchRequest::post_json(url, &json)`) |
| `load(key)` / `save(key, &value)` / `remove(key)` / `keys()` | 保存 | 保存領域を読み書きする(値は serde で JSON にできるもの) |

通信するプラグインは、マニフェストに接続先を書く(`network` の権限も付く)。

```rust
PluginManifest::new("github-issues", "GitHub の課題を付箋に")
    .permission(Permission::BoardWrite)
    .permission(Permission::Storage)
    .host("api.github.com")
```

`RunOutput` の `message` は実行した人に見せる短い文、`select` は実行後に選択してほしい要素、`data` は REST API の呼び出し側に返す任意の JSON。

### テスト

WASM 以外のターゲットでは、ホスト関数の代わりに手元の `Board` を使う擬似ホストで動くので、普段の `cargo test` でそのまま試せる。

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use graboard_plugin::board_core::Board;
    use graboard_plugin::testing;

    #[test]
    fn puts_a_sticky() {
        let out = testing::run(&manifest(), run, Board::new(), testing::input("hello"));
        assert!(out.result.is_ok());
        assert_eq!(out.board.len(), 1);
    }
}
```

通信や保存を使うプラグインは `testing::run_with` で、通信の代わりの関数・最初の保存領域・
許可しない権限を渡して試す。接続先の確かめ方はサーバーと同じ。

```rust
let out = testing::run_with(
    &manifest(), run, board, input,
    testing::Env::default()
        .fetch(|req| Ok(FetchResponse { status: 200, body: "[]".into(), ..Default::default() }))
        .storage("repo", json!("octo/demo"))
        .deny(Permission::Network), // 通信を許可しなかったとき
);
assert!(out.fetches.is_empty());
```

例は `crates/board-plugin/examples/`(`tidy_by_color.rs`、`board_stats.rs`、`github_issues.rs`)。

```sh
cargo test -p graboard-plugin
cargo build -p graboard-plugin --examples --target wasm32-unknown-unknown --release
# → target/wasm32-unknown-unknown/release/examples/tidy_by_color.wasm など
```

## ほかの言語で書く(ABI)

WebAssembly にできる言語なら何でもよい。やり取りはすべて UTF-8 の JSON。型の定義は
`crates/board-types/src/plugin.rs` と `crates/board-types/src/ops.rs`。

プラグインが出すもの:

| 名前 | 型 | 内容 |
| --- | --- | --- |
| `memory` | メモリ | 線形メモリ |
| `gb_alloc` | `(len: i32) -> i32` | `len` バイトの領域を確保して先頭のアドレスを返す(ホストが入力を書き込む) |
| `gb_manifest` | `() -> i64` | マニフェストの JSON の場所(`ptr << 32 \| len`) |
| `gb_run` | `(ptr: i32, len: i32) -> i64` | 入力(`RunInput`)を受け取り、`{"ok": RunOutput}` か `{"err": "理由"}` の場所を返す |

ホストが渡すもの(モジュール名 `graboard`):

| 名前 | 型 | 内容 |
| --- | --- | --- |
| `call` | `(ptr: i32, len: i32) -> i32` | 呼び出し(`HostCall`)の JSON を送る。返り値は応答の長さ |
| `response` | `(dst: i32)` | 直前の `call` の応答(`{"ok": …}` か `{"err": "…"}`)を `dst` に書く |

マニフェスト:

```json
{
  "id": "tidy-by-color",
  "name": "色ごとに並べる",
  "version": "0.1.0",
  "description": "…",
  "permissions": ["board:read", "board:write"],
  "hosts": [],
  "commands": [{"id": "tidy", "title": "付箋を色ごとに並べる"}],
  "abi": 1
}
```

`permissions` に書けるのは `board:read`・`board:write`・`network`・`storage`。`network` を書くときは
`hosts` も書く。

入力(`RunInput`):

```json
{"command": "tidy", "args": {}, "board": {"id": "abc", "title": "振り返り"},
 "user": {"login": "alice", "name": "Alice"}, "selection": ["5f3a00000001"], "can_write": true,
 "permissions": ["board:read", "board:write"], "hosts": []}
```

呼び出し(`HostCall`):

```json
{"fn": "query", "query": {"kind": "sticky", "bbox": [0, 0, 1000, 800]}}
{"fn": "get", "id": "5f3a00000001"}
{"fn": "apply", "ops": [{"op": "update", "id": "5f3a00000001", "set": {"x": 100}}]}
{"fn": "log", "message": "…"}
{"fn": "fetch", "method": "GET", "url": "https://api.github.com/…", "headers": {"accept": "application/json"}}
{"fn": "storage_get", "key": "repo"}
{"fn": "storage_set", "key": "repo", "value": "octo/demo"}
{"fn": "storage_delete", "key": "repo"}
{"fn": "storage_keys"}
```

`fetch` の応答は `{"ok": {"status": 200, "headers": {…}, "body": "…"}}`。権限がない・接続先が
許可されていないときは `{"err": "理由"}`。
