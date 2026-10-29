//! OAuth 로그인 흐름과 토큰 갱신 종단간 테스트.
//!
//! 실제 Google 대신 wiremock 서버를 인증/토큰 엔드포인트로 사용한다.

use std::collections::HashMap;
use std::io::{BufRead, Read};
use std::path::Path;
use std::process::{Child, ChildStderr, ExitStatus, Stdio};
use std::sync::Arc;

use chrono::{DateTime, TimeZone, Utc};
use serde_json::{Value, json};
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use subscription_toolkit::account::Account;
use subscription_toolkit::auth::store::{FileTokenStore, StoredToken};
use subscription_toolkit::auth::{OAuthTokenSource, TokenSource};
use subscription_toolkit::paths::Paths;
use subscription_toolkit::youtube::ApiError;

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 29, 12, 0, 0)
        .single()
        .expect("시각 생성")
}

fn stored_token(expires_at: Option<DateTime<Utc>>) -> StoredToken {
    StoredToken {
        client_id: "cid.apps.googleusercontent.com".to_string(),
        client_secret: "csecret".to_string(),
        refresh_token: "refresh-1".to_string(),
        access_token: Some("old-access".to_string()),
        expires_at,
        channel_id: "UC_DST".to_string(),
        channel_title: "대상 채널".to_string(),
    }
}

fn write_client_secret(home: &Path, server: &MockServer) {
    std::fs::create_dir_all(home).expect("홈 디렉터리 생성");
    let payload = json!({
        "installed": {
            "client_id": "cid.apps.googleusercontent.com",
            "client_secret": "csecret",
            "auth_uri": format!("{}/authorize", server.uri()),
            "token_uri": format!("{}/token", server.uri()),
        }
    });
    std::fs::write(
        home.join("client_secret.json"),
        serde_json::to_vec_pretty(&payload).expect("직렬화"),
    )
    .expect("설정 파일 쓰기");
}

/// 로그인 프로세스를 띄우고 인증 URL을 읽는다. stderr는 EOF까지 계속 소비한다.
struct LoginProcess {
    child: Child,
    stderr_lines: std::io::Lines<std::io::BufReader<ChildStderr>>,
    authorize_url: Url,
    seen_stderr: String,
}

impl LoginProcess {
    fn spawn(home: &Path, api_base: &str) -> Self {
        let binary = assert_cmd::cargo::cargo_bin("stk");
        let mut child = std::process::Command::new(binary)
            .args(["auth", "login", "--account", "source", "--no-browser"])
            .env("STK_HOME", home)
            .env("STK_API_BASE", api_base)
            .env_remove("STK_ACCESS_TOKEN")
            .env_remove("STK_SOURCE_ACCESS_TOKEN")
            .env_remove("STK_TARGET_ACCESS_TOKEN")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("로그인 프로세스 실행 실패");

        let stderr = child.stderr.take().expect("stderr 파이프");
        let mut stderr_lines = std::io::BufReader::new(stderr).lines();
        let mut seen_stderr = String::new();
        let mut authorize_url = None;
        for line in stderr_lines.by_ref() {
            let line = line.expect("stderr 읽기 실패");
            seen_stderr.push_str(&line);
            seen_stderr.push('\n');
            if line.starts_with("http") {
                authorize_url = Some(Url::parse(&line).expect("인증 URL 파싱 실패"));
                break;
            }
        }

        Self {
            child,
            stderr_lines,
            authorize_url: authorize_url.expect("인증 URL이 출력되지 않았습니다"),
            seen_stderr,
        }
    }

    fn callback_targets(&self) -> (String, String) {
        let params: HashMap<String, String> =
            self.authorize_url.query_pairs().into_owned().collect();
        (
            params
                .get("redirect_uri")
                .expect("redirect_uri가 없습니다")
                .clone(),
            params.get("state").expect("state가 없습니다").clone(),
        )
    }

    fn finish(mut self) -> (ExitStatus, String, String) {
        let mut stderr = self.seen_stderr.clone();
        for line in self.stderr_lines.by_ref().map_while(Result::ok) {
            stderr.push_str(&line);
            stderr.push('\n');
        }
        let mut stdout = String::new();
        if let Some(mut pipe) = self.child.stdout.take() {
            let _ = pipe.read_to_string(&mut stdout);
        }
        let status = self.child.wait().expect("프로세스 종료 대기 실패");
        (status, stdout, stderr)
    }
}

async fn complete_callback(redirect_uri: &str, state: &str) -> reqwest::StatusCode {
    let target = format!("{redirect_uri}/?code=test-code&state={state}");
    let response = reqwest::get(&target).await.expect("콜백 요청 실패");
    response.status()
}

async fn mock_token_success(server: &MockServer, body: Value) {
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

async fn mock_channels(server: &MockServer, access_token: &str, items: Value) {
    Mock::given(method("GET"))
        .and(path("/youtube/v3/channels"))
        .and(wiremock::matchers::header(
            "authorization",
            format!("Bearer {access_token}"),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"items": items})))
        .mount(server)
        .await;
}

