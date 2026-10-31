//! クライアントとサーバーの間でやり取りする WebSocket メッセージ(バイナリ)。
//!
//! 先頭 1 バイトが種類、残りが中身。数値はすべてリトルエンディアン。
//!
//! | 種類 | 向き | 中身 |
//! |------|------|------|
//! | `0x00` Sync | サーバー → クライアント | 自分の参加者番号 `u32` + ボード全体(yrs v1 更新) |
//! | `0x01` Update | 双方向 | yrs v1 更新 |
//! | `0x02` Presence | 双方向 | 参加者番号 `u32`, x `f32`, y `f32`, 色 `u32`, 名前(UTF-8) |
//! | `0x03` Leave | サーバー → クライアント | 参加者番号 `u32` |
//!
//! Presence の参加者番号はクライアントからは 0 で送り、サーバーが送り主の番号に書き換えて配る。
//! 座標はワールド座標。カーソルがボードの外に出たら x, y を NaN にする。

const SYNC: u8 = 0x00;
const UPDATE: u8 = 0x01;
const PRESENCE: u8 = 0x02;
const LEAVE: u8 = 0x03;

/// 名前の最大バイト数(超えた分は切り詰める)。
pub const MAX_NAME_BYTES: usize = 64;

/// 他の参加者のカーソル位置と表示情報。
#[derive(Clone, Debug, PartialEq)]
pub struct Presence {
    pub peer: u32,
    pub x: f32,
    pub y: f32,
    /// `0xRRGGBB`。
    pub color: u32,
    pub name: String,
}

impl Presence {
    /// カーソルがボード上にあるか。
    pub fn is_visible(&self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Message {
    Sync { you: u32, state: Vec<u8> },
    Update(Vec<u8>),
    Presence(Presence),
    Leave(u32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    Empty,
    UnknownKind(u8),
    Truncated,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "空のメッセージ"),
            Self::UnknownKind(k) => write!(f, "不明なメッセージ種類 0x{k:02x}"),
            Self::Truncated => write!(f, "メッセージが途中で切れています"),
        }
    }
}

impl std::error::Error for DecodeError {}

impl Message {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::Sync { you, state } => {
                let mut out = Vec::with_capacity(5 + state.len());
                out.push(SYNC);
                out.extend_from_slice(&you.to_le_bytes());
                out.extend_from_slice(state);
                out
            }
            Self::Update(u) => {
                let mut out = Vec::with_capacity(1 + u.len());
                out.push(UPDATE);
                out.extend_from_slice(u);
                out
            }
            Self::Presence(p) => {
                let name = truncate_utf8(&p.name, MAX_NAME_BYTES);
                let mut out = Vec::with_capacity(17 + name.len());
                out.push(PRESENCE);
                out.extend_from_slice(&p.peer.to_le_bytes());
                out.extend_from_slice(&p.x.to_le_bytes());
                out.extend_from_slice(&p.y.to_le_bytes());
                out.extend_from_slice(&p.color.to_le_bytes());
                out.extend_from_slice(name.as_bytes());
                out
            }
            Self::Leave(peer) => {
                let mut out = vec![LEAVE];
                out.extend_from_slice(&peer.to_le_bytes());
                out
            }
        }
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let (&kind, body) = bytes.split_first().ok_or(DecodeError::Empty)?;
        match kind {
            SYNC => {
                let you = read_u32(body, 0)?;
                Ok(Self::Sync {
                    you,
                    state: body[4..].to_vec(),
                })
            }
            UPDATE => Ok(Self::Update(body.to_vec())),
            PRESENCE => {
                let peer = read_u32(body, 0)?;
                let x = f32::from_bits(read_u32(body, 4)?);
                let y = f32::from_bits(read_u32(body, 8)?);
                let color = read_u32(body, 12)?;
                let name = String::from_utf8_lossy(&body[16..]);
                let name = truncate_utf8(&name, MAX_NAME_BYTES).to_string();
                Ok(Self::Presence(Presence {
                    peer,
                    x,
                    y,
                    color,
                    name,
                }))
            }
            LEAVE => Ok(Self::Leave(read_u32(body, 0)?)),
            k => Err(DecodeError::UnknownKind(k)),
        }
    }
}

fn read_u32(b: &[u8], at: usize) -> Result<u32, DecodeError> {
    let s = b.get(at..at + 4).ok_or(DecodeError::Truncated)?;
    Ok(u32::from_le_bytes(s.try_into().unwrap()))
}

fn truncate_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(m: Message) {
        assert_eq!(Message::decode(&m.encode()).unwrap(), m);
    }

    #[test]
    fn roundtrips() {
        roundtrip(Message::Sync {
            you: 7,
            state: vec![1, 2, 3],
        });
        roundtrip(Message::Sync {
            you: 0,
            state: vec![],
        });
        roundtrip(Message::Update(vec![9; 100]));
        roundtrip(Message::Presence(Presence {
            peer: 42,
            x: -12.5,
            y: 3.25,
            color: 0x2f6fed,
            name: "ゲスト・カワウソ".into(),
        }));
        roundtrip(Message::Leave(u32::MAX));
    }

    #[test]
    fn hidden_cursor_is_nan() {
        let p = Presence {
            peer: 1,
            x: f32::NAN,
            y: f32::NAN,
            color: 0,
            name: String::new(),
        };
        let Message::Presence(back) = Message::decode(&Message::Presence(p).encode()).unwrap()
        else {
            panic!()
        };
        assert!(!back.is_visible());
    }

    #[test]
    fn long_names_are_truncated_on_char_boundary() {
        let name = "あ".repeat(40); // 120 バイト
        let m = Message::Presence(Presence {
            peer: 1,
            x: 0.0,
            y: 0.0,
            color: 0,
            name,
        });
        let Message::Presence(p) = Message::decode(&m.encode()).unwrap() else {
            panic!()
        };
        assert_eq!(p.name.len(), 63);
        assert_eq!(p.name.chars().count(), 21);
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(Message::decode(&[]), Err(DecodeError::Empty));
        assert_eq!(
            Message::decode(&[0x7f]),
            Err(DecodeError::UnknownKind(0x7f))
        );
        assert_eq!(Message::decode(&[SYNC, 1, 2]), Err(DecodeError::Truncated));
        assert_eq!(
            Message::decode(&[PRESENCE, 0, 0, 0, 0, 1]),
            Err(DecodeError::Truncated)
        );
    }
}
