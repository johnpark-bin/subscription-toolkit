//! OAuth 클라이언트 설정 파일 파싱.

use std::path::Path;

use serde::Deserialize;

use crate::error::AppError;
use crate::paths::restrict_permissions;

/// Google OAuth 데스크톱 클라이언트 설정.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthConfig {
    /// 클라이언트 ID.
    pub client_id: String,
    /// 클라이언트 시크릿. 데스크톱 클라이언트에서는 공개 정보로 취급한다.
    pub client_secret: String,
    /// 인증 엔드포인트.
    pub auth_uri: String,
    /// 토큰 엔드포인트.
    pub token_uri: String,
}

impl OAuthConfig {
    /// Google 인증 엔드포인트 기본값.
    pub const DEFAULT_AUTH_URI: &'static str = "https://accounts.google.com/o/oauth2/v2/auth";
    /// Google 토큰 엔드포인트 기본값.
    pub const DEFAULT_TOKEN_URI: &'static str = "https://oauth2.googleapis.com/token";

    /// JSON 문자열에서 설정을 읽는다.
    ///
    /// Google Cloud 콘솔이 내려주는 `{"installed": {...}}` 형식만 지원한다.
    pub fn from_json_str(raw: &str) -> Result<Self, AppError> {
        #[derive(Deserialize)]
        struct Wrapper {
            installed: Option<Installed>,
            web: Option<serde_json::Value>,
        }

        #[derive(Deserialize)]
        struct Installed {
            client_id: String,
            client_secret: String,
            auth_uri: Option<String>,
            token_uri: Option<String>,
        }

        let wrapper: Wrapper = serde_json::from_str(raw).map_err(|err| {
            AppError::json("OAuth 클라이언트 설정 JSON을 해석할 수 없습니다", err)
        })?;

        let installed = match (wrapper.installed, wrapper.web) {
            (Some(installed), _) => installed,
            (None, Some(_)) => {
                return Err(AppError::Config(
                    "웹 애플리케이션 형식 OAuth 클라이언트입니다. 데스크톱 클라이언트 JSON을 사용하세요."
                        .to_string(),
                ));
            }
            (None, None) => {
                return Err(AppError::Config(
                    "OAuth 클라이언트 설정에 installed 항목이 없습니다. 데스크톱 클라이언트 JSON을 사용하세요."
                        .to_string(),
                ));
            }
        };

        if installed.client_id.trim().is_empty() || installed.client_secret.trim().is_empty() {
            return Err(AppError::Config(
                "OAuth 클라이언트 설정에 client_id 또는 client_secret이 비어 있습니다.".to_string(),
            ));
        }

        Ok(Self {
            client_id: installed.client_id,
            client_secret: installed.client_secret,
            auth_uri: installed
                .auth_uri
                .unwrap_or_else(|| Self::DEFAULT_AUTH_URI.to_string()),
            token_uri: installed
                .token_uri
                .unwrap_or_else(|| Self::DEFAULT_TOKEN_URI.to_string()),
        })
    }

    /// 파일에서 설정을 읽는다.
    ///
    /// 클라이언트 시크릿이 담긴 파일이므로 읽은 뒤 권한을 `0600`으로 맞춘다.
    pub fn load(path: &Path) -> Result<Self, AppError> {
        let raw = std::fs::read_to_string(path).map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                AppError::Config(format!(
                    "OAuth 클라이언트 설정 파일을 찾을 수 없습니다: {}. docs/setup.md를 참고하세요.",
                    path.display()
                ))
            } else {
                AppError::io(
                    format!("OAuth 클라이언트 설정 파일을 읽을 수 없습니다: {}", path.display()),
                    err,
                )
            }
        })?;
        let config = Self::from_json_str(&raw)?;
        restrict_permissions(path, 0o600).map_err(|err| {
            AppError::io(
                format!("설정 파일 권한을 설정할 수 없습니다: {}", path.display()),
                err,
            )
        })?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INSTALLED: &str = r#"{
        "installed": {
            "client_id": "1234.apps.googleusercontent.com",
            "project_id": "demo",
            "auth_uri": "https://accounts.google.com/o/oauth2/auth",
            "token_uri": "https://oauth2.googleapis.com/token",
            "auth_provider_x509_cert_url": "https://www.googleapis.com/oauth2/v1/certs",
            "client_secret": "secret-value",
            "redirect_uris": ["http://localhost"]
        }
    }"#;

    #[test]
    fn config_parses_installed_json() {
        let cfg = OAuthConfig::from_json_str(INSTALLED).expect("설정 파싱 실패");
        assert_eq!(cfg.client_id, "1234.apps.googleusercontent.com");
        assert_eq!(cfg.client_secret, "secret-value");
        assert_eq!(cfg.auth_uri, "https://accounts.google.com/o/oauth2/auth");
        assert_eq!(cfg.token_uri, "https://oauth2.googleapis.com/token");
    }

    #[test]
    fn config_restricts_permissions() {
        let dir = tempfile::tempdir().expect("임시 디렉터리");
        let path = dir.path().join("client_secret.json");
        std::fs::write(&path, INSTALLED).expect("설정 파일 쓰기");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
                .expect("권한 변경");
        }

        OAuthConfig::load(&path).expect("설정 로드 실패");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path)
                .expect("메타데이터")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "클라이언트 시크릿은 0600으로 맞춘다");
        }
    }

    #[test]
    fn config_rejects_web_client() {
        let err = OAuthConfig::from_json_str(r#"{"web": {"client_id": "a"}}"#)
            .expect_err("웹 클라이언트는 거부해야 한다");
        assert_eq!(err.exit_code(), 1);
        assert!(err.to_string().contains("데스크톱 클라이언트"));
    }

    #[test]
    fn config_missing_file_mentions_setup_doc() {
        let err = OAuthConfig::load(Path::new("/nonexistent/client_secret.json"))
            .expect_err("없는 파일은 실패해야 한다");
        let message = err.to_string();
        assert!(message.contains("docs/setup.md"));
        assert!(message.contains("/nonexistent/client_secret.json"));
    }
}
