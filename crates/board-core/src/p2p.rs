//! サーバーを通さない直接接続(P2P)の、ブラウザに依らない部分。
//!
//! ブラウザ側(`board-web` の `p2p`)は WebRTC のデータチャネルで端末どうしを直接つなぐ。
//! 接続手順(SDP)を中継するサーバーがないので、招待する側と参加する側が
//! 「招待コード」と「応答コード」を一度ずつ手で渡す(コピー&ペーストか QR コード)。
//! ここではそのコードの形、データチャネルに流すメッセージの形、大きなメッセージの分割、
//! どの相手に更新を回すか(サーバーとの橋渡し)を決める。
//!
//! ボードの中身は yrs(CRDT)の更新のまま流すので、届く順番が前後しても、同じ更新が
//! 二重に届いても、全員が同じ状態にそろう。

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use crate::protocol::{Message, Presence};

/// コードの先頭に付ける印(形を変えたら数字を上げる)。
pub const CODE_PREFIX: &str = "GB1.";
/// 招待リンクで、URL の `#` 以降にコードを入れるときの名前。`#` 以降はサーバーに送られない。
pub const LINK_PARAM: &str = "p2p";
/// 展開後のコードの大きさの上限(壊れたコードや悪意のあるコードで固まらないように)。
const MAX_CODE_BYTES: usize = 64 * 1024;
const MAX_ROOM_BYTES: usize = 200;
const MAX_NAME_BYTES: usize = 120;

/// データチャネル 1 回で送る大きさ。どのブラウザでも確実に通る大きさにしておく。
pub const CHUNK_BYTES: usize = 16 * 1024;
/// 分割して届くメッセージをつなぎ直すときの上限。
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
/// 直接つなぐ相手の数の上限(全員と 1 本ずつつなぐので、多すぎると回線が足りない)。
pub const MAX_LINKS: usize = 8;
/// 直接つないだ相手のカーソルに使う番号の始まり(サーバーが振る番号と重ならないように)。
pub const PEER_ID_BASE: u32 = 0xF000_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeKind {
    /// 招待する側が作る。
    Offer,
    /// 招待された側が作って、招待した人へ返す。
    Answer,
}

/// 招待コード・応答コードの中身。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Code {
    pub kind: CodeKind,
    /// 招待ごとの番号。応答コードに同じ番号を入れて、どの招待への応答か分かるようにする。
    pub token: u32,
    /// ボード(ルーム)の ID。
    pub room: String,
    /// 作った人の表示名(つなぐ前に「誰とつなぐか」を見せる)。
    pub name: String,
    /// WebRTC の SDP(ICE 候補を含む)。
    pub sdp: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodeError {
    /// コードの形をしていない。
    NotACode,
    /// 途中で切れているか、壊れている。
    Broken,
}

impl std::fmt::Display for CodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotACode => write!(f, "グラボードの接続コードではありません"),
            Self::Broken => write!(f, "コードが途中で切れているか、壊れています"),
        }
    }
}

impl std::error::Error for CodeError {}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

impl Code {
    /// 貼り付けやすい 1 行の文字列にする(圧縮して URL に入れられる文字だけで書く)。
    pub fn encode(&self) -> String {
        let room = truncate(&self.room, MAX_ROOM_BYTES);
        let name = truncate(&self.name, MAX_NAME_BYTES);
        let mut raw = Vec::with_capacity(8 + room.len() + name.len() + self.sdp.len());
        raw.push(match self.kind {
            CodeKind::Offer => b'O',
            CodeKind::Answer => b'A',
        });
        raw.extend_from_slice(&self.token.to_le_bytes());
        raw.push(room.len() as u8);
        raw.extend_from_slice(room.as_bytes());
        raw.push(name.len() as u8);
        raw.extend_from_slice(name.as_bytes());
        raw.extend_from_slice(self.sdp.as_bytes());
        let packed = miniz_oxide::deflate::compress_to_vec(&raw, 9);
        format!("{CODE_PREFIX}{}", URL_SAFE_NO_PAD.encode(packed))
    }

