//! グラボード のプラットフォーム非依存なコア。
//!
//! 描画にも同期にも依存しないデータモデル・座標変換・編集操作を持つ。
//! クライアント(WASM)とサーバーの両方から使う。

pub mod a11y;
pub mod access;
pub mod api;
pub mod arrange;
pub mod autolayout;
pub mod board;
pub mod camera;
pub mod color;
pub mod connector;
mod crdt;
pub mod diagram;
pub mod drawio;
pub mod editor;
pub mod element;
pub mod export;
pub mod format;
pub mod geom;
pub mod ink;
pub mod issue;
pub mod markdown;
pub mod miro;
pub mod mindmap;
pub mod navigation;
pub mod ops;
pub mod plugin;
pub mod privacy;
pub mod protocol;
pub mod search;
pub mod session;
pub mod shape;
pub mod slides;
pub mod snap;
pub mod spatial;
pub mod stamp;
pub mod table;
pub mod tabular;
pub mod templates;
pub mod tidy;
pub mod uml;
pub mod workshop;

pub use board::Board;
pub use camera::Camera;
pub use color::Rgba8;
pub use editor::{Editor, Modifiers, Tool};
pub use element::{Cap, Connector, Element, ElementId, Endpoint, Meta, PathStyle, Stroke};
pub use geom::Rect;
pub use navigation::Navigator;
pub use shape::{
    ImageData, Shape, ShapeCategory, ShapeKind, TextAlign, TextBox, TextStyle, VAlign,
};

pub use glam::{Mat4, Vec2};
