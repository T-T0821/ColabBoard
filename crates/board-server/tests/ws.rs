//! 実際に WebSocket でつないで、2 人の参加者の間で同期できることを確かめる。

use std::sync::Arc;
use std::time::Duration;

use board_core::protocol::{Message, Presence, View};
use board_core::{Board, Rgba8, Shape, Vec2};
use board_server::{Config, Hub, Store, router};
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn serve() -> String {
    let hub = Arc::new(Hub::new(Arc::new(Store::memory())));
    let app = router(hub, Config::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("ws://{addr}/ws")
}

async fn recv(ws: &mut Ws) -> Message {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("timeout")
            .unwrap()
            .unwrap();
        if let WsMessage::Binary(b) = msg {
            return Message::decode(&b).unwrap();
        }
    }
}

async fn send(ws: &mut Ws, m: Message) {
    ws.send(WsMessage::Binary(m.encode().into())).await.unwrap();
}

/// 接続して初回同期を受け取る。
async fn join(base: &str, room: &str) -> (Ws, Board, u32) {
    let (mut ws, _) = connect_async(format!("{base}/{room}")).await.unwrap();
    let Message::Sync { you, state } = recv(&mut ws).await else {
        panic!("最初は Sync のはず")
    };
    let mut board = Board::new();
    board.apply_remote(&state).unwrap();
    (ws, board, you)
}

#[tokio::test]
async fn two_clients_sync_edits_and_cursors() {
    let base = serve().await;
    let (mut a, mut board_a, id_a) = join(&base, "room1").await;
    let (mut b, mut board_b, id_b) = join(&base, "room1").await;
    assert_ne!(id_a, id_b);
    assert_eq!(board_a.len(), Board::welcome().len());
    assert_eq!(board_b.len(), board_a.len());

    // A が付箋を足す → B に届く。
    let id = board_a.alloc_id();
    board_a.push(Shape::sticky(id, Vec2::new(10.0, 20.0), Rgba8::WHITE));
    for u in board_a.take_outgoing() {
        send(&mut a, Message::Update(u)).await;
    }
    let Message::Update(u) = recv(&mut b).await else {
        panic!()
    };
    board_b.apply_remote(&u).unwrap();
    assert!(board_b.get(id).is_some());

    // B のカーソルが A に届く(参加者番号はサーバーが付ける)。
    send(
        &mut b,
        Message::Presence(Presence {
            peer: 0,
            x: 1.0,
            y: 2.0,
            color: 0xff0000,
            name: "B".into(),
        }),
    )
    .await;
    let Message::Presence(p) = recv(&mut a).await else {
        panic!()
    };
    assert_eq!((p.peer, p.x, p.y, p.name.as_str()), (id_b, 1.0, 2.0, "B"));

    // 後から来た C は、現在の状態と B のカーソルを受け取る。
    let (mut c, board_c, _) = join(&base, "room1").await;
    assert!(board_c.get(id).is_some());
    let Message::Presence(p) = recv(&mut c).await else {
        panic!()
    };
    assert_eq!(p.peer, id_b);

    // B が抜けると Leave が届く。
    b.close(None).await.unwrap();
    let Message::Leave(peer) = recv(&mut a).await else {
        panic!()
    };
    assert_eq!(peer, id_b);
}

#[tokio::test]
async fn rooms_are_isolated() {
    let base = serve().await;
    let (mut a, mut board_a, _) = join(&base, "left").await;
    let (mut b, _, _) = join(&base, "right").await;

    let id = board_a.alloc_id();
    board_a.push(Shape::sticky(id, Vec2::ZERO, Rgba8::WHITE));
    for u in board_a.take_outgoing() {
        send(&mut a, Message::Update(u)).await;
    }
    // 別ルームには何も届かない。
    let got = tokio::time::timeout(Duration::from_millis(300), b.next()).await;
    assert!(got.is_err(), "別のルームに配信された: {got:?}");

    // 同じルームに後から入れば見える。
    let (_c, board_c, _) = join(&base, "left").await;
    assert!(board_c.get(id).is_some());
}

#[tokio::test]
async fn rejects_bad_room_names() {
    let base = serve().await;
    assert!(connect_async(format!("{base}/bad.name")).await.is_err());
}

#[tokio::test]
async fn views_are_relayed_and_replayed() {
    let base = serve().await;
    let (mut a, _, id_a) = join(&base, "present").await;
    let (mut b, _, _) = join(&base, "present").await;
    let view = View {
        peer: 0,
        min_x: 0.0,
        min_y: 0.0,
        max_x: 1600.0,
        max_y: 900.0,
        presenting: true,
    };
    send(&mut a, Message::View(view)).await;
    let Message::View(v) = recv(&mut b).await else {
        panic!()
    };
    assert_eq!(v, View { peer: id_a, ..view });

    // 不正な範囲は配らない。
    send(
        &mut a,
        Message::View(View {
            max_x: f32::NAN,
            ..view
        }),
    )
    .await;
    let got = tokio::time::timeout(Duration::from_millis(300), b.next()).await;
    assert!(got.is_err(), "不正な View が配信された: {got:?}");

    // 途中参加でもプレゼン中の範囲を受け取る。
    let (mut c, _, _) = join(&base, "present").await;
    let Message::View(v) = recv(&mut c).await else {
        panic!()
    };
    assert!(v.presenting && v.peer == id_a);
}
