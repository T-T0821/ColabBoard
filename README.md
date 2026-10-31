# ColabBoard

Miro や FigJam のように、Web 上で複数人が同時に編集できるホワイトボード。
クライアントもサーバーも Pure Rust で書き、描画は wgpu(WebGPU / WebGL2)で GPU に任せる。

## いまできること

- 無限キャンバスのパン/ズーム(スクロール、Space+ドラッグ、Ctrl/⌘+スクロール、トラックパッド/タッチのピンチ)
- 付箋・四角形・楕円・ペン(手書き)の作成
- 選択(クリック、Shift で追加、範囲選択)、移動、8 方向リサイズ(Shift で縦横比固定)、削除、色変更
- 付箋・図形への文字入力(ダブルクリック / Enter、日本語 IME 対応)と GPU 文字描画
- 矩形・楕円・付箋・手書き線の GPU 描画(SDF による角丸・枠線・影、アンチエイリアス)
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
| `V` `H` `S` `R` `O` `P` | 選択 / 手のひら / 付箋 / 四角形 / 楕円 / ペン |
| スクロール、Space+ドラッグ、中ボタンドラッグ | 移動 |
| Ctrl/⌘ + スクロール、ピンチ | カーソル位置を中心にズーム |
| Shift + クリック / ドラッグ | 追加選択 / 縦横比を保って変形・作成 |
| ダブルクリック / `Enter` | 文字を入力(`Esc` か `Ctrl/⌘+Enter`、または外側クリックで確定) |
| `Delete` / `Backspace` | 選択中の要素を削除 |
| `Ctrl/⌘ + A` / `Esc` | すべて選択 / 選択解除 |
| `+` / `-` / `1` / `0` | ズーム / 全体表示 / 100% |

## 構成

| crate | 役割 |
| --- | --- |
| `board-core` | データモデル(図形・手書き線・ボード)、カメラ座標変換、パン/ズームと編集操作(`Editor`)。描画にも通信にも依存しない |
| `board-render` | wgpu の描画エンジン。WGSL シェーダ(`grid.wgsl`, `shapes.wgsl`)と文字描画(glyphon / cosmic-text) |
| `board-web` | ブラウザ用クライアント(WASM)。canvas、入力イベント、HUD |

### GPU 描画の仕組み

- 1 要素 = 1 インスタンス(40 バイト)。全要素を **1 回の描画命令** で描く。手書き線は線分ごとのカプセルとして同じ命令に入るので、重なり順も保たれる
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
2. ~~編集操作: ツールバー、図形・付箋・手書きの作成、選択・移動・リサイズ~~
3. ~~付箋の文字入力(日本語 IME)と GPU 文字描画~~
4. データモデルを yrs(CRDT)化、Undo/Redo
5. リアルタイム同期: axum サーバー、WebSocket、カーソル共有、SQLite 永続化
6. 計算シェーダによるカリング・LOD の強化、性能計測

## ライセンス

MIT OR Apache-2.0

同梱フォント `crates/board-web/assets/fonts/NotoSansJP-Regular.ttf` は Noto Sans JP のサブセット(SIL Open Font License 1.1、`OFL.txt`)。作り直しは `tools/subset-font.sh`。
