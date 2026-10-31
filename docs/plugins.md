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

## 安全のための仕組み

- 実行は [wasmi](https://github.com/wasmi-labs/wasmi)(Rust 製の WebAssembly インタープリター)。
  プラグインに渡すのは `graboard.call` と `graboard.response` の 2 つの関数だけで、
  ファイル・ネットワーク・時計・乱数(WASI)はない。ほかの関数を読み込むモジュールは入れられない。
- 命令数(燃料、既定 20 億)とメモリ(既定 64 MB)に上限がある。無限ループや使いすぎは止める。
- ホスト関数の呼び出しは 1 回の実行で 1 万回まで、1 回のやり取りは 16 MB まで。
- 権限はマニフェストで宣言する(`board:read` / `board:write`)。宣言していないことはできない。
- `board:write` のプラグインは、実行する人がそのボードの編集者以上のときだけ実行できる。
- 書き込みは使い捨てのボードに溜め、**実行が最後まで成功したときだけ**、CRDT の更新として
  そのボードを開いている全員に配る。途中で止まったりエラーを返したりしたら何も変わらない。
  1 回の `apply` も、1 つでも失敗した操作があれば何も変えない。
- ユーザーが入れたプラグインは本人しか使えない。全員に使わせたいものはサーバーの管理者が `PLUGIN_DIR` に置く。

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
| `GET` | `/api/v1/plugins` | 使えるプラグイン(サーバーのもの + 自分のもの)。マニフェストと `source`(`server` / `user`) |
| `POST` | `/api/v1/plugins` | 入れる(本文は `.wasm`、8 MB まで)。1 人 30 個まで |
| `DELETE` | `/api/v1/plugins/{id}` | 自分のプラグインを消す |
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
  "fuel_used": 3763489
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

例は `crates/board-plugin/examples/`(`tidy_by_color.rs`、`board_stats.rs`)。

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
  "commands": [{"id": "tidy", "title": "付箋を色ごとに並べる"}],
  "abi": 1
}
```

入力(`RunInput`):

```json
{"command": "tidy", "args": {}, "board": {"id": "abc", "title": "振り返り"},
 "user": {"login": "alice", "name": "Alice"}, "selection": ["5f3a00000001"], "can_write": true}
```

呼び出し(`HostCall`):

```json
{"fn": "query", "query": {"kind": "sticky", "bbox": [0, 0, 1000, 800]}}
{"fn": "get", "id": "5f3a00000001"}
{"fn": "apply", "ops": [{"op": "update", "id": "5f3a00000001", "set": {"x": 100}}]}
{"fn": "log", "message": "…"}
```
