//! 인증 대상 계정 구분.

use std::fmt;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

/// 이전 작업에서 사용하는 두 계정.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, ValueEnum, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Account {
    /// 원본 계정(데이터를 읽기만 한다).
    Source,
    /// 대상 계정(데이터를 쓴다).
    Target,
}

impl Account {
    /// 파일 이름 등에 쓰는 소문자 식별자.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Source => "source",
            Self::Target => "target",
        }
    }

    /// 사람이 읽는 한국어 이름.
    pub fn label(self) -> &'static str {
        match self {
            Self::Source => "원본",
            Self::Target => "대상",
        }
    }
}

impl fmt::Display for Account {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_serializes_to_lowercase() {
        let json = serde_json::to_string(&[Account::Source, Account::Target]).expect("직렬화 실패");
        assert_eq!(json, r#"["source","target"]"#);
        let parsed: Vec<Account> = serde_json::from_str(&json).expect("역직렬화 실패");
        assert_eq!(parsed, vec![Account::Source, Account::Target]);
    }
}
