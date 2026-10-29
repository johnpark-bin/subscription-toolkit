//! Google OAuth 인증, 토큰 저장, 액세스 토큰 공급.

pub mod oauth;
pub mod store;

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{Value, json};

use crate::account::Account;
use crate::error::AppError;
use crate::paths::Paths;
use crate::youtube::{ApiError, ChannelInfo};

pub use oauth::{
    LoginOptions, LoginOutcome, OAuthTokenSource, YOUTUBE_SCOPE, classify_callback, login,
    open_browser,
};
pub use store::{FileTokenStore, StoredToken};

/// 액세스 토큰 공급자.
#[async_trait]
pub trait TokenSource: Send + Sync {
    /// 액세스 토큰을 돌려준다. `force_refresh`가 참이면 갱신을 강제한다.
    async fn access_token(&self, force_refresh: bool) -> Result<String, ApiError>;
}

/// 환경 변수로 주입한 고정 토큰(테스트/개발 전용).
pub struct StaticTokenSource {
    token: String,
}

impl StaticTokenSource {
    /// 고정 토큰으로 소스를 만든다.
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
        }
    }
}

#[async_trait]
impl TokenSource for StaticTokenSource {
    async fn access_token(&self, _force_refresh: bool) -> Result<String, ApiError> {
        Ok(self.token.clone())
    }
}

/// 로그인 직후 채널을 확인하는 최소 인터페이스.
#[async_trait]
pub trait ChannelLookup: Send + Sync {
    /// 주어진 액세스 토큰의 채널. 채널이 없으면 `None`.
    async fn channel_for_token(&self, access_token: &str) -> Result<Option<ChannelInfo>, ApiError>;
}

/// 계정 하나의 인증 상태.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AccountStatus {
    /// 토큰이 저장되어 있는지 여부.
    pub logged_in: bool,
    /// 채널 ID.
    pub channel_id: Option<String>,
    /// 채널 이름.
    pub channel_title: Option<String>,
    /// 갱신 토큰 보유 여부.
    pub has_refresh_token: bool,
    /// 액세스 토큰 만료 시각.
    pub access_token_expires_at: Option<chrono::DateTime<chrono::Utc>>,
    /// 토큰 파일 경로.
    pub token_path: String,
}

impl AccountStatus {
    fn missing(path: String) -> Self {
        Self {
            logged_in: false,
            channel_id: None,
            channel_title: None,
            has_refresh_token: false,
            access_token_expires_at: None,
            token_path: path,
        }
    }

    fn from_token(path: String, token: &StoredToken) -> Self {
        Self {
            logged_in: true,
            channel_id: Some(token.channel_id.clone()),
            channel_title: Some(token.channel_title.clone()),
            has_refresh_token: !token.refresh_token.is_empty(),
            access_token_expires_at: token.expires_at,
            token_path: path,
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "logged_in": self.logged_in,
            "channel_id": self.channel_id,
            "channel_title": self.channel_title,
        })
    }
}

/// 두 계정의 인증 상태.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StatusReport {
    /// 원본 계정.
    pub source: AccountStatus,
    /// 대상 계정.
    pub target: AccountStatus,
}

impl StatusReport {
    /// 계정별 상태를 돌려준다.
    pub fn get(&self, account: Account) -> &AccountStatus {
        match account {
            Account::Source => &self.source,
            Account::Target => &self.target,
        }
    }

    /// `stk auth status --json` 출력.
    pub fn to_json(&self) -> Value {
        json!({
            "source": self.source.to_json(),
            "target": self.target.to_json(),
        })
    }
}

/// 토큰 파일을 읽어 두 계정의 인증 상태를 만든다. 네트워크를 쓰지 않는다.
pub fn status(paths: &Paths) -> Result<StatusReport, AppError> {
    let store = FileTokenStore::new(paths.clone());
    let mut statuses = Vec::new();
    for account in [Account::Source, Account::Target] {
        let path = store.path(account).display().to_string();
        let status = match store.load(account)? {
            Some(token) => AccountStatus::from_token(path, &token),
            None => AccountStatus::missing(path),
        };
        statuses.push(status);
    }
    Ok(StatusReport {
        source: statuses.remove(0),
        target: statuses.remove(0),
    })
}

/// 계정 토큰을 삭제한다. 파일이 있었으면 `true`.
pub fn logout(paths: &Paths, account: Account) -> Result<bool, AppError> {
    FileTokenStore::new(paths.clone()).delete(account)
}

/// 계정에 사용할 토큰 소스를 결정한다.
pub fn resolve_token_source(
    paths: &Paths,
    account: Account,
) -> Result<Box<dyn TokenSource>, AppError> {
    resolve_token_source_from(paths, account, |key| std::env::var(key).ok())
}

