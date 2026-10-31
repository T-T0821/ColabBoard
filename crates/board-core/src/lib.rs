//! ColabBoard のプラットフォーム非依存なコア。
//!
//! 描画にも同期にも依存しないデータモデル・座標変換・編集操作を持つ。
//! クライアント(WASM)とサーバーの両方から使う。

pub mod api;
pub mod arrange;
pub mod board;
pub mod camera;
pub mod color;
pub mod connector;
mod crdt;
pub mod diagram;
pub mod editor;
pub mod element;
pub mod format;
pub mod geom;
pub mod navigation;
pub mod protocol;
pub mod session;
pub mod shape;
pub mod stamp;
pub mod tidy;

pub use board::Board;
pub use camera::Camera;
pub use color::Rgba8;
pub use editor::{Editor, Modifiers, Tool};
pub use element::{Cap, Connector, Element, ElementId, Endpoint, Meta, PathStyle, Stroke};
pub use geom::Rect;
pub use navigation::Navigator;
pub use shape::{Shape, ShapeKind, TextAlign, TextBox, TextStyle};

pub use glam::{Mat4, Vec2};