#[tokio::test]
async fn login_saves_token_after_callback() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().expect("임시 디렉터리");
    write_client_secret(dir.path(), &server);
    mock_token_success(
        &server,
        json!({
            "access_token": "access-1",
            "expires_in": 3600,
            "refresh_token": "refresh-1",
            "token_type": "Bearer",
        }),
    )
    .await;
    mock_channels(
        &server,
        "access-1",
        json!([{"id": "UC_SRC", "snippet": {"title": "원본 채널"}}]),
    )
    .await;

    let api_base = server.uri() + "/youtube/v3";
    let login = LoginProcess::spawn(dir.path(), &api_base);
    let (redirect_uri, state) = login.callback_targets();
    assert!(
        redirect_uri.starts_with("http://127.0.0.1:"),
        "루프백 리다이렉트"
    );
    assert_eq!(
        complete_callback(&redirect_uri, &state).await,
        reqwest::StatusCode::OK,
        "콜백 서버가 200을 돌려줘야 한다"
    );

    let (status, stdout, stderr) = login.finish();
    assert!(status.success(), "로그인 실패: {stderr}");
    assert!(
        stdout.contains("UC_SRC"),
        "채널 정보를 출력해야 한다: {stdout}"
    );
    assert!(stdout.contains("원본 채널"), "{stdout}");

    let token_path = dir.path().join("tokens").join("source.json");
    let saved: Value =
        serde_json::from_slice(&std::fs::read(&token_path).expect("토큰 파일이 저장되어야 한다"))
            .expect("토큰 JSON 파싱");
    assert_eq!(saved["refresh_token"], "refresh-1");
    assert_eq!(saved["access_token"], "access-1");
    assert_eq!(saved["channel_id"], "UC_SRC");
    assert_eq!(saved["channel_title"], "원본 채널");
    assert!(saved["expires_at"].is_string(), "만료 시각을 저장해야 한다");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&token_path)
            .expect("메타데이터")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    // 토큰을 저장했으므로 인증 상태 조회가 로그인됨으로 바뀐다.
    let output = std::process::Command::new(assert_cmd::cargo::cargo_bin("stk"))
        .args(["auth", "status", "--json"])
        .env("STK_HOME", dir.path())
        .output()
        .expect("상태 조회 실패");
    let report: Value = serde_json::from_slice(&output.stdout).expect("JSON 파싱");
    assert_eq!(report["source"]["logged_in"], true);
    assert_eq!(report["source"]["channel_id"], "UC_SRC");
}

#[tokio::test]
async fn login_requires_refresh_token() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().expect("임시 디렉터리");
    write_client_secret(dir.path(), &server);
    mock_token_success(
        &server,
        json!({"access_token": "access-1", "expires_in": 3600, "token_type": "Bearer"}),
    )
    .await;
    mock_channels(
        &server,
        "access-1",
        json!([{"id": "UC_SRC", "snippet": {"title": "원본 채널"}}]),
    )
    .await;

    let api_base = server.uri() + "/youtube/v3";
    let login = LoginProcess::spawn(dir.path(), &api_base);
    let (redirect_uri, state) = login.callback_targets();
    complete_callback(&redirect_uri, &state).await;

    let (status, _stdout, stderr) = login.finish();
    assert_eq!(
        status.code(),
        Some(3),
        "인증 오류로 종료해야 한다: {stderr}"
    );
    assert!(stderr.contains("갱신 토큰을 받지 못했습니다"), "{stderr}");
    assert!(
        !dir.path().join("tokens").join("source.json").exists(),
        "refresh token이 없으면 저장하지 않는다"
    );
}

#[tokio::test]
async fn login_without_channel_fails() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().expect("임시 디렉터리");
    write_client_secret(dir.path(), &server);
    mock_token_success(
        &server,
        json!({
            "access_token": "access-1",
            "expires_in": 3600,
            "refresh_token": "refresh-1",
            "token_type": "Bearer",
        }),
    )
    .await;
    mock_channels(&server, "access-1", json!([])).await;

    let api_base = server.uri() + "/youtube/v3";
    let login = LoginProcess::spawn(dir.path(), &api_base);
    let (redirect_uri, state) = login.callback_targets();
    assert_eq!(
        complete_callback(&redirect_uri, &state).await,
        reqwest::StatusCode::OK
    );

    let (status, _stdout, stderr) = login.finish();
    assert_eq!(status.code(), Some(3), "{stderr}");
    assert!(stderr.contains("YouTube 채널이 없습니다"), "{stderr}");
    assert!(!dir.path().join("tokens").join("source.json").exists());
}

