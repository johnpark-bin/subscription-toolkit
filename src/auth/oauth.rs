//! OAuth 2.0 데스크톱 로그인 흐름과 액세스 토큰 갱신.

use std::collections::HashMap;
use std::process::Command;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use oauth2::basic::{BasicClient, BasicErrorResponseType};
use oauth2::reqwest;
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, PkceCodeChallenge, RefreshToken,
    Scope, TokenResponse, TokenUrl,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use url::Url;

use crate::account::Account;
use crate::auth::store::{FileTokenStore, StoredToken};
use crate::auth::{ChannelLookup, TokenSource};
use crate::config::OAuthConfig;
use crate::error::AppError;
use crate::youtube::ApiError;

/// YouTube Data API 쓰기 권한 스코프.
pub const YOUTUBE_SCOPE: &str = "https://www.googleapis.com/auth/youtube.force-ssl";

/// 로그인 옵션.
#[derive(Debug, Clone, Default)]
pub struct LoginOptions {
    /// 브라우저를 열지 않고 주소만 출력한다.
    pub no_browser: bool,
    /// 로그인 화면에서 미리 채울 계정 이메일.
    pub login_hint: Option<String>,
}

/// 로그인 결과 요약.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginOutcome {
    /// 인증된 채널 ID.
    pub channel_id: String,
    /// 인증된 채널 이름.
    pub channel_title: String,
    /// 토큰 파일 경로.
    pub token_path: String,
}

/// 루프백 콜백 요청 분류 결과.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Callback {
    /// 인증 코드를 받았다.
    Code(String),
    /// 처리 대상이 아닌 요청.
    Ignore,
    /// 사용자가 동의를 거부했거나 오류가 돌아왔다.
    Denied(String),
    /// state 값이 일치하지 않는다.
    StateMismatch,
}

/// 콜백 요청 라인의 경로/쿼리를 분류한다.
pub fn classify_callback(target: &str, expected_state: &str) -> Callback {
    let Ok(url) = Url::parse(&format!("http://127.0.0.1{target}")) else {
        return Callback::Ignore;
    };
    if url.path() != "/" {
        return Callback::Ignore;
    }
    let params: HashMap<String, String> = url.query_pairs().into_owned().collect();
    if let Some(error) = params.get("error") {
        return Callback::Denied(error.clone());
    }
    match (params.get("code"), params.get("state")) {
        (Some(code), Some(state)) if state == expected_state => Callback::Code(code.clone()),
        (Some(_), _) => Callback::StateMismatch,
        _ => Callback::Ignore,
    }
}

/// 사용자 브라우저를 열어 인증 페이지를 띄운다. 성공하면 `true`.
pub fn open_browser(url: &str) -> bool {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(url);
        command
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", "", url]);
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(url);
        command
    };

    command.spawn().is_ok()
}

fn build_http() -> Result<reqwest::Client, AppError> {
    reqwest::ClientBuilder::new()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|err| AppError::Config(format!("OAuth HTTP 클라이언트를 만들 수 없습니다: {err}")))
}

