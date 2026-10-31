# ColabBoard

Miro や FigJam のように、Web 上で複数人が同時に編集できるホワイトボード。
クライアントもサーバーも Pure Rust で書き、描画は wgpu(WebGPU / WebGL2)で GPU に任せる。

## いまできること

- 無限キャンバスのパン/ズーム(ドラッグ、スクロール、Ctrl/⌘+スクロール、トラックパッド/タッチのピンチ)
- 矩形・楕円・付箋の GPU 描画(SDF による角丸・枠線・影、アンチエイリアス)
- ダミー要素 1 千〜100 万個での性能確認(右上のボタン、または `?n=100000`)
- WebGPU が使えないブラウザでは WebGL2 に自動で切り替え(`?webgl` で強制)

## 動かし方

```sh
rustup target add wasm32-unknown-unknown
cargo install trunk --locked
cd crates/board-web
trunk serve --release --open
```

| 操作 | 動作 |
| --- | --- |
| ドラッグ / スクロール | 移動 |
| Ctrl/⌘ + スクロール、ピンチ | カーソル位置を中心にズーム |
| `+` / `-` | ズーム |
| `1` | 全体表示 |
| `0` | 100% |

## 構成

| crate | 役割 |
| --- | --- |
| `board-core` | データモデル(図形・ボード)、カメラ座標変換、パン/ズーム操作の解釈。描画にも通信にも依存しない |
| `board-render` | wgpu の描画エンジン。WGSL シェーダ(`grid.wgsl`, `shapes.wgsl`) |
| `board-web` | ブラウザ用クライアント(WASM)。canvas、入力イベント、HUD |

### GPU 描画の仕組み

- 1 要素 = 1 インスタンス(40 バイト)。全要素を **1 回の描画命令** で描く
- 輪郭・枠線・付箋の影はフラグメントシェーダの SDF で計算するので、どの倍率でも輪郭がぼけない
- 画面外の要素と 1px 未満の要素は頂点シェーダで捨てる(GPU 側カリング)
- パン/ズームで GPU に送るのはカメラ情報 96 バイトだけ。要素の転送はボードが変わったときのみ

## 開発

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p board-web --target wasm32-unknown-unknown -- -D warnings
cargo test --workspace   # シェーダの naga 検証も含む
```

## ロードマップ

1. ~~土台と描画エンジン~~
2. 編集操作: ツールバー、図形・付箋・手書きの作成、選択・移動・リサイズ、文字描画
3. データモデルを yrs(CRDT)化、Undo/Redo
4. リアルタイム同期: axum サーバー、WebSocket、カーソル共有、SQLite 永続化
5. 計算シェーダによるカリング・LOD の強化、性能計測

## ライセンス

MIT OR Apache-2.0
