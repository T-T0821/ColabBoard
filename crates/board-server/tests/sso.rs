//! OpenID Connect の SSO を、テスト用の IdP(このファイルの中の小さな HTTP サーバー)とつないで確かめる。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Body;
use axum::extract::{Form, State};
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use board_server::oidc::{Oidc, OidcConfig};
use board_server::{Config, Hub, Store, router};
use http_body_util::BodyExt;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

/// 発行待ちのコード → (nonce, code_challenge, ID トークンに入れる値)。
type Codes = Arc<Mutex<HashMap<String, (String, String, Value)>>>;

struct Idp {
    url: String,
    codes: Codes,
}

#[derive(Clone)]
struct IdpState {
    url: String,
    codes: Codes,
}

fn key() -> SigningKey {
    SigningKey::from_slice(&[9u8; 32]).unwrap()
}

async fn start_idp() -> Idp {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let codes: Codes = Arc::default();
    let state = IdpState {
        url: url.clone(),
        codes: codes.clone(),
    };
    let app = Router::new()
        .route("/.well-known/openid-configuration", get(discovery))
        .route("/jwks", get(jwks))
        .route("/token", post(token))
        .with_state(state);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Idp { url, codes }
}

async fn discovery(State(s): State<IdpState>) -> impl IntoResponse {
    axum::Json(json!({
        "issuer": s.url,
        "authorization_endpoint": format!("{}/authorize", s.url),
        "token_endpoint": format!("{}/token", s.url),
        "jwks_uri": format!("{}/jwks", s.url),
    }))
}

async fn jwks() -> impl IntoResponse {
    let p = key().verifying_key().to_encoded_point(false);
    axum::Json(json!({"keys": [{
        "kty": "EC", "crv": "P-256", "kid": "test", "use": "sig", "alg": "ES256",
        "x": B64.encode(p.x().unwrap()), "y": B64.encode(p.y().unwrap()),
    }]}))
}

async fn token(
    State(s): State<IdpState>,
    headers: HeaderMap,
    Form(form): Form<HashMap<String, String>>,
) -> impl IntoResponse {
    // クライアント認証(Basic)。
    let expected = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode("graboard:s3cret")
    );
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        != Some(expected.as_str())
    {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(json!({"error": "invalid_client"})),
        );
    }
    let code = form.get("code").cloned().unwrap_or_default();
    let Some((nonce, challenge, mut claims)) = s.codes.lock().unwrap().remove(&code) else {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({"error": "invalid_grant"})),
        );
    };
    // PKCE。
    let verifier = form.get("code_verifier").cloned().unwrap_or_default();
    if B64.encode(Sha256::digest(verifier.as_bytes())) != challenge {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({"error": "invalid_grant", "error_description": "PKCE"})),
        );
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    claims["iss"] = s.url.clone().into();
    claims["aud"] = "graboard".into();
    claims["nonce"] = nonce.into();
    claims["iat"] = now.into();
    claims["exp"] = (now + 300).into();
    let h = B64.encode(json!({"alg": "ES256", "kid": "test"}).to_string());
    let p = B64.encode(claims.to_string());
    let sig: Signature = key().sign(format!("{h}.{p}").as_bytes());
    let id_token = format!("{h}.{p}.{}", B64.encode(sig.to_bytes()));
    (
        StatusCode::OK,
        axum::Json(json!({"access_token": "x", "token_type": "Bearer", "id_token": id_token})),
    )
}

struct Server {
    app: Router,
}

impl Server {
    async fn call(
        &self,
        method: &str,
        path: &str,
        cookie: Option<&str>,
        body: Option<Value>,
    ) -> (StatusCode, Value, HeaderMap) {
        let mut req = Request::builder().method(method).uri(path);
        if let Some(c) = cookie {
            req = req.header(header::COOKIE, c);
        }
        let req = match body {
            Some(b) => req
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(b.to_string())),
            None => req.body(Body::empty()),
        }
        .unwrap();
        let res = self.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            headers,
        )
    }

    /// SSO でログインする。IdP が返す中身は `claims`。戻り値は (Location, セッション Cookie)。
    async fn sso(&self, idp: &Idp, claims: Value, next: &str) -> (String, Option<String>) {
        let (status, _, h) = self
            .call(
                "GET",
                &format!("/api/sso/oidc/start?next={next}"),
                None,
                None,
            )
            .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        let loc = h[header::LOCATION].to_str().unwrap().to_string();
        assert!(loc.starts_with(&format!("{}/authorize?", idp.url)), "{loc}");
        let q: HashMap<String, String> = loc
            .split_once('?')
            .unwrap()
            .1
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["client_id"], "graboard");
        let state_cookie = cookie_named(&h, "cb_oidc").unwrap();
        let code = format!("code-{}", q["state"]);
        idp.codes.lock().unwrap().insert(
            code.clone(),
            (q["nonce"].clone(), q["code_challenge"].clone(), claims),
        );
        let (status, _, h) = self
            .call(
                "GET",
                &format!("/api/sso/oidc/callback?code={code}&state={}", q["state"]),
                Some(&state_cookie),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        (
            h[header::LOCATION].to_str().unwrap().to_string(),
            cookie_named(&h, "cb_session"),
        )
    }
}

fn cookie_named(h: &HeaderMap, name: &str) -> Option<String> {
    h.get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .map(|v| v.split(';').next().unwrap().to_string())
        .find(|c| c.starts_with(&format!("{name}=")) && c.len() > name.len() + 1)
}

