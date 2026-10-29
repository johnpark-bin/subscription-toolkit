//! 태평양 날짜 기준 쓰기 쿼터 장부.

use std::path::PathBuf;

use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::America::Los_Angeles;
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::paths::Paths;

/// 장부 파일 형식.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LedgerFile {
    /// 태평양 기준 날짜(`YYYY-MM-DD`).
    date_pt: NaiveDate,
    /// 사용한 쿼터.
    used: u64,
    /// 오늘 예산.
    limit: u64,
}

/// 하루 쓰기 쿼터를 추적하고 사전 예약한다.
///
/// 쓰기는 액션마다 50 units를 예약하고, 예약할 수 없으면 [`AppError::Quota`]로 중단한다.
#[derive(Debug, Clone)]
pub struct QuotaLedger {
    path: PathBuf,
    date_pt: NaiveDate,
    used: u64,
    limit: u64,
}

impl QuotaLedger {
    /// 장부를 연다. 태평양 날짜가 바뀌었으면 사용량을 0으로 되돌린다.
    pub fn open(paths: &Paths, budget: u64, now: DateTime<Utc>) -> Result<Self, AppError> {
        let path = paths.quota();
        let today = pacific_date(now);
        let limit = budget.max(1);
        let existing: Option<LedgerFile> = match std::fs::read_to_string(&path) {
            Ok(raw) => serde_json::from_str(&raw).ok(),
            Err(_) => None,
        };
        let used = match &existing {
            Some(file) if file.date_pt == today => file.used,
            _ => 0,
        };
        let ledger = Self {
            path,
            date_pt: today,
            used,
            limit,
        };
        ledger.save()?;
        Ok(ledger)
    }

    fn save(&self) -> Result<(), AppError> {
        let file = LedgerFile {
            date_pt: self.date_pt,
            used: self.used,
            limit: self.limit,
        };
        let raw = serde_json::to_vec(&file)
            .map_err(|err| AppError::json("쿼터 장부를 직렬화할 수 없습니다", err))?;
        let paths = Paths::with_root(
            self.path
                .parent()
                .unwrap_or_else(|| std::path::Path::new(".")),
        );
        paths.write_atomic(&self.path, &raw)
    }

    /// 오늘 사용한 쿼터.
    pub fn used(&self) -> u64 {
        self.used
    }

    /// 오늘 예산.
    pub fn limit(&self) -> u64 {
        self.limit
    }

    /// 남은 예산.
    pub fn remaining(&self) -> u64 {
        self.limit.saturating_sub(self.used)
    }

    /// 태평양 기준 날짜.
    pub fn date_pt(&self) -> NaiveDate {
        self.date_pt
    }

    /// 예산이 남아 있는지 확인한다.
    pub fn ensure_available(&self, units: u64) -> Result<(), AppError> {
        if self.remaining() < units {
            return Err(AppError::Quota(format!(
                "일일 쿼터 예산을 모두 사용했습니다(사용 {}/{}). 태평양 시간 자정 이후에 다시 실행하면 이어서 진행됩니다.",
                self.used, self.limit
            )));
        }
        Ok(())
    }

    /// 예산을 미리 차감한다. 남은 예산이 부족하면 오류.
    pub fn reserve(&mut self, units: u64) -> Result<(), AppError> {
        self.ensure_available(units)?;
        self.used += units;
        self.save()
    }
}

/// 태평양 기준 날짜.
pub fn pacific_date(now: DateTime<Utc>) -> NaiveDate {
    now.with_timezone(&Los_Angeles).date_naive()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(raw: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(raw)
            .expect("시각 파싱")
            .with_timezone(&Utc)
    }

    #[test]
    fn quota_resets_on_pt_midnight() {
        let dir = tempfile::tempdir().expect("임시 디렉터리");
        let paths = Paths::with_root(dir.path());

        // 2026-10-29 22:00 태평양 = 2026-10-30 05:00 UTC
        let mut ledger =
            QuotaLedger::open(&paths, 9_500, at("2026-10-30T05:00:00Z")).expect("장부 열기 실패");
        ledger.reserve(500).expect("예약 실패");
        assert_eq!(ledger.used(), 500);

        let same_day =
            QuotaLedger::open(&paths, 9_500, at("2026-10-30T06:00:00Z")).expect("장부 열기 실패");
        assert_eq!(same_day.used(), 500, "같은 태평양 날짜는 유지한다");

        let next_day =
            QuotaLedger::open(&paths, 9_500, at("2026-10-31T08:00:00Z")).expect("장부 열기 실패");
        assert_eq!(next_day.used(), 0, "태평양 자정이 지나면 초기화한다");
        assert_eq!(next_day.date_pt().to_string(), "2026-10-31");
    }

    #[test]
    fn quota_blocks_51st_write() {
        let dir = tempfile::tempdir().expect("임시 디렉터리");
        let paths = Paths::with_root(dir.path());
        let mut ledger =
            QuotaLedger::open(&paths, 2_500, at("2026-10-29T12:00:00Z")).expect("장부 열기 실패");

        for index in 0..50 {
            ledger.reserve(50).unwrap_or_else(|err| {
                panic!("{index}번째 예약은 성공해야 한다: {err}");
            });
        }
        assert_eq!(ledger.remaining(), 0);
        let error = ledger.reserve(50).expect_err("51번째 쓰기는 막아야 한다");
        assert_eq!(error.exit_code(), 4);
        assert!(error.to_string().contains("태평양"));
    }

    #[test]
    fn quota_persists_usage_across_processes() {
        let dir = tempfile::tempdir().expect("임시 디렉터리");
        let paths = Paths::with_root(dir.path());
        let now = at("2026-10-29T12:00:00Z");
        QuotaLedger::open(&paths, 9_500, now)
            .expect("장부 열기 실패")
            .reserve(150)
            .expect("예약 실패");

        let reopened = QuotaLedger::open(&paths, 9_500, now).expect("장부 열기 실패");
        assert_eq!(reopened.used(), 150);
        let raw = std::fs::read_to_string(paths.quota()).expect("읽기");
        assert!(raw.contains("\"used\":150"), "{raw}");
    }
}
