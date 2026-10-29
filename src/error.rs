//! 응용 오류와 종료 코드.

/// 명령 실행 중 발생한 오류.
///
/// 종료 코드는 [`AppError::exit_code`]가 결정하며 `README.md`에 문서화되어 있다.
/// `0` 성공(건너뜀 포함), `1` 일반 오류, `2` 사용법 오류, `3` 인증/채널,
/// `4` 쿼터 예산 소진, `5` 부분 실패 또는 재시도 필요.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// 사용법 오류. 종료 코드 2.
    #[error("{0}")]
    Usage(String),
    /// 인증 또는 채널 문제. 종료 코드 3.
    #[error("{0}")]
    Auth(String),
    /// 쿼터 예산 소진. 종료 코드 4.
    #[error("{0}")]
    Quota(String),
    /// 부분 실패 또는 재시도 필요. 종료 코드 5.
    #[error("{0}")]
    Partial(String),
    /// 설정, 파일, 상태 문제. 종료 코드 1.
    #[error("{0}")]
    Config(String),
    /// 입출력 오류. 종료 코드 1.
    #[error("{message}: {source}")]
    Io {
        /// 사용자에게 보여줄 한국어 설명.
        message: String,
        /// 원인.
        #[source]
        source: std::io::Error,
    },
    /// JSON 직렬화/역직렬화 오류. 종료 코드 1.
    #[error("{message}: {source}")]
    Json {
        /// 사용자에게 보여줄 한국어 설명.
        message: String,
        /// 원인.
        #[source]
        source: serde_json::Error,
    },
}

impl AppError {
    /// 입출력 오류를 문맥과 함께 감싼다.
    pub fn io(message: impl Into<String>, source: std::io::Error) -> Self {
        Self::Io {
            message: message.into(),
            source,
        }
    }

    /// JSON 오류를 문맥과 함께 감싼다.
    pub fn json(message: impl Into<String>, source: serde_json::Error) -> Self {
        Self::Json {
            message: message.into(),
            source,
        }
    }

    /// 오류에 대응하는 프로세스 종료 코드.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Usage(_) => 2,
            Self::Auth(_) => 3,
            Self::Quota(_) => 4,
            Self::Partial(_) => 5,
            Self::Config(_) | Self::Io { .. } | Self::Json { .. } => 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_match_contract() {
        assert_eq!(AppError::Usage("x".into()).exit_code(), 2);
        assert_eq!(AppError::Auth("x".into()).exit_code(), 3);
        assert_eq!(AppError::Quota("x".into()).exit_code(), 4);
        assert_eq!(AppError::Partial("x".into()).exit_code(), 5);
        assert_eq!(AppError::Config("x".into()).exit_code(), 1);
        let io = AppError::io("읽기 실패", std::io::Error::other("boom"));
        assert_eq!(io.exit_code(), 1);
        assert_eq!(io.to_string(), "읽기 실패: boom");
    }
}