#[tokio::test]
async fn login_rejects_state_mismatch_over_http() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().expect("임시 디렉터리");
    write_client_secret(dir.path(), &server);

    let login = LoginProcess::spawn(dir.path(), &(server.uri() + "/youtube/v3"));
    let (redirect_uri, _state) = login.callback_targets();
    assert_eq!(
        complete_callback(&redirect_uri, "forged-state").await,
        reqwest::StatusCode::BAD_REQUEST,
        "state 불일치는 400으로 응답해야 한다"
    );

    let (status, _stdout, stderr) = login.finish();
    assert_eq!(status.code(), Some(3), "{stderr}");
    assert!(stderr.contains("state"), "{stderr}");
    assert!(!dir.path().join("tokens").join("source.json").exists());
}

#[tokio::test]
async fn refresh_token_exchange_updates_store() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "new-access",
            "expires_in": 3600,
            "token_type": "Bearer",
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("임시 디렉터리");
    let paths = Paths::with_root(dir.path());
    let store = FileTokenStore::new(paths.clone());
    // 만료된 토큰을 저장해 갱신을 유도한다.
    store
        .save(
            Account::Target,
            &stored_token(Some(now() - chrono::Duration::minutes(10))),
        )
        .expect("토큰 저장 실패");

    let fixed = now();
    let source = OAuthTokenSource::new(store.clone(), Account::Target)
        .expect("토큰 소스 생성 실패")
        .with_clock(Arc::new(move || fixed))
        .with_token_uri(format!("{}/token", server.uri()));

    let token = source.access_token(false).await.expect("갱신 실패");
    assert_eq!(token, "new-access");

    let requests = server.received_requests().await.expect("요청 기록");
    assert_eq!(requests.len(), 1);
    let body = String::from_utf8_lossy(&requests[0].body).to_string();
    assert!(body.contains("grant_type=refresh_token"), "{body}");
    assert!(body.contains("refresh_token=refresh-1"), "{body}");

    let saved = store
        .load(Account::Target)
        .expect("토큰 로드 실패")
        .expect("토큰이 있어야 한다");
    assert_eq!(saved.access_token.as_deref(), Some("new-access"));
    assert_eq!(
        saved.expires_at,
        Some(now() + chrono::Duration::seconds(3600)),
        "만료 시각을 주입한 시계 기준으로 계산한다"
    );
    assert_eq!(saved.refresh_token, "refresh-1", "refresh token은 유지한다");

    // 신선한 토큰은 다시 요청하지 않는다.
    let cached = source.access_token(false).await.expect("토큰 조회 실패");
    assert_eq!(cached, "new-access");
    assert_eq!(
        server.received_requests().await.expect("요청 기록").len(),
        1
    );
}

#[tokio::test]
async fn refresh_invalid_grant_is_auth_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "invalid_grant",
            "error_description": "Token has been expired or revoked.",
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("임시 디렉터리");
    let paths = Paths::with_root(dir.path());
    let store = FileTokenStore::new(paths);
    store
        .save(
            Account::Target,
            &stored_token(Some(now() - chrono::Duration::minutes(10))),
        )
        .expect("토큰 저장 실패");

    let source = OAuthTokenSource::new(store, Account::Target)
        .expect("토큰 소스 생성 실패")
        .with_token_uri(format!("{}/token", server.uri()));

    let error = source
        .access_token(false)
        .await
        .expect_err("invalid_grant는 인증 오류여야 한다");
    assert!(matches!(error, ApiError::Auth(_)), "{error:?}");
    assert!(error.to_string().contains("다시 실행하세요"), "{error}");
}

#[tokio::test]
async fn forced_refresh_ignores_fresh_access_token() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "second-access",
            "expires_in": 3600,
            "token_type": "Bearer",
        })))
        .mount(&server)
        .await;

    let dir = tempfile::tempdir().expect("임시 디렉터리");
    let paths = Paths::with_root(dir.path());
    let store = FileTokenStore::new(paths);
    store
        .save(
            Account::Target,
            &stored_token(Some(now() + chrono::Duration::hours(1))),
        )
        .expect("토큰 저장 실패");

    let source = OAuthTokenSource::new(store, Account::Target)
        .expect("토큰 소스 생성 실패")
        .with_token_uri(format!("{}/token", server.uri()));

    let token = source.access_token(true).await.expect("강제 갱신 실패");
    assert_eq!(token, "second-access");
    assert_eq!(
        server.received_requests().await.expect("요청 기록").len(),
        1
    );
}

#[tokio::test]
async fn login_without_client_secret_file_reports_path() {
    let dir = tempfile::tempdir().expect("임시 디렉터리");
    let output = std::process::Command::new(assert_cmd::cargo::cargo_bin("stk"))
        .args(["auth", "login", "--account", "source", "--no-browser"])
        .env("STK_HOME", dir.path())
        .output()
        .expect("실행 실패");
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("client_secret.json"), "{stderr}");
    assert!(stderr.contains("docs/setup.md"), "{stderr}");
}
