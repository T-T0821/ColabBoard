//! 本物のサーバーを立てて SDK から使う。

use std::sync::Arc;

use board_server::{Config, Hub, Store, router};
use graboard_sdk::{
    Batch, Client, ElementBody, ElementPatch, ElementQuery, NewElement, ShapeStyle,
};

/// サーバーを立て、ユーザーを作ってトークンを発行する。
async fn setup() -> (String, String) {
    let hub = Arc::new(Hub::new(Arc::new(Store::memory())));
    let app = router(hub, Config::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let http = reqwest::Client::new();
    let res = http
        .post(format!("{base}/api/signup"))
        .json(&serde_json::json!({"login": "alice", "name": "アリス", "password": "password123"}))
        .send()
        .await
        .unwrap();
    let cookie = res.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let v: serde_json::Value = http
        .post(format!("{base}/api/tokens"))
        .header("cookie", cookie)
        .json(&serde_json::json!({"name": "sdk", "scope": "write"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    (base, v["token"].as_str().unwrap().to_string())
}

#[tokio::test]
async fn sdk_round_trip() {
    let (base, token) = setup().await;
    let client = Client::new(&base, token).unwrap();
    assert_eq!(client.me().await.unwrap().name, "アリス");

    let info = client.create_board("SDK").await.unwrap();
    let board = client.board(&info.id);
    let before = board
        .elements(&ElementQuery::default())
        .await
        .unwrap()
        .len();

    let note = board
        .create(NewElement::sticky(0.0, 0.0, "hello"))
        .await
        .unwrap();
    let note = board.set_text(&note.id, "こんにちは").await.unwrap();
    assert!(matches!(&note.body, ElementBody::Shape { text, .. } if text == "こんにちは"));
    let moved = board.move_to(&note.id, 50.0, 60.0).await.unwrap();
    assert!(matches!(moved.body, ElementBody::Shape { x, y, .. } if x == 50.0 && y == 60.0));

    let mut batch = Batch::new();
    let a = batch.create(NewElement::shape("rect", 0.0, 400.0, 160.0, 100.0));
    let b = batch.create(NewElement::shape("ellipse", 400.0, 400.0, 160.0, 100.0));
    batch.create(NewElement::connector(a.endpoint(), b.endpoint()));
    batch.update(
        b.as_str(),
        ElementPatch {
            shape: ShapeStyle {
                fill: Some("#b5e3ff".into()),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    let r = board.apply(batch).await.unwrap();
    assert_eq!(r.created.len(), 3);
    assert_eq!(
        board
            .elements(&ElementQuery::default())
            .await
            .unwrap()
            .len(),
        before + 4
    );
    assert_eq!(
        board
            .stickies()
            .await
            .unwrap()
            .iter()
            .filter(|e| e.id == note.id)
            .count(),
        1
    );

    board.delete(&note.id).await.unwrap();
    let err = board.element(&note.id).await.unwrap_err();
    assert!(err.is_not_found(), "{err}");

    // 入力の誤りはメッセージ付きの 400。
    let err = board
        .create(NewElement::shape("nope", 0.0, 0.0, 10.0, 10.0))
        .await
        .unwrap_err();
    assert_eq!(err.status(), Some(400));
    assert!(err.to_string().contains("nope"));

    board.comment("SDK から", 10.0, 10.0).await.unwrap();
    assert_eq!(board.comments().await.unwrap().len(), 1);

    // 間違ったトークン。
    let bad = Client::new(&base, "gbt_x_y").unwrap();
    assert_eq!(bad.me().await.unwrap_err().status(), Some(401));
    assert!(Client::new("ftp://x", "t").is_err());
}

/// サーバーで書き出す・読み込ませる(CLI の `pull` / `push` が使う)。
#[tokio::test]
async fn export_and_import() {
    let (base, token) = setup().await;
    let client = Client::new(&base, token).unwrap();
    let info = client.create_board("書き出し").await.unwrap();
    let board = client.board(&info.id);

    // Mermaid を読み込ませる。
    let created = board
        .import(
            Some("mermaid"),
            "graph LR\n A[開始] --> B[終了]".as_bytes().to_vec(),
        )
        .await
        .unwrap();
    assert!(created.len() >= 3);
    // 形式を省いても中身から分かる。
    let more = board
        .import(None, b"# Memo\n- one\n- two".to_vec())
        .await
        .unwrap();
    assert!(!more.is_empty());

    let png = board.export("png", &[("scale", "1".into())]).await.unwrap();
    assert_eq!(&png[1..4], b"PNG");
    let pdf = board.export("pdf", &[]).await.unwrap();
    assert!(pdf.starts_with(b"%PDF"));
    let md = board.export("md", &[]).await.unwrap();
    assert!(String::from_utf8(md).unwrap().contains("one"));
    let file = board.export("graboard", &[]).await.unwrap();
    let copy = board_core::convert::load(&file).unwrap();
    assert!(copy.len() >= created.len() + more.len());

    // `.graboard` を別のボードへ(画像なども欠けずに写る)。
    let other = client.create_board("写し").await.unwrap();
    let n = client
        .board(&other.id)
        .import(Some("graboard"), file)
        .await
        .unwrap()
        .len();
    assert_eq!(n, copy.len());

    // 誤った設定はエラー。
    let err = board.export("gif", &[]).await.unwrap_err();
    assert_eq!(err.status(), Some(400));
    let err = board
        .export("png", &[("scale", "99".into())])
        .await
        .unwrap_err();
    assert_eq!(err.status(), Some(400));
    let err = board
        .export("png", &[("frame", "ない".into())])
        .await
        .unwrap_err();
    assert_eq!(err.status(), Some(404));
    // ログインしていない人は書き出せない。
    let anon = Client::anonymous(&base).unwrap();
    assert!(anon.board(&info.id).export("png", &[]).await.is_err());
}
