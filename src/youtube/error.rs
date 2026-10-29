//! YouTube Data API 오류.

/// YouTube API 호출에서 발생한 오류.
///
/// `reason`(예: `quotaExceeded`)은 원인 코드를 그대로 유지한다.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// 인증이 필요하거나 토큰이 만료되었다.
    #[error("인증이 필요합니다: {0}")]
    Auth(String),
    /// 일일 쿼터 소진.
    #[error("일일 쿼터를 초과했습니다")]
    QuotaExceeded,
    /// 요청 제한(일시적).
    #[error("요청이 제한되었습니다: {0}")]
    RateLimited(String),
    /// 이미 존재하는 리소스(중복 구독 등).
    #[error("이미 존재합니다: {0}")]
    AlreadyExists(String),
    /// 리소스를 찾을 수 없다(삭제/비공개 영상 등).
    #[error("대상을 찾을 수 없습니다: {0}")]
    NotFound(String),
    /// 허용되지 않은 작업(구독 한도, 이메일 미인증 등).
    #[error("허용되지 않은 작업입니다: {0}")]
    Forbidden(String),
    /// 플레이리스트 항목 상한 초과.
    #[error("플레이리스트가 가득 찼습니다")]
    PlaylistFull,
    /// 지원되지 않는 플레이리스트 작업(Watch Later 등).
    #[error("지원되지 않는 플레이리스트 작업입니다: {0}")]
    Unsupported(String),
    /// 수동 정렬이 필요해 위치를 지정할 수 없다.
    #[error("수동 정렬이 필요합니다")]
    ManualSortRequired,
    /// 잠시 후 다시 시도해야 한다.
    #[error("나중에 다시 시도해야 합니다: {0}")]
    RetryLater(String),
    /// HTTP 상태 오류(매핑되지 않은 응답).
    #[error("HTTP {status}: {body}")]
    Http {
        /// HTTP 상태 코드.
        status: u16,
        /// 응답 본문(요약).
        body: String,
    },
    /// 네트워크 오류.
    #[error("네트워크 오류: {0}")]
    Network(String),
    /// 응답 해석 실패.
    #[error("응답 해석 실패: {0}")]
    Decode(String),
}

impl ApiError {
    /// 이 오류가 인증 문제인지 여부.
    pub fn is_auth(&self) -> bool {
        matches!(self, Self::Auth(_))
    }

    /// 이 오류가 쿼터 소진인지 여부.
    pub fn is_quota(&self) -> bool {
        matches!(self, Self::QuotaExceeded)
    }

    /// 구독 한도(최대 구독 수) 도달 오류인지 여부.
    pub fn is_subscription_limit(&self) -> bool {
        matches!(self, Self::Forbidden(message) if message.contains("maximum number of subscriptions"))
    }
}
