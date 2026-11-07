//! 通報と、管理者による利用停止の結合テスト。

use std::sync::Arc;
use std::time::Duration;

use board_server::{Config, Hub, Store, router};
use futures_util::StreamExt;
use reqwest::StatusCode;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct Server {
    http: String,
    ws: String,
    client: reqwest::Client,
}

impl Server {
    async fn start() -> Self {
        let hub = Arc::new(Hub::new(Arc::new(Store::memory())));
        let config = Config {
            admins: vec!["admin".into()],
            ..Config::default()
        };
        let app = router(hub, config);
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

/// サーバーが接続を切るまで読み捨てる。切られたら true。
async fn closed(ws: &mut Ws) -> bool {
    loop {
        match tokio::time::timeout(Duration::from_secs(5), ws.next()).await {
            Err(_) => return false,
            Ok(None | Some(Err(_)) | Some(Ok(WsMessage::Close(_)))) => return true,
            Ok(Some(Ok(_))) => {}
        }
    }
}

#[tokio::test]
async fn report_and_suspend() {
    const EVIDENCE: &str = "社外秘の資料が貼られています";
    let s = Server::start().await;
    let alice = s.signup("alice").await;
    let carol = s.signup("carol").await;
    let admin = s.signup("admin").await;

    let (st, info) = s
        .call(
            "POST",
            "/api/boards",
            Some(&alice),
            Some(json!({"title": "企画"})),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{info}");
    let id = info["id"].as_str().unwrap().to_string();
    let (st, _) = s
        .call(
            "PUT",
            &format!("/api/boards/{id}/members"),
            Some(&alice),
            Some(json!({"login": "carol", "role": "viewer"})),
        )
        .await;
    assert_eq!(st, StatusCode::OK);

    // 通報: 開ける人なら誰でも。理由は決まったものから選ぶ。
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
            Some(json!({"reason": "leak", "comment": "社外に出してはいけない資料です", "evidence": EVIDENCE})),
        )
        .await;
    assert_eq!(st, StatusCode::CREATED, "{r}");
    let rid = r["id"].as_u64().unwrap();
    // 通報を見られるのはサーバー管理者だけ。
    let (st, _) = s.call("GET", "/api/reports", Some(&alice), None).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, list) = s.call("GET", "/api/reports", Some(&admin), None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(list[0]["evidence"], EVIDENCE);
    assert_eq!(list[0]["reporter"], "carol");

    // 利用停止: 誰も開けなくなり、つないでいる人も切れる。解除すれば戻る。
    let mut ws = s.connect(&id, Some(&alice)).await.unwrap();
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
    assert!(closed(&mut ws).await);
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
    // 添えた文字は通報にだけ残り、操作ログには出ない。
    assert!(!all.contains(EVIDENCE));
}
