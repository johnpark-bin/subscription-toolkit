//! 계정별 OAuth 토큰 파일 저장.

use std::fs;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::account::Account;
use crate::error::AppError;
use crate::paths::Paths;

/// 디스크에 저장하는 인증 정보.
///
/// 데스크톱 OAuth 클라이언트의 시크릿은 공개 정보로 취급하므로 토큰과 함께 저장해
/// `plan`/`push`가 별도 설정 파일 없이 액세스 토큰을 갱신할 수 있게 한다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredToken {
    /// OAuth 클라이언트 ID.
    pub client_id: String,
    /// OAuth 클라이언트 시크릿.
    pub client_secret: String,
    /// 갱신 토큰.
    pub refresh_token: String,
    /// 마지막으로 발급받은 액세스 토큰.
    pub access_token: Option<String>,
    /// 액세스 토큰 만료 시각.
    pub expires_at: Option<DateTime<Utc>>,
    /// 인증된 계정의 YouTube 채널 ID.
    pub channel_id: String,
    /// 인증된 계정의 YouTube 채널 이름.
    pub channel_title: String,
}

impl StoredToken {
    /// 만료 60초 전까지 유효한 액세스 토큰을 돌려준다.
    pub fn fresh_access_token(&self, now: DateTime<Utc>) -> Option<&str> {
        let expires_at = self.expires_at?;
        if expires_at > now + chrono::Duration::seconds(60) {
            self.access_token.as_deref()
        } else {
            None
        }
    }
}

/// `<home>/tokens/<account>.json` 파일 저장소.
#[derive(Debug, Clone)]
pub struct FileTokenStore {
    paths: Paths,
}

impl FileTokenStore {
    /// 경로 묶음으로 저장소를 만든다.
    pub fn new(paths: Paths) -> Self {
        Self { paths }
    }

    /// 계정 토큰 파일 경로.
    pub fn path(&self, account: Account) -> PathBuf {
        self.paths.token(account)
    }

    /// 토큰을 읽는다. 파일이 없으면 `None`.
    ///
    /// 기존 파일의 권한이 `0600`이 아니면 고친다.
    pub fn load(&self, account: Account) -> Result<Option<StoredToken>, AppError> {
        let path = self.path(account);
        let raw = match fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(AppError::io(
                    format!("토큰 파일을 읽을 수 없습니다: {}", path.display()),
                    err,
                ));
            }
        };
        self.paths.restrict_file(&path)?;
        let token = serde_json::from_str(&raw).map_err(|err| {
            AppError::json(
                format!("토큰 파일을 해석할 수 없습니다: {}", path.display()),
                err,
            )
        })?;
        Ok(Some(token))
    }

    /// 토큰을 `0600` 권한으로 저장한다.
    pub fn save(&self, account: Account, token: &StoredToken) -> Result<(), AppError> {
        let path = self.path(account);
        let raw = serde_json::to_vec_pretty(token)
            .map_err(|err| AppError::json("토큰을 직렬화할 수 없습니다", err))?;
        self.paths.write_atomic(&path, &raw)
    }

    /// 토큰 파일을 지운다. 파일이 있었으면 `true`.
    pub fn delete(&self, account: Account) -> Result<bool, AppError> {
        let path = self.path(account);
        match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(AppError::io(
                format!("토큰 파일을 삭제할 수 없습니다: {}", path.display()),
                err,
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_token() -> StoredToken {
        StoredToken {
            client_id: "cid".to_string(),
            client_secret: "csecret".to_string(),
            refresh_token: "refresh".to_string(),
            access_token: Some("access".to_string()),
            expires_at: Some(Utc::now() + chrono::Duration::hours(1)),
            channel_id: "UC_TEST".to_string(),
            channel_title: "테스트 채널".to_string(),
        }
    }

    #[test]
    fn token_file_permissions_0600() {
        let dir = tempfile::tempdir().expect("임시 디렉터리 생성 실패");
        let paths = Paths::with_root(dir.path());
        let store = FileTokenStore::new(paths.clone());
        let token = sample_token();

        store.save(Account::Source, &token).expect("저장 실패");
        let path = store.path(Account::Source);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("권한 변경 실패");
            assert_eq!(
                fs::metadata(&path)
                    .expect("메타데이터")
                    .permissions()
                    .mode()
                    & 0o777,
                0o644
            );

            let loaded = store.load(Account::Source).expect("로드 실패");
            assert_eq!(loaded.as_ref(), Some(&token));

            assert_eq!(
                fs::metadata(&path)
                    .expect("메타데이터")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600,
                "로드 시 토큰 파일 권한을 0600으로 맞춰야 한다"
            );
        }

        assert!(store.delete(Account::Source).expect("삭제 실패"));
        assert!(!store.delete(Account::Source).expect("삭제 실패"));
        assert!(store.load(Account::Source).expect("로드 실패").is_none());
    }

    #[test]
    fn fresh_access_token_requires_margin() {
        let now = Utc::now();
        let mut token = sample_token();
        token.expires_at = Some(now + chrono::Duration::seconds(30));
        token.access_token = Some("short".to_string());
        assert!(token.fresh_access_token(now).is_none());
        token.expires_at = Some(now + chrono::Duration::minutes(10));
        assert_eq!(token.fresh_access_token(now), Some("short"));
    }
}