    /// コードを読む。招待リンク(`…#p2p=GB1.…`)をそのまま貼っても、途中で改行が入っても読める。
    pub fn decode(text: &str) -> Result<Self, CodeError> {
        let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        let start = compact.find(CODE_PREFIX).ok_or(CodeError::NotACode)?;
        let body = &compact[start + CODE_PREFIX.len()..];
        // リンクの後ろに別のパラメータが続いていても読めるように、コードに使わない文字で切る。
        let end = body
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
            .unwrap_or(body.len());
        let packed = URL_SAFE_NO_PAD
            .decode(&body[..end])
            .map_err(|_| CodeError::Broken)?;
        let raw = miniz_oxide::inflate::decompress_to_vec_with_limit(&packed, MAX_CODE_BYTES)
            .map_err(|_| CodeError::Broken)?;
        let mut r = Reader(&raw);
        let kind = match r.byte()? {
            b'O' => CodeKind::Offer,
            b'A' => CodeKind::Answer,
            _ => return Err(CodeError::Broken),
        };
        let token = u32::from_le_bytes(r.take(4)?.try_into().map_err(|_| CodeError::Broken)?);
        let n = r.byte()? as usize;
        let room = r.text(n)?;
        let n = r.byte()? as usize;
        let name = r.text(n)?;
        let sdp = String::from_utf8(r.0.to_vec()).map_err(|_| CodeError::Broken)?;
        if room.is_empty() || sdp.is_empty() {
            return Err(CodeError::Broken);
        }
        Ok(Self {
            kind,
            token,
            room,
            name,
            sdp,
        })
    }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], CodeError> {
        if self.0.len() < n {
            return Err(CodeError::Broken);
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }
    fn byte(&mut self) -> Result<u8, CodeError> {
        Ok(self.take(1)?[0])
    }
    fn text(&mut self, n: usize) -> Result<String, CodeError> {
        String::from_utf8(self.take(n)?.to_vec()).map_err(|_| CodeError::Broken)
    }
}

// ---- データチャネルに流すメッセージ ----

const HELLO: u8 = 1;
const UPDATE: u8 = 2;
const PRESENCE: u8 = 3;
const BYE: u8 = 4;

/// 直接つないだ相手とやり取りするメッセージ。
#[derive(Clone, Debug, PartialEq)]
pub enum PeerMsg {
    /// つながったとき(とサーバーへの接続状態が変わったとき)に送る。
    /// 受け取った側は、相手の状態ベクトル以降の差分を [`PeerMsg::Update`] で返す。
    Hello {
        /// yrs の状態ベクトル。
        state_vector: Vec<u8>,
        /// 送った人が今サーバーにつながっているか(二重に届けないための目安)。
        online: bool,
    },
    /// yrs v1 の更新。
    Update(Vec<u8>),
    /// カーソルと名前(`peer` は受け取った側で振り直す)。
    Presence(Presence),
    /// 切断する(相手の画面からカーソルを消す)。
    Bye,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerMsgError;

impl std::fmt::Display for PeerMsgError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "読めないメッセージ")
    }
}

impl PeerMsg {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::Hello {
                state_vector,
                online,
            } => {
                let mut out = Vec::with_capacity(2 + state_vector.len());
                out.push(HELLO);
                out.push(u8::from(*online));
                out.extend_from_slice(state_vector);
                out
            }
            Self::Update(u) => {
                let mut out = Vec::with_capacity(1 + u.len());
                out.push(UPDATE);
                out.extend_from_slice(u);
                out
            }
            Self::Presence(p) => {
                let mut out = vec![PRESENCE];
                out.extend_from_slice(&Message::Presence(p.clone()).encode());
                out
            }
            Self::Bye => vec![BYE],
        }
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, PeerMsgError> {
        let (&kind, rest) = bytes.split_first().ok_or(PeerMsgError)?;
        match kind {
            HELLO => {
                let (&online, sv) = rest.split_first().ok_or(PeerMsgError)?;
                Ok(Self::Hello {
                    state_vector: sv.to_vec(),
                    online: online != 0,
                })
            }
            UPDATE => Ok(Self::Update(rest.to_vec())),
            PRESENCE => match Message::decode(rest) {
                Ok(Message::Presence(p)) => Ok(Self::Presence(p)),
                _ => Err(PeerMsgError),
            },
            BYE => Ok(Self::Bye),
            _ => Err(PeerMsgError),
        }
    }
}