/// 환경 변수 조회 함수를 주입해 토큰 소스를 결정한다.
///
/// 계정별 변수(`STK_SOURCE_ACCESS_TOKEN`, `STK_TARGET_ACCESS_TOKEN`)가
/// `STK_ACCESS_TOKEN`보다 우선한다.
pub fn resolve_token_source_from<F>(
    paths: &Paths,
    account: Account,
    lookup: F,
) -> Result<Box<dyn TokenSource>, AppError>
where
    F: Fn(&str) -> Option<String>,
{
    let read = |key: &str| lookup(key).filter(|value| !value.trim().is_empty());
    let account_key = match account {
        Account::Source => "STK_SOURCE_ACCESS_TOKEN",
        Account::Target => "STK_TARGET_ACCESS_TOKEN",
    };
    if let Some(token) = read(account_key).or_else(|| read("STK_ACCESS_TOKEN")) {
        return Ok(Box::new(StaticTokenSource::new(token)));
    }

    let store = FileTokenStore::new(paths.clone());
    if store.load(account)?.is_none() {
        return Err(AppError::Auth(format!(
            "{} 계정 토큰이 없습니다. `stk auth login --account {}`를 실행하세요.",
            account.label(),
            account
        )));
    }
    Ok(Box::new(OAuthTokenSource::new(store, account)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::store::StoredToken;

    fn temp_paths() -> (tempfile::TempDir, Paths) {
        let dir = tempfile::tempdir().expect("임시 디렉터리");
        let paths = Paths::with_root(dir.path());
        (dir, paths)
    }

    #[test]
    fn status_reports_missing_tokens_without_error() {
        let (_dir, paths) = temp_paths();
        let report = status(&paths).expect("상태 조회 실패");
        assert!(!report.source.logged_in);
        assert!(!report.target.logged_in);
        let json = report.to_json();
        assert_eq!(json["source"]["logged_in"], serde_json::json!(false));
        assert_eq!(json["target"]["channel_id"], Value::Null);
    }

    #[test]
    fn status_reads_saved_token() {
        let (_dir, paths) = temp_paths();
        let store = FileTokenStore::new(paths.clone());
        store
            .save(
                Account::Target,
                &StoredToken {
                    client_id: "cid".into(),
                    client_secret: "csecret".into(),
                    refresh_token: "refresh".into(),
                    access_token: Some("access".into()),
                    expires_at: None,
                    channel_id: "UC_DST".into(),
                    channel_title: "대상 채널".into(),
                },
            )
            .expect("저장 실패");

        let report = status(&paths).expect("상태 조회 실패");
        assert!(!report.source.logged_in);
        assert!(report.target.logged_in);
        assert_eq!(report.target.channel_id.as_deref(), Some("UC_DST"));
        assert_eq!(report.target.channel_title.as_deref(), Some("대상 채널"));
        assert!(report.target.has_refresh_token);
    }

    #[test]
    fn logout_removes_token() {
        let (_dir, paths) = temp_paths();
        let store = FileTokenStore::new(paths.clone());
        store
            .save(
                Account::Source,
                &StoredToken {
                    client_id: "cid".into(),
                    client_secret: "csecret".into(),
                    refresh_token: "refresh".into(),
                    access_token: None,
                    expires_at: None,
                    channel_id: "UC_SRC".into(),
                    channel_title: "원본 채널".into(),
                },
            )
            .expect("저장 실패");

        assert!(logout(&paths, Account::Source).expect("로그아웃 실패"));
        assert!(!logout(&paths, Account::Source).expect("로그아웃 실패"));
        assert!(!status(&paths).expect("상태 조회 실패").source.logged_in);
    }

    #[test]
    fn resolve_token_source_prefers_account_env() {
        let (_dir, paths) = temp_paths();
        let source = resolve_token_source_from(&paths, Account::Source, |key| match key {
            "STK_ACCESS_TOKEN" => Some("shared".to_string()),
            "STK_SOURCE_ACCESS_TOKEN" => Some("specific".to_string()),
            _ => None,
        })
        .expect("토큰 소스 결정 실패");
        let token = tokio::runtime::Runtime::new()
            .expect("런타임")
            .block_on(source.access_token(false))
            .expect("토큰 조회 실패");
        assert_eq!(token, "specific");

        let shared = resolve_token_source_from(&paths, Account::Target, |key| {
            (key == "STK_ACCESS_TOKEN").then(|| "shared".to_string())
        })
        .expect("토큰 소스 결정 실패");
        let token = tokio::runtime::Runtime::new()
            .expect("런타임")
            .block_on(shared.access_token(false))
            .expect("토큰 조회 실패");
        assert_eq!(token, "shared");
    }

    #[test]
    fn resolve_token_source_without_token_is_auth_error() {
        let (_dir, paths) = temp_paths();
        let error = resolve_token_source_from(&paths, Account::Target, |_| None)
            .err()
            .expect("토큰이 없으면 실패해야 한다");
        assert_eq!(error.exit_code(), 3);
    }
}