/// 루프백 OAuth 로그인을 수행하고 토큰을 저장한다.
///
/// `probe`는 방금 받은 액세스 토큰으로 채널을 확인하는 데 쓰인다. 채널이 없으면
/// 토큰을 저장하지 않고 중단한다.
pub async fn login(
    account: Account,
    config: &OAuthConfig,
    store: &FileTokenStore,
    options: &LoginOptions,
    probe: &dyn ChannelLookup,
) -> Result<LoginOutcome, AppError> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|err| AppError::io("로컬 콜백 서버를 열 수 없습니다", err))?;
    let port = listener
        .local_addr()
        .map_err(|err| AppError::io("로컬 콜백 서버 주소를 확인할 수 없습니다", err))?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}");

    let client = BasicClient::new(ClientId::new(config.client_id.clone()))
        .set_client_secret(ClientSecret::new(config.client_secret.clone()))
        .set_auth_uri(AuthUrl::new(config.auth_uri.clone()).map_err(|err| {
            AppError::Config(format!("인증 엔드포인트가 올바르지 않습니다: {err}"))
        })?)
        .set_token_uri(TokenUrl::new(config.token_uri.clone()).map_err(|err| {
            AppError::Config(format!("토큰 엔드포인트가 올바르지 않습니다: {err}"))
        })?)
        .set_redirect_uri(
            oauth2::RedirectUrl::new(redirect_uri.clone()).map_err(|err| {
                AppError::Config(format!("리다이렉트 URL이 올바르지 않습니다: {err}"))
            })?,
        );

    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();
    let mut request = client
        .authorize_url(CsrfToken::new_random)
        .add_scope(Scope::new(YOUTUBE_SCOPE.to_string()))
        .add_extra_param("access_type", "offline")
        .add_extra_param("include_granted_scopes", "true")
        .add_extra_param("prompt", "consent")
        .set_pkce_challenge(pkce_challenge);
    if let Some(hint) = options
        .login_hint
        .as_deref()
        .filter(|hint| !hint.trim().is_empty())
    {
        request = request.add_extra_param("login_hint", hint.to_string());
    }
    let (authorize_url, csrf_state) = request.url();

    if options.no_browser || !open_browser(authorize_url.as_str()) {
        eprintln!("브라우저에서 다음 주소를 열어 로그인하세요:\n{authorize_url}\n");
    } else {
        eprintln!(
            "브라우저에서 로그인을 완료하세요. 자동으로 열리지 않으면 다음 주소를 사용하세요:\n{authorize_url}\n"
        );
    }

    let code = wait_for_callback(listener, csrf_state.secret()).await?;

    let http = build_http()?;
    let token_response = client
        .exchange_code(AuthorizationCode::new(code))
        .set_pkce_verifier(pkce_verifier)
        .request_async(&http)
        .await
        .map_err(|err| {
            AppError::Auth(format!("인증 코드를 토큰으로 교환하지 못했습니다: {err}"))
        })?;

    let refresh_token = token_response.refresh_token().ok_or_else(|| {
        AppError::Auth(
            "갱신 토큰을 받지 못했습니다. Google 계정에서 앱 권한을 제거한 뒤 다시 동의해 주세요."
                .to_string(),
        )
    })?;
    let access_token = token_response.access_token().secret().clone();
    let expires_at = token_response
        .expires_in()
        .and_then(|duration| chrono::Duration::from_std(duration).ok())
        .map(|duration| Utc::now() + duration);

    let channel = probe
        .channel_for_token(&access_token)
        .await
        .map_err(|err| AppError::Auth(format!("채널을 확인하지 못했습니다: {err}")))?;
    let Some(channel) = channel else {
        return Err(AppError::Auth(format!(
            "{} 계정에 YouTube 채널이 없습니다. youtube.com에서 채널을 먼저 만든 뒤 다시 로그인하세요.",
            account.label()
        )));
    };

    let stored = StoredToken {
        client_id: config.client_id.clone(),
        client_secret: config.client_secret.clone(),
        refresh_token: refresh_token.secret().clone(),
        access_token: Some(access_token),
        expires_at,
        channel_id: channel.id.clone(),
        channel_title: channel.title.clone(),
    };
    store.save(account, &stored)?;

    Ok(LoginOutcome {
        channel_id: channel.id,
        channel_title: channel.title,
        token_path: store.path(account).display().to_string(),
    })
}

async fn respond<W>(stream: &mut W, status: &str, message: &str)
where
    W: AsyncWriteExt + Unpin,
{
    let body = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>subscription-toolkit</title><p>{message}</p>"
    );
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.flush().await;
}

/// 루프백 콜백에서 인증 코드를 기다린다.
///
/// 최대 10개의 요청을 읽고, `/` 경로에 `code`와 `state`가 있는 요청만 처리한다.
async fn wait_for_callback(
    listener: TcpListener,
    expected_state: &str,
) -> Result<String, AppError> {
    for _ in 0..10 {
        let (stream, _) = listener
            .accept()
            .await
            .map_err(|err| AppError::io("콜백 연결을 받을 수 없습니다", err))?;

        let (read_half, mut stream) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut line = String::new();
        let _ = reader.read_line(&mut line).await;
        drop(reader);

        let target = line.split_whitespace().nth(1).unwrap_or("/").to_string();
        match classify_callback(&target, expected_state) {
            Callback::Code(code) => {
                respond(
                    &mut stream,
                    "200 OK",
                    "인증이 완료되었습니다. 터미널 창으로 돌아가세요.",
                )
                .await;
                return Ok(code);
            }
            Callback::Ignore => {
                respond(&mut stream, "404 Not Found", "잘못된 요청입니다.").await;
            }
            Callback::Denied(reason) => {
                respond(&mut stream, "400 Bad Request", "동의가 취소되었습니다.").await;
                return Err(AppError::Auth(format!(
                    "Google 인증이 취소되었습니다({reason}). 다시 실행해 주세요."
                )));
            }
            Callback::StateMismatch => {
                respond(
                    &mut stream,
                    "400 Bad Request",
                    "state 값이 일치하지 않습니다.",
                )
                .await;
                return Err(AppError::Auth(
                    "OAuth state 값이 일치하지 않습니다. 보안을 위해 로그인을 중단합니다."
                        .to_string(),
                ));
            }
        }
    }
    Err(AppError::Auth(
        "인증 콜백을 받지 못했습니다. `stk auth login`을 다시 실행하세요.".to_string(),
    ))
}