// ---- 分割と組み立て ----

const WHOLE: u8 = 0;
const PART: u8 = 1;

/// メッセージを [`CHUNK_BYTES`] 以下の断片に分ける。`id` は分割したメッセージの番号。
pub fn split(msg: &[u8], id: u32) -> Vec<Vec<u8>> {
    if msg.len() < CHUNK_BYTES {
        let mut out = Vec::with_capacity(1 + msg.len());
        out.push(WHOLE);
        out.extend_from_slice(msg);
        return vec![out];
    }
    const HEADER: usize = 13;
    let size = CHUNK_BYTES - HEADER;
    let count = msg.len().div_ceil(size) as u32;
    msg.chunks(size)
        .enumerate()
        .map(|(i, part)| {
            let mut out = Vec::with_capacity(HEADER + part.len());
            out.push(PART);
            out.extend_from_slice(&id.to_le_bytes());
            out.extend_from_slice(&(i as u32).to_le_bytes());
            out.extend_from_slice(&count.to_le_bytes());
            out.extend_from_slice(part);
            out
        })
        .collect()
}

/// [`split`] した断片を組み立て直す。データチャネルは順番どおりに届く設定で使う。
#[derive(Default)]
pub struct Reassembler {
    partial: Option<(u32, u32, Vec<u8>)>,
    next: u32,
}

impl Reassembler {
    /// 断片を 1 つ渡す。メッセージがそろったら返す。
    pub fn push(&mut self, frame: &[u8]) -> Result<Option<Vec<u8>>, PeerMsgError> {
        let (&kind, rest) = frame.split_first().ok_or(PeerMsgError)?;
        match kind {
            WHOLE => Ok(Some(rest.to_vec())),
            PART => {
                if rest.len() < 12 {
                    return Err(PeerMsgError);
                }
                let num = |i: usize| u32::from_le_bytes(rest[i..i + 4].try_into().unwrap());
                let (id, index, count) = (num(0), num(4), num(8));
                let body = &rest[12..];
                if index == 0 {
                    self.partial = Some((id, count, Vec::new()));
                    self.next = 0;
                }
                let Some((pid, pcount, buf)) = self.partial.as_mut() else {
                    return Err(PeerMsgError);
                };
                if *pid != id || *pcount != count || self.next != index {
                    self.partial = None;
                    return Err(PeerMsgError);
                }
                if buf.len() + body.len() > MAX_MESSAGE_BYTES {
                    self.partial = None;
                    return Err(PeerMsgError);
                }
                buf.extend_from_slice(body);
                self.next += 1;
                if self.next == count {
                    let (_, _, buf) = self.partial.take().unwrap();
                    return Ok(Some(buf));
                }
                Ok(None)
            }
            _ => Err(PeerMsgError),
        }
    }
}

// ---- 更新の回し方 ----

/// 更新がどこから来たか。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// 自分の編集。
    Local,
    /// サーバーから届いた。
    Server,
    /// 直接つないだ相手から届いた(相手の番号)。
    Peer(u32),
}

/// 回し先を決めるのに要る、直接つないだ相手の様子。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LinkView {
    pub id: u32,
    /// データチャネルが開いている。
    pub open: bool,
    /// 相手がサーバーにつながっている(相手が最後に知らせてきた状態)。
    pub online: bool,
}

/// 回し先。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Route {
    pub server: bool,
    pub peers: Vec<u32>,
}

