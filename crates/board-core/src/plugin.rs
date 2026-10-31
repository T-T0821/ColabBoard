//! WASM プラグインの取り決め。型は [`board_types::plugin`] にあり、ここではホスト関数の処理
//! ([`HostContext`])を持つ。サーバーと、プラグインのテスト用の擬似ホストで共有する。

pub use board_types::plugin::*;

use serde_json::Value;

use crate::board::Board;
use crate::ops;

/// ホスト関数を実行する側の状態。サーバーと、プラグインのテスト用の擬似ホストで共有する。
pub struct HostContext {
    /// 操作するボード(実行の後に取り出す)。
    pub board: Board,
    pub can_read: bool,
    pub can_write: bool,
    pub logs: Vec<String>,
    /// 適用した操作の数。
    pub ops_applied: usize,
}

/// ログの上限。
pub const MAX_LOG_LINES: usize = 200;
pub const MAX_LOG_CHARS: usize = 1000;

impl HostContext {
    pub fn new(board: Board, can_read: bool, can_write: bool) -> Self {
        Self {
            board,
            can_read,
            can_write,
            logs: Vec::new(),
            ops_applied: 0,
        }
    }

    /// 1 回の呼び出しを処理して JSON の値を返す。
    pub fn dispatch(&mut self, call: HostCall) -> Result<Value, String> {
        let need_read = |ok: bool| {
            if ok {
                Ok(())
            } else {
                Err("このプラグインには「ボードを読む」権限がありません".to_string())
            }
        };
        match call {
            HostCall::Query { query } => {
                need_read(self.can_read)?;
                Ok(serde_json::to_value(ops::query(&self.board, &query)).unwrap_or(Value::Null))
            }
            HostCall::Get { id } => {
                need_read(self.can_read)?;
                Ok(serde_json::to_value(ops::get(&self.board, &id)).unwrap_or(Value::Null))
            }
            HostCall::Apply { ops: list } => {
                if !self.can_write {
                    return Err("このプラグインには「ボードを編集する」権限がありません".into());
                }
                // 先に写しで試し、失敗する一括操作は手元のボードに一切触れない。
                let mut trial = Board::new();
                trial.apply_remote(&self.board.encode_diff(None)?)?;
                ops::apply(&mut trial, &list).map_err(|e| e.to_string())?;
                let r = ops::apply(&mut self.board, &list).map_err(|e| e.to_string())?;
                self.ops_applied += list.len();
                Ok(serde_json::to_value(r).unwrap_or(Value::Null))
            }
            HostCall::Log { message } => {
                if self.logs.len() < MAX_LOG_LINES {
                    self.logs
                        .push(message.chars().take(MAX_LOG_CHARS).collect());
                }
                Ok(Value::Null)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::{ElementQuery, Op};
    use serde_json::json;

    #[test]
    fn manifest_validation() {
        let mut m: PluginManifest = serde_json::from_value(json!({
            "id": "tidy", "name": "整理", "permissions": ["board:read", "board:write"],
            "commands": [{"id": "run", "title": "実行"}]
        }))
        .unwrap();
        assert!(m.validate().is_ok());
        assert!(m.can(Permission::BoardWrite));
        m.id = "Bad Id".into();
        assert!(m.validate().is_err());
        m.id = "ok".into();
        m.abi = 9;
        assert!(m.validate().is_err());
        m.abi = ABI_VERSION;
        m.commands.clear();
        assert!(m.validate().is_err());
    }

    #[test]
    fn dispatch_respects_permissions_and_atomicity() {
        let mut ro = HostContext::new(Board::new(), true, false);
        assert!(
            ro.dispatch(HostCall::Query {
                query: ElementQuery::default()
            })
            .is_ok()
        );
        let create: Vec<Op> = serde_json::from_value(json!([
            {"op": "create", "element": {"type": "shape", "kind": "sticky", "text": "a"}}
        ]))
        .unwrap();
        assert!(
            ro.dispatch(HostCall::Apply {
                ops: create.clone()
            })
            .is_err()
        );

        let mut rw = HostContext::new(ro.board, true, true);
        let r = rw.dispatch(HostCall::Apply { ops: create }).unwrap();
        assert_eq!(r["created"].as_array().unwrap().len(), 1);
        // 2 つ目で失敗する一括操作は 1 つ目も残さない。
        let bad: Vec<Op> = serde_json::from_value(json!([
            {"op": "create", "element": {"type": "shape", "kind": "sticky"}},
            {"op": "delete", "id": "ffffff"}
        ]))
        .unwrap();
        assert!(rw.dispatch(HostCall::Apply { ops: bad }).is_err());
        rw.dispatch(HostCall::Log {
            message: "hi".into(),
        })
        .unwrap();
        assert_eq!(rw.logs, vec!["hi".to_string()]);
        assert_eq!(rw.ops_applied, 1);
        assert_eq!(rw.board.len(), 1);

        let mut none = HostContext::new(rw.board, false, false);
        assert!(none.dispatch(HostCall::Get { id: "1".into() }).is_err());
    }
}