fn map_token_error<E>(
    error: oauth2::RequestTokenError<E, oauth2::basic::BasicErrorResponse>,
) -> ApiError
where
    E: std::error::Error + 'static,
{
    match error {
        oauth2::RequestTokenError::ServerResponse(response)
            if matches!(response.error(), BasicErrorResponseType::InvalidGrant) =>
        {
            ApiError::Auth(
                "갱신 토큰이 만료되었거나 취소되었습니다. `stk auth login`을 다시 실행하세요."
                    .to_string(),
            )
        }
        oauth2::RequestTokenError::ServerResponse(response) => ApiError::Auth(format!(
            "토큰 서버가 오류를 반환했습니다: {}",
            response.error()
        )),
        other => ApiError::Network(format!("토큰 요청에 실패했습니다: {other}")),
    }
}

/// 토큰 파일에 저장된 갱신 토큰으로 액세스 토큰을 발급한다.
pub struct OAuthTokenSource {
    store: FileTokenStore,
    account: Account,
    http: reqwest::Client,
    clock: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
}

impl OAuthTokenSource {
    /// 저장소와 계정으로 토큰 소스를 만든다.
    pub fn new(store: FileTokenStore, account: Account) -> Result<Self, AppError> {
        Ok(Self {
            store,
            account,
            http: build_http()?,
            clock: Arc::new(Utc::now),
        })
    }

    /// 테스트용 시계 주입.
    pub fn with_clock(mut self, clock: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>) -> Self {
        self.clock = clock;
        self
    }

    async fn refresh(&self, stored: &StoredToken, now: DateTime<Utc>) -> Result<String, ApiError> {
        let client = BasicClient::new(ClientId::new(stored.client_id.clone()))
            .set_client_secret(ClientSecret::new(stored.client_secret.clone()))
            .set_token_uri(
                TokenUrl::new(OAuthConfig::DEFAULT_TOKEN_URI.to_string())
                    .map_err(|err| ApiError::Decode(err.to_string()))?,
            );

        let response = client
            .exchange_refresh_token(&RefreshToken::new(stored.refresh_token.clone()))
            .request_async(&self.http)
            .await
            .map_err(map_token_error)?;

        let access_token = response.access_token().secret().clone();
        let expires_at = response
            .expires_in()
            .and_then(|duration| chrono::Duration::from_std(duration).ok())
            .map(|duration| now + duration);

        let mut updated = stored.clone();
        updated.access_token = Some(access_token.clone());
        updated.expires_at = expires_at;
        self.store
            .save(self.account, &updated)
            .map_err(|err| ApiError::Auth(format!("갱신한 토큰을 저장할 수 없습니다: {err}")))?;
        Ok(access_token)
    }
}