/// 更新をどこへ回すかを決める。
///
/// 直接つないだ相手とサーバーの両方につながっている人は「橋」になり、サーバーにいない相手の
/// 編集をサーバーへ、サーバーから届いた編集をサーバーにいない相手へ回す。相手もサーバーに
/// つながっていれば、サーバー経由で届くので送らない(同じ更新を二重に流さない)。
///
/// `new_info` は、その更新で自分のボードが変わったか。変わらなかった(もう知っていた)更新は
/// 回さないので、相手どうしが輪になっていても行き来が止まる。
pub fn route(source: Source, new_info: bool, me_online: bool, links: &[LinkView]) -> Route {
    let open = links.iter().filter(|l| l.open);
    match source {
        Source::Local => Route {
            server: me_online,
            peers: open
                .filter(|l| !(l.online && me_online))
                .map(|l| l.id)
                .collect(),
        },
        _ if !new_info => Route::default(),
        Source::Server => Route {
            server: false,
            peers: open.filter(|l| !l.online).map(|l| l.id).collect(),
        },
        Source::Peer(from) => {
            let from_online = links.iter().any(|l| l.id == from && l.online);
            Route {
                server: me_online && !from_online,
                peers: open
                    .filter(|l| l.id != from)
                    // その相手がサーバーにいて、自分か送り主がサーバーへ流すなら、サーバー経由で届く。
                    .filter(|l| !(l.online && (me_online || from_online)))
                    .map(|l| l.id)
                    .collect(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code() -> Code {
        Code {
            kind: CodeKind::Offer,
            token: 0xDEAD_BEEF,
            room: "team-meeting".into(),
            name: "佐藤".into(),
            sdp: "v=0\r\no=- 46117327 2 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\na=group:BUNDLE 0\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\nc=IN IP4 0.0.0.0\r\na=candidate:1 1 udp 2113937151 3c1f2a.local 51234 typ host\r\na=ice-ufrag:abcd\r\na=ice-pwd:0123456789abcdef01234567\r\na=fingerprint:sha-256 AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99\r\na=setup:actpass\r\na=mid:0\r\na=sctp-port:5000\r\na=max-message-size:262144\r\n".into(),
        }
    }

    #[test]
    fn code_round_trip() {
        let c = code();
        let text = c.encode();
        assert!(text.starts_with(CODE_PREFIX));
        assert!(
            text.chars()
                .all(|ch| ch.is_ascii_alphanumeric() || "._-".contains(ch))
        );
        assert_eq!(Code::decode(&text).unwrap(), c);
        // base64 にしても、元の SDP と大きく変わらない長さに収まる。
        assert!(
            text.len() < c.sdp.len() * 6 / 5,
            "{} / {}",
            text.len(),
            c.sdp.len()
        );
    }

    #[test]
    fn code_from_link_and_wrapped_text() {
        let c = code();
        let text = c.encode();
        let link = format!("https://example.com/?room=team-meeting#{LINK_PARAM}={text}&x=1");
        assert_eq!(Code::decode(&link).unwrap(), c);
        let wrapped: String = text
            .chars()
            .enumerate()
            .flat_map(|(i, ch)| {
                if i % 40 == 39 {
                    vec![ch, '\n']
                } else {
                    vec![ch]
                }
            })
            .collect();
        assert_eq!(Code::decode(&format!("  {wrapped}  ")).unwrap(), c);
    }

    #[test]
    fn bad_codes() {
        assert_eq!(Code::decode("こんにちは"), Err(CodeError::NotACode));
        assert_eq!(Code::decode("GB1.!!!"), Err(CodeError::Broken));
        let text = code().encode();
        assert_eq!(
            Code::decode(&text[..text.len() / 2]),
            Err(CodeError::Broken)
        );
        // 展開すると大きくなりすぎるもの。
        let bomb = miniz_oxide::deflate::compress_to_vec(&vec![b'O'; MAX_CODE_BYTES * 4], 9);
        let bomb = format!("{CODE_PREFIX}{}", URL_SAFE_NO_PAD.encode(bomb));
        assert_eq!(Code::decode(&bomb), Err(CodeError::Broken));
    }

    #[test]
    fn long_names_are_cut_on_char_boundary() {
        let mut c = code();
        c.name = "あ".repeat(100);
        let back = Code::decode(&c.encode()).unwrap();
        assert!(back.name.len() <= MAX_NAME_BYTES);
        assert!(back.name.chars().all(|ch| ch == 'あ'));
    }

    #[test]
    fn peer_messages_round_trip() {
        let msgs = [
            PeerMsg::Hello {
                state_vector: vec![1, 2, 3],
                online: true,
            },
            PeerMsg::Hello {
                state_vector: vec![],
                online: false,
            },
            PeerMsg::Update(vec![9; 100]),
            PeerMsg::Presence(Presence {
                peer: 3,
                x: 1.5,
                y: -2.0,
                color: 0x12_3456,
                name: "鈴木".into(),
            }),
            PeerMsg::Bye,
        ];
        for m in msgs {
            assert_eq!(PeerMsg::decode(&m.encode()).unwrap(), m);
        }
        assert!(PeerMsg::decode(&[]).is_err());
        assert!(PeerMsg::decode(&[99]).is_err());
    }

    #[test]
    fn split_and_reassemble() {
        let mut r = Reassembler::default();
        let small = vec![7u8; 100];
        let frames = split(&small, 1);
        assert_eq!(frames.len(), 1);
        assert_eq!(r.push(&frames[0]).unwrap(), Some(small));

        let big: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let frames = split(&big, 2);
        assert!(frames.len() > 10);
        assert!(frames.iter().all(|f| f.len() <= CHUNK_BYTES));
        let mut out = None;
        for (i, f) in frames.iter().enumerate() {
            let got = r.push(f).unwrap();
            if i + 1 < frames.len() {
                assert!(got.is_none());
            } else {
                out = got;
            }
        }
        assert_eq!(out, Some(big));
    }

    #[test]
    fn reassemble_rejects_gaps() {
        let big = vec![1u8; CHUNK_BYTES * 3];
        let frames = split(&big, 5);
        let mut r = Reassembler::default();
        assert!(r.push(&frames[0]).unwrap().is_none());
        assert!(r.push(&frames[2]).is_err());
        // 次のメッセージからは普通に組み立てられる。
        let frames = split(&big, 6);
        let mut last = None;
        for f in &frames {
            last = r.push(f).unwrap();
        }
        assert_eq!(last, Some(big));
    }

    fn link(id: u32, online: bool) -> LinkView {
        LinkView {
            id,
            open: true,
            online,
        }
    }

    #[test]
    fn local_edits() {
        // サーバーなし: 全員へ直接。
        let links = [link(1, false), link(2, true)];
        let r = route(Source::Local, true, false, &links);
        assert_eq!(
            r,
            Route {
                server: false,
                peers: vec![1, 2]
            }
        );
        // 自分も相手もサーバーにいるなら、サーバー経由で届くので直接は送らない。
        let r = route(Source::Local, true, true, &links);
        assert_eq!(
            r,
            Route {
                server: true,
                peers: vec![1]
            }
        );
        // 閉じている相手には送らない。
        let closed = [LinkView {
            id: 1,
            open: false,
            online: false,
        }];
        assert_eq!(
            route(Source::Local, true, false, &closed).peers,
            Vec::<u32>::new()
        );
    }

    #[test]
    fn bridging() {
        let links = [link(1, false), link(2, true), link(3, false)];
        // サーバーからの更新は、サーバーにいない相手にだけ回す。
        let r = route(Source::Server, true, true, &links);
        assert_eq!(
            r,
            Route {
                server: false,
                peers: vec![1, 3]
            }
        );
        // サーバーにいない相手の更新は、サーバーとほかのサーバーにいない相手へ。
        let r = route(Source::Peer(1), true, true, &links);
        assert_eq!(
            r,
            Route {
                server: true,
                peers: vec![3]
            }
        );
        // 送り主がサーバーにいるなら、サーバーへは送らない。サーバーにいる相手にも届いている。
        let r = route(Source::Peer(2), true, true, &links);
        assert_eq!(
            r,
            Route {
                server: false,
                peers: vec![1, 3]
            }
        );
        // 自分がサーバーにいない: サーバーにいる相手 2 には、送り主もいないので直接回す。
        let r = route(Source::Peer(1), true, false, &links);
        assert_eq!(
            r,
            Route {
                server: false,
                peers: vec![2, 3]
            }
        );
    }

    #[test]
    fn known_updates_stop() {
        let links = [link(1, false), link(2, false)];
        assert_eq!(
            route(Source::Peer(1), false, true, &links),
            Route::default()
        );
        assert_eq!(route(Source::Server, false, true, &links), Route::default());
    }
}