fn server(idp: &Idp, password_login: bool, allowed: Vec<String>) -> Server {
    let oidc = Oidc::new(OidcConfig {
        issuer: idp.url.clone(),
        client_id: "graboard".into(),
        client_secret: "s3cret".into(),
        redirect_url: "http://localhost:8080/api/sso/oidc/callback".into(),
        name: "社内 SSO".into(),
        scopes: "openid email profile".into(),
        allowed_domains: allowed,
    });
    let hub = Arc::new(Hub::new(Arc::new(Store::memory())));
    Server {
        app: router(
            hub,
            Config {
                oidc: Some(Arc::new(oidc)),
                password_login,
                admins: vec!["root".into()],
                ..Config::default()
            },
        ),
    }
}

#[tokio::test]
async fn oidc_login_provisions_and_joins_teams() {
    let idp = start_idp().await;
    let s = server(&idp, true, Vec::new());

    let (_, status, _) = s.call("GET", "/api/sso", None, None).await;
    assert_eq!(status["oidc"], "社内 SSO");
    assert_eq!(status["password_login"], true);

    // サーバー管理者がチームに自動参加ドメインを設定する。
    let (_, _, h) = s
        .call(
            "POST",
            "/api/signup",
            None,
            Some(json!({"login": "root", "name": "", "password": "password123"})),
        )
        .await;
    let root = cookie_named(&h, "cb_session").unwrap();
    let (_, team, _) = s
        .call(
            "POST",
            "/api/teams",
            Some(&root),
            Some(json!({"name": "全社"})),
        )
        .await;
    let tid = team["id"].as_str().unwrap().to_string();
    let (status, team, _) = s
        .call(
            "PUT",
            &format!("/api/teams/{tid}/sso"),
            Some(&root),
            Some(json!({"domains": ["Example.co.jp"]})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{team}");
    assert_eq!(team["sso_domains"], json!(["example.co.jp"]));

    // 初めての SSO ログインでアカウントができ、チームに入る。
    let claims = json!({
        "sub": "u-1", "email": "taro.yamada@example.co.jp", "email_verified": true,
        "name": "山田 太郎", "preferred_username": "taro.yamada",
    });
    let (loc, session) = s.sso(&idp, claims.clone(), "?room=abc").await;
    assert_eq!(loc, "/?room=abc");
    let session = session.unwrap();
    let (_, me, _) = s.call("GET", "/api/me", Some(&session), None).await;
    assert_eq!(me["login"], "taro-yamada");
    assert_eq!(me["name"], "山田 太郎");
    let (_, teams, _) = s.call("GET", "/api/teams", Some(&session), None).await;
    assert_eq!(teams[0]["id"], tid.as_str());

    // 2 回目は同じアカウント。
    let (_, session2) = s.sso(&idp, claims, "").await;
    let (_, me, _) = s
        .call("GET", "/api/me", Some(&session2.unwrap()), None)
        .await;
    assert_eq!(me["login"], "taro-yamada");

    // 確認されていないメールではチームに入らない。
    let (_, session3) = s
        .sso(
            &idp,
            json!({"sub": "u-2", "email": "eve@example.co.jp", "email_verified": false}),
            "",
        )
        .await;
    let session3 = session3.unwrap();
    let (_, teams, _) = s.call("GET", "/api/teams", Some(&session3), None).await;
    assert_eq!(teams.as_array().unwrap().len(), 0);

    // 操作ログに残る。
    let (_, events, _) = s
        .call("GET", "/api/audit?action=auth", Some(&root), None)
        .await;
    assert!(
        events
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["action"] == "auth.sso" && e["actor"] == "taro-yamada")
    );
    // チームの設定はサーバー管理者だけ。
    let (status, _, _) = s
        .call(
            "PUT",
            &format!("/api/teams/{tid}/sso"),
            Some(&session3),
            Some(json!({"domains": ["evil.com"]})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn oidc_rejects_bad_callbacks() {
    let idp = start_idp().await;
    let s = server(&idp, false, vec!["example.com".into()]);

    // パスワードのログインは止めてある。
    let (status, _, _) = s
        .call(
            "POST",
            "/api/signup",
            None,
            Some(json!({"login": "mallory", "name": "", "password": "password123"})),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // 始めたときの Cookie がない(別のブラウザー)→ 断る。
    let (_, _, h) = s.call("GET", "/api/sso/oidc/start", None, None).await;
    let loc = h[header::LOCATION].to_str().unwrap().to_string();
    let state = loc
        .split("state=")
        .nth(1)
        .unwrap()
        .split('&')
        .next()
        .unwrap()
        .to_string();
    let (status, _, h) = s
        .call(
            "GET",
            &format!("/api/sso/oidc/callback?code=x&state={state}"),
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(
        h[header::LOCATION]
            .to_str()
            .unwrap()
            .starts_with("/?sso_error=")
    );
    assert!(cookie_named(&h, "cb_session").is_none());

    // 許可されていないドメイン。
    let (loc, session) = s
        .sso(
            &idp,
            json!({"sub": "u-9", "email": "a@other.com", "email_verified": true}),
            "",
        )
        .await;
    assert!(loc.starts_with("/?sso_error="), "{loc}");
    assert!(session.is_none());

    // 許可されたドメインなら入れる。
    let (loc, session) = s
        .sso(
            &idp,
            json!({"sub": "u-10", "email": "b@example.com", "email_verified": true}),
            "//evil.com",
        )
        .await;
    // 外のサイトへの戻り先は無視する。
    assert_eq!(loc, "/");
    assert!(session.is_some());
}
