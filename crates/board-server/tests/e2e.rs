//! エンドツーエンド暗号化ボード: サーバーは暗号文を保存・中継するだけで中身を読めないこと、
//! 権限・鍵の作り直し・管理者のオン・オフ・操作ログ・通報が働くこと。

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use board_core::protocol::Message;
use board_core::{Board, Element, Rgba8, Shape, Vec2};
use board_e2e::{BoardKey, Kind};
use board_server::{Config, Hub, Store, router};
use futures_util::{SinkExt, StreamExt};
use reqwest::StatusCode;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

const SECRET: &str = "社外秘:来期の買収先はA社";

fn rng(buf: &mut [u8]) {
    getrandom::fill(buf).unwrap();
}

struct Server {
    http: String,
    ws: String,
    hub: Arc<Hub>,
    client: reqwest::Client,
}

impl Server {
    async fn start() -> Self {
        let hub = Arc::new(Hub::new(Arc::new(Store::memory())));
        let config = Config {
            admins: vec!["admin".into()],
            ..Config::default()
        };
        let app = router(hub.clone(), config);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap()
        });
        Self {
            http: format!("http://{addr}"),
            ws: format!("ws://{addr}/ws"),
            hub,
            client: reqwest::Client::new(),
        }
    }

    async fn call(
        &self,
        method: &str,
        path: &str,
        cookie: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let mut req = self
            .client
            .request(method.parse().unwrap(), format!("{}{path}", self.http));
        if let Some(c) = cookie {
            req = req.header("cookie", c);
        }
        if let Some(b) = body {
            req = req.json(&b);
        }
        let res = req.send().await.unwrap();
        let status = res.status();
        let v = res.json::<Value>().await.unwrap_or(Value::Null);
        (status, v)
    }

    async fn signup(&self, login: &str) -> String {
        let res = self
            .client
            .post(format!("{}/api/signup", self.http))
            .json(&json!({"login": login, "name": login, "password": "password123"}))
            .send()
            .await
            .unwrap();
        assert!(res.status().is_success());
        res.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_string()
    }

    async fn connect(&self, room: &str, cookie: Option<&str>) -> Result<Ws, String> {
        let mut req = format!("{}/{room}", self.ws).into_client_request().unwrap();
        if let Some(c) = cookie {
            req.headers_mut().insert("cookie", c.parse().unwrap());
        }
        connect_async(req)
            .await
            .map(|(ws, _)| ws)
            .map_err(|e| e.to_string())
    }
}

async fn recv(ws: &mut Ws) -> Option<Message> {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("timeout")?;
        match msg {
            Ok(WsMessage::Binary(b)) => return Some(Message::decode(&b).unwrap()),
            Ok(WsMessage::Close(_)) | Err(_) => return None,
            _ => {}
        }
    }
}

async fn send(ws: &mut Ws, m: Message) {
    ws.send(WsMessage::Binary(m.encode().into())).await.unwrap();
}

/// 参加して、ログを復号して取り込んだボードを返す。
async fn join(s: &Server, room: &str, cookie: &str, key: &BoardKey) -> (Ws, Board, u64, usize) {
    let mut ws = s.connect(room, Some(cookie)).await.unwrap();
    let Some(Message::SealedSync { base, blobs, .. }) = recv(&mut ws).await else {
        panic!("暗号化ボードは SealedSync で始まるはず")
    };
    let mut plain = Vec::new();
    for (i, b) in blobs.iter().enumerate() {
        // 先頭がまとめた全体か、1 回の編集か。両方試す。
        let p = key
            .open(room, Kind::Update, b)
            .or_else(|_| key.open(room, Kind::Snapshot, b))
            .unwrap_or_else(|e| panic!("{i} 件目を復号できません: {e}"));
        plain.push(p);
    }
    let mut board = Board::new();
    let merged = board_core::board::merge_updates(&plain).unwrap();
    if !merged.is_empty() {
        board.apply_remote(&merged).unwrap();
    }
    let n = blobs.len();
    (ws, board, base, n)
}

fn sticky_with_text(board: &mut Board, text: &str) {
    let id = board.alloc_id();
    let mut s = Shape::sticky(id, Vec2::new(10.0, 20.0), Rgba8::WHITE);
    s.text = text.into();
    board.push(s);
}

