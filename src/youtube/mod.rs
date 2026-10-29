//! YouTube Data API v3 클라이언트.

pub mod error;

pub use error::ApiError;

use serde::{Deserialize, Serialize};

/// 채널 식별 정보.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelInfo {
    /// 채널 ID(`UC...`).
    pub id: String,
    /// 채널 표시 이름.
    pub title: String,
}