#[async_trait]
impl TokenSource for OAuthTokenSource {
    async fn access_token(&self, force_refresh: bool) -> Result<String, ApiError> {
        let stored = self
            .store
            .load(self.account)
            .map_err(|err| ApiError::Auth(err.to_string()))?
            .ok_or_else(|| {
                ApiError::Auth(format!(
                    "{} 계정 토큰이 없습니다. `stk auth login --account {}`를 실행하세요.",
                    self.account.label(),
                    self.account
                ))
            })?;

        let now = (self.clock)();
        if !force_refresh && let Some(token) = stored.fresh_access_token(now) {
            return Ok(token.to_string());
        }
        self.refresh(&stored, now).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Paths;
    use crate::youtube::ChannelInfo;

    fn sample_config() -> OAuthConfig {
        OAuthConfig {
            client_id: "cid.apps.googleusercontent.com".to_string(),
            client_secret: "csecret".to_string(),
            auth_uri: "https://accounts.google.com/o/oauth2/v2/auth".to_string(),
            token_uri: "https://oauth2.googleapis.com/token".to_string(),
        }
    }

    struct FakeProbe {
        channel: Option<ChannelInfo>,
    }

    #[async_trait]
    impl ChannelLookup for FakeProbe {
        async fn channel_for_token(&self, token: &str) -> Result<Option<ChannelInfo>, ApiError> {
            assert_eq!(token, "access-1", "방금 받은 액세스 토큰으로 확인해야 한다");
            Ok(self.channel.clone())
        }
    }

    fn login_url_for_test() -> Url {
        // 실제 login()은 브라우저 상호작용이 필요해 URL 생성 규칙만 직접 검증한다.
        let (pkce_challenge, _verifier) = PkceCodeChallenge::new_random_sha256();
        let client = BasicClient::new(ClientId::new("cid".to_string()))
            .set_client_secret(ClientSecret::new("csecret".to_string()))
            .set_auth_uri(AuthUrl::new(sample_config().auth_uri).expect("auth uri"))
            .set_token_uri(TokenUrl::new(sample_config().token_uri).expect("token uri"))
            .set_redirect_uri(
                oauth2::RedirectUrl::new("http://127.0.0.1:8123".to_string()).expect("redirect"),
            );
        let (url, _state) = client
            .authorize_url(CsrfToken::new_random)
            .add_scope(Scope::new(YOUTUBE_SCOPE.to_string()))
            .add_extra_param("access_type", "offline")
            .add_extra_param("include_granted_scopes", "true")
            .add_extra_param("prompt", "consent")
            .set_pkce_challenge(pkce_challenge)
            .url();
        url
    }

    #[test]
    fn oauth_url_contains_pkce_and_scope() {
        let url = login_url_for_test();
        let params: HashMap<String, String> = url.query_pairs().into_owned().collect();
        assert_eq!(
            params.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        assert!(params.get("code_challenge").is_some_and(|v| !v.is_empty()));
        assert_eq!(params.get("scope").map(String::as_str), Some(YOUTUBE_SCOPE));
        assert_eq!(
            params.get("access_type").map(String::as_str),
            Some("offline")
        );
        assert_eq!(params.get("prompt").map(String::as_str), Some("consent"));
        assert_eq!(
            params.get("redirect_uri").map(String::as_str),
            Some("http://127.0.0.1:8123")
        );
        assert!(params.contains_key("state"));
    }

    #[test]
    fn classify_callback_accepts_matching_state() {
        assert_eq!(
            classify_callback("/?code=abc&state=xyz", "xyz"),
            Callback::Code("abc".to_string())
        );
    }

    #[tokio::test]
    async fn login_rejects_state_mismatch() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("바인드 실패");
        let address = listener.local_addr().expect("주소");
        let server = tokio::spawn(async move { wait_for_callback(listener, "expected").await });

        let mut stream = tokio::net::TcpStream::connect(address)
            .await
            .expect("연결 실패");
        stream
            .write_all(b"GET /?code=abc&state=forged HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
            .await
            .expect("전송 실패");

        let error = server
            .await
            .expect("작업 패닉")
            .expect_err("state 불일치는 실패해야 한다");
        assert_eq!(error.exit_code(), 3);
        assert!(error.to_string().contains("state"));
    }

    #[tokio::test]
    async fn login_ignores_unrelated_requests_then_accepts_code() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("바인드 실패");
        let address = listener.local_addr().expect("주소");
        let server = tokio::spawn(async move { wait_for_callback(listener, "expected").await });

        let mut first = tokio::net::TcpStream::connect(address)
            .await
            .expect("연결 실패");
        first
            .write_all(b"GET /favicon.ico HTTP/1.1\r\n\r\n")
            .await
            .expect("전송 실패");
        drop(first);

        let mut second = tokio::net::TcpStream::connect(address)
            .await
            .expect("연결 실패");
        second
            .write_all(b"GET /?code=good&state=expected HTTP/1.1\r\n\r\n")
            .await
            .expect("전송 실패");

        assert_eq!(
            server.await.expect("작업 패닉").expect("코드 수신 실패"),
            "good"
        );
    }

    #[test]
    fn oauth_token_source_requires_login_when_missing() {
        let dir = tempfile::tempdir().expect("임시 디렉터리");
        let paths = Paths::with_root(dir.path());
        let store = FileTokenStore::new(paths);
        let source = OAuthTokenSource::new(store, Account::Target).expect("소스 생성 실패");
        let error = tokio::runtime::Runtime::new()
            .expect("런타임")
            .block_on(source.access_token(false))
            .expect_err("토큰이 없으면 실패해야 한다");
        assert!(matches!(error, ApiError::Auth(_)));
        assert!(
            error
                .to_string()
                .contains("stk auth login --account target")
        );
    }

    // FakeProbe가 실제로 쓰이는 경로(채널 확인)를 문서화하기 위한 컴파일 검증용 사용처.
    #[tokio::test]
    async fn probe_without_channel_returns_none() {
        let probe = FakeProbe { channel: None };
        assert!(
            probe
                .channel_for_token("access-1")
                .await
                .expect("호출")
                .is_none()
        );
    }
}