fn texts(board: &Board) -> Vec<String> {
    board
        .elements()
        .iter()
        .filter_map(|e| match e {
            Element::Shape(s) => Some(s.text.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn sealed_board_end_to_end() {
    let s = Server::start().await;
    let alice = s.signup("alice").await;
    let bob = s.signup("bob").await;
    let carol = s.signup("carol").await;
    let admin = s.signup("admin").await;

    let key = BoardKey::generate(&mut rng);
    let (st, info) = s
        .call(
            "POST",
            "/api/boards",
            Some(&alice),
            Some(json!({"title": "極秘", "e2e": key.key_id()})),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{info}");
    assert_eq!(info["e2e"]["key_id"], key.key_id());
    let id = info["id"].as_str().unwrap().to_string();
    // 鍵の指紋でないものは断る。
    let (st, _) = s
        .call(
            "POST",
            "/api/boards",
            Some(&alice),
            Some(json!({"title": "x", "e2e": "nope"})),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    for (login, role) in [("bob", "editor"), ("carol", "viewer")] {
        let (st, _) = s
            .call(
                "PUT",
                &format!("/api/boards/{id}/members"),
                Some(&alice),
                Some(json!({"login": login, "role": role})),
            )
            .await;
        assert_eq!(st, StatusCode::OK);
    }

    // 新しい暗号化ボードは空(サーバーは案内の付箋も書けない)。
    let (mut a, mut board_a, base, n) = join(&s, &id, &alice, &key).await;
    assert_eq!((base, n, board_a.len()), (0, 0, 0));

    // A が秘密の付箋を書いて、暗号化して送る。
    sticky_with_text(&mut board_a, SECRET);
    for u in board_a.take_outgoing() {
        let blob = key.seal(&id, Kind::Update, &u, &mut rng);
        send(&mut a, Message::Sealed { seq: 0, blob }).await;
    }

    // B が参加すると、ログを復号して同じ内容になる。
    let (mut b, mut board_b, _, n) = join(&s, &id, &bob, &key).await;
    assert!(n >= 1);
    assert_eq!(texts(&board_b), vec![SECRET.to_string()]);

    // B の編集は番号付きで A に届く。
    sticky_with_text(&mut board_b, "B のメモ");
    for u in board_b.take_outgoing() {
        send(
            &mut b,
            Message::Sealed {
                seq: 0,
                blob: key.seal(&id, Kind::Update, &u, &mut rng),
            },
        )
        .await;
    }
    let Some(Message::Sealed { seq, blob }) = recv(&mut a).await else {
        panic!()
    };
    assert_eq!(seq, n as u64);
    board_a
        .apply_remote(&key.open(&id, Kind::Update, &blob).unwrap())
        .unwrap();
    assert_eq!(board_a.len(), 2);

    // 閲覧だけの C の暗号文は捨てられる。平文の更新も受け付けない。
    let mut c = s.connect(&id, Some(&carol)).await.unwrap();
    let _ = recv(&mut c).await;
    send(
        &mut c,
        Message::Sealed {
            seq: 0,
            blob: vec![1; 64],
        },
    )
    .await;
    send(&mut a, Message::Update(board_a.encode_diff(None).unwrap())).await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    // 保存データは暗号文のログで、秘密の文字は含まれない。
    s.hub.flush();
    let saved = s.hub.store().load(&id).unwrap().unwrap();
    assert!(board_server::sealed::is_sealed(&saved));
    let log = board_server::sealed::SealedLog::decode(&saved).unwrap();
    assert_eq!(log.entries.len(), n + 1, "閲覧者や平文の更新は足されない");
    assert!(!saved.windows(SECRET.len()).any(|w| w == SECRET.as_bytes()));
    // サーバーの中身を読む入口は暗号化ボードでは断る(または何も返さない)。
    assert_eq!(s.hub.current_state(&id).unwrap(), None);
    assert!(s.hub.read(&id, |b| b.len()).is_err());

    for (method, path, body) in [
        (
            "POST",
            format!("/api/boards/{id}/comments"),
            json!({"body": "x", "x": 0.0, "y": 0.0}),
        ),
        (
            "POST",
            format!("/api/boards/{id}/versions"),
            json!({"label": "v"}),
        ),
        (
            "PUT",
            format!("/api/boards/{id}/public"),
            json!({"enabled": true}),
        ),
        ("GET", format!("/api/v1/boards/{id}/elements"), Value::Null),
        (
            "POST",
            format!("/api/boards/{id}/issues"),
            json!({"urls": []}),
        ),
    ] {
        let body = (!body.is_null()).then_some(body);
        let (st, v) = s.call(method, &path, Some(&alice), body).await;
        assert_eq!(st, StatusCode::CONFLICT, "{method} {path}: {v}");
    }

    // ログをまとめる: 全体を 1 つの暗号文にして置き換える。
    let whole = board_a.encode_diff(None).unwrap();
    let snap = key.seal(&id, Kind::Snapshot, &whole, &mut rng);
    send(
        &mut a,
        Message::Compact {
            base: 0,
            upto: (n + 1) as u64,
            blob: snap,
        },
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (_d, board_d, base, n2) = join(&s, &id, &bob, &key).await;
    assert_eq!((base, n2), (n as u64, 1));
    assert_eq!(board_d.len(), 2);

    // 鍵を作り直す(オーナーだけ)。つないでいる人は切れ、古い鍵では読めなくなる。
    let new_key = BoardKey::generate(&mut rng);
    let snap = new_key.seal(&id, Kind::Snapshot, &whole, &mut rng);
    let body = json!({
        "key_id": new_key.key_id(),
        "snapshot": base64::engine::general_purpose::STANDARD.encode(&snap),
    });
    let (st, _) = s
        .call(
            "POST",
            &format!("/api/boards/{id}/e2e/rotate"),
            Some(&bob),
            Some(body.clone()),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, v) = s
        .call(
            "POST",
            &format!("/api/boards/{id}/e2e/rotate"),
            Some(&alice),
            Some(body),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(v["rotated"].as_u64().unwrap() > 0);
    assert!(recv(&mut b).await.is_none(), "鍵を作り直すとつなぎ直させる");
    let mut ws = s.connect(&id, Some(&bob)).await.unwrap();
    let Some(Message::SealedSync { blobs, .. }) = recv(&mut ws).await else {
        panic!()
    };
    assert_eq!(blobs.len(), 1);
    assert!(key.open(&id, Kind::Snapshot, &blobs[0]).is_err());
    assert!(new_key.open(&id, Kind::Snapshot, &blobs[0]).is_ok());
    let (_, info) = s
        .call("GET", &format!("/api/boards/{id}"), Some(&bob), None)
        .await;
    assert_eq!(info["e2e"]["key_id"], new_key.key_id());

    // 操作ログには誰がいつ使ったかだけが残り、中身は残らない。
    let (st, events) = s
        .call("GET", "/api/audit?limit=500", Some(&admin), None)
        .await;
    assert_eq!(st, StatusCode::OK);
    let actions: Vec<&str> = events
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["board"] == id.as_str())
        .map(|e| e["action"].as_str().unwrap())
        .collect();
    for want in ["board.e2e.create", "board.e2e.open", "board.e2e.rotate"] {
        assert!(actions.contains(&want), "{want} がない: {actions:?}");
    }
    // 参加は同じ人なら間引く(bob は何度もつないだが 1 回)。
    let bob_opens = events
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["action"] == "board.e2e.open" && e["actor"] == "bob")
        .count();
    assert_eq!(bob_opens, 1);
    assert!(!events.to_string().contains(SECRET));

    // 通報: 開ける人なら誰でも。中身を添えるかは通報する人が決める。
    let (st, _) = s
        .call(
            "POST",
            &format!("/api/boards/{id}/reports"),
            Some(&carol),
            Some(json!({"reason": "bogus"})),
        )
        .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, r) = s
        .call(
            "POST",
            &format!("/api/boards/{id}/reports"),
            Some(&carol),
            Some(json!({"reason": "leak", "comment": "社外に出してはいけない資料です", "evidence": SECRET})),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{r}");
    assert_eq!(r["e2e"], true);
    let rid = r["id"].as_u64().unwrap();
    // 通報を見られるのはサーバー管理者だけ。
    let (st, _) = s.call("GET", "/api/reports", Some(&alice), None).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, list) = s.call("GET", "/api/reports", Some(&admin), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(list[0]["evidence"], SECRET);
    assert_eq!(list[0]["reporter"], "carol");

    // 利用停止: 誰も開けなくなり、つないでいる人も切れる。解除すれば戻る。
    let (st, v) = s
        .call(
            "PUT",
            &format!("/api/reports/{rid}"),
            Some(&admin),
            Some(json!({"status": "resolved", "suspend": true})),
        )
        .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["suspended"], true);
    assert!(recv(&mut ws).await.is_none());
    let (st, _) = s
        .call("GET", &format!("/api/boards/{id}"), Some(&alice), None)
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert!(s.connect(&id, Some(&alice)).await.is_err());
    s.call(
        "PUT",
        &format!("/api/reports/{rid}"),
        Some(&admin),
        Some(json!({"status": "dismissed", "suspend": false})),
    )
    .await;
    let (st, _) = s
        .call("GET", &format!("/api/boards/{id}"), Some(&alice), None)
        .await;
    assert_eq!(st, StatusCode::OK);
    let (_, events) = s
        .call("GET", "/api/audit?limit=500", Some(&admin), None)
        .await;
    let all = events.to_string();
    for want in [
        "board.report",
        "board.suspend",
        "board.unsuspend",
        "report.handle",
    ] {
        assert!(all.contains(want), "{want} がない");
    }
    // 添えた中身は通報にだけ残り、操作ログには出ない。
    assert!(!all.contains(SECRET));
}

#[tokio::test]
async fn admins_turn_sealed_boards_on_and_off() {
    let s = Server::start().await;
    let alice = s.signup("alice").await;
    let admin = s.signup("admin").await;
    let key = BoardKey::generate(&mut rng);
    let new_board = |team: Option<String>| {
        let mut b = json!({"title": "t", "e2e": key.key_id()});
        if let Some(t) = team {
            b["team"] = json!(t);
        }
        b
    };

    // チームでは既定で不可。チームの管理者が許可すると作れる。
    let (_, team) = s
        .call(
            "POST",
            "/api/teams",
            Some(&alice),
            Some(json!({"name": "営業"})),
        )
        .await;
    let tid = team["id"].as_str().unwrap().to_string();
    assert_eq!(team["e2e"], Value::Null);
    let (st, _) = s
        .call(
            "POST",
            "/api/boards",
            Some(&alice),
            Some(new_board(Some(tid.clone()))),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (_, p) = s.call("GET", "/api/e2e", Some(&alice), None).await;
    assert_eq!(p, json!({"enabled": true, "teams": []}));
    let (st, t) = s
        .call(
            "PUT",
            &format!("/api/teams/{tid}/e2e"),
            Some(&alice),
            Some(json!({"allowed": true})),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(t["e2e"], true);
    let (_, p) = s.call("GET", "/api/e2e", Some(&alice), None).await;
    assert_eq!(p["teams"], json!([tid]));
    let (st, _) = s
        .call(
            "POST",
            "/api/boards",
            Some(&alice),
            Some(new_board(Some(tid.clone()))),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);

    // 個人の暗号化ボードは、許可していないチームには移せない。
    let (_, other) = s
        .call(
            "POST",
            "/api/teams",
            Some(&alice),
            Some(json!({"name": "経理"})),
        )
        .await;
    let (_, b) = s
        .call("POST", "/api/boards", Some(&alice), Some(new_board(None)))
        .await;
    let bid = b["id"].as_str().unwrap().to_string();
    let (st, _) = s
        .call(
            "PUT",
            &format!("/api/boards/{bid}/team"),
            Some(&alice),
            Some(json!({"team": other["id"]})),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // サーバー全体のオン・オフはサーバー管理者だけ。止めると新しく作れない(既存のボードは開ける)。
    let (st, _) = s
        .call(
            "PUT",
            "/api/e2e",
            Some(&alice),
            Some(json!({"enabled": false})),
        )
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = s
        .call(
            "PUT",
            "/api/e2e",
            Some(&admin),
            Some(json!({"enabled": false})),
        )
        .await;
    assert_eq!(st, StatusCode::OK);
    let (_, p) = s.call("GET", "/api/e2e", Some(&alice), None).await;
    assert_eq!(p["enabled"], false);
    let (st, v) = s
        .call("POST", "/api/boards", Some(&alice), Some(new_board(None)))
        .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    let (st, _) = s
        .call("GET", &format!("/api/boards/{bid}"), Some(&alice), None)
        .await;
    assert_eq!(st, StatusCode::OK);
    // 普通のボードはこれまでどおり作れる。
    let (st, _) = s
        .call(
            "POST",
            "/api/boards",
            Some(&alice),
            Some(json!({"title": "普通"})),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED);
    // API トークンなど画面以外からは暗号化ボードを作らない。
    let (_, events) = s
        .call("GET", "/api/audit?limit=500", Some(&admin), None)
        .await;
    let all = events.to_string();
    assert!(all.contains("暗号化ボード: 許可"));
    assert!(all.contains("暗号化ボード: 無効"));
}
