//! グラボード の外部向けの JSON の型。
//!
//! REST API(`/api/v1`)と WASM プラグインでやり取りする形を、serde だけに依存する
//! 軽いクレートにまとめたもの。プラグイン(`wasm32-unknown-unknown`)からも使える。
//! ボードへの適用などの処理は `board-core` の同名のモジュールにある。

pub mod ops;
pub mod plugin;
