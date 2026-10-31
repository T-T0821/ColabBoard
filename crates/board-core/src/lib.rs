//! ColabBoard のプラットフォーム非依存なコア。
//!
//! 描画にも同期にも依存しないデータモデルと座標変換を持つ。
//! クライアント(WASM)とサーバーの両方から使う。

pub mod board;
pub mod camera;
pub mod color;
pub mod navigation;
pub mod shape;

pub use board::Board;
pub use camera::Camera;
pub use color::Rgba8;
pub use navigation::Navigator;
pub use shape::{Shape, ShapeId, ShapeKind};

pub use glam::{Mat4, Vec2};
