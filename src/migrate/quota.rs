//! 태평양 날짜 기준 쓰기 쿼터 장부.

use std::path::PathBuf;

use chrono::{DateTime, NaiveDate, Utc};
use chrono_tz::America::Los_Angeles;
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::migrate::WRITE_QUOTA_UNIT;
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
    /// 쓰기 요청 하나의 단가. Google 콘솔 실측에 맞춰 조정할 수 있다.
    #[serde(default = "default_write_cost")]
    write_cost: u64,
}

fn default_write_cost() -> u64 {
    WRITE_QUOTA_UNIT
}

fn read_ledger(path: &std::path::Path) -> Option<LedgerFile> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
}

/// 장부 파일에서 읽은 값(쓰기 없음).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct QuotaSnapshot {
    /// 태평양 기준 날짜.
    pub date_pt: NaiveDate,
    /// 사용한 쿼터.
    pub used: u64,
    /// 오늘 예산.
    pub limit: u64,
    /// 쓰기 요청 하나의 단가.
    pub write_cost: u64,
}

impl QuotaSnapshot {
    /// 남은 예산.
    pub fn remaining(&self) -> u64 {
        self.limit.saturating_sub(self.used)
    }

    /// 남은 예산으로 처리할 수 있는 쓰기 수.
    pub fn writes_left(&self) -> u64 {
        self.remaining() / self.write_cost.max(1)
    }
}

/// 하루 쓰기 쿼터를 추적하고 사전 예약한다.
///
/// 쓰기는 액션마다 [`QuotaLedger::write_cost`]만큼 예약하고, 예약할 수 없으면
/// [`AppError::Quota`]로 중단한다. 태평양 날짜가 바뀌면 사용량은 0으로 돌아간다.
#[derive(Debug, Clone)]
pub struct QuotaLedger {
    path: PathBuf,
    date_pt: NaiveDate,
    used: u64,
    limit: u64,
    write_cost: u64,
}

impl QuotaLedger {
    /// 장부를 연다. 저장된 단가가 있으면 그 값을 유지한다.
    pub fn open(paths: &Paths, budget: u64, now: DateTime<Utc>) -> Result<Self, AppError> {
        Self::open_with_cost(paths, budget, None, now)
    }

    /// 장부를 열고 단가를 명시적으로 지정한다(없으면 저장값, 그다음 기본 50).
    pub fn open_with_cost(
        paths: &Paths,
        budget: u64,
        write_cost: Option<u64>,
        now: DateTime<Utc>,
    ) -> Result<Self, AppError> {
        let today = pacific_date(now);
        let existing = read_ledger(&paths.quota());
        let same_day = existing.as_ref().filter(|file| file.date_pt == today);
        let used = same_day.map(|file| file.used).unwrap_or(0);
        let write_cost = write_cost
            .or_else(|| same_day.map(|file| file.write_cost))
            .unwrap_or(WRITE_QUOTA_UNIT)
            .max(1);

        let ledger = Self {
            path: paths.quota(),
            date_pt: today,
            used,
            limit: budget.max(1),
            write_cost,
        };
        ledger.save()?;
        Ok(ledger)
    }

    /// 장부를 쓰지 않고 읽기만 한다.
    pub fn peek(paths: &Paths, now: DateTime<Utc>) -> Option<QuotaSnapshot> {
        let file = read_ledger(&paths.quota())?;
        if file.date_pt != pacific_date(now) {
            return None;
        }
        Some(QuotaSnapshot {
            date_pt: file.date_pt,
            used: file.used,
            limit: file.limit,
            write_cost: file.write_cost.max(1),
        })
    }

    fn save(&self) -> Result<(), AppError> {
        let file = LedgerFile {
            date_pt: self.date_pt,
            used: self.used,
            limit: self.limit,
            write_cost: self.write_cost,
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

    /// 쓰기 요청 하나의 단가.
    pub fn write_cost(&self) -> u64 {
        self.write_cost
    }

    /// 남은 예산.
    pub fn remaining(&self) -> u64 {
        self.limit.saturating_sub(self.used)
    }

    /// 남은 예산으로 처리할 수 있는 쓰기 수.
    pub fn writes_left(&self) -> u64 {
        self.remaining() / self.write_cost.max(1)
    }

    /// 태평양 기준 날짜.
    pub fn date_pt(&self) -> NaiveDate {
        self.date_pt
    }

    /// 현재 상태 스냅샷.
    pub fn snapshot(&self) -> QuotaSnapshot {
        QuotaSnapshot {
            date_pt: self.date_pt,
            used: self.used,
            limit: self.limit,
            write_cost: self.write_cost,
        }
    }

    /// Google 콘솔에서 확인한 사용량으로 장부를 맞춘다.
    ///
    /// 단가가 실제와 달라 장부가 어긋났을 때 복구하는 용도다.
    pub fn sync_used(&mut self, used: u64) -> Result<(), AppError> {
        self.used = used;
        self.save()
    }

    /// 쓰기 단가를 바꾼다.
    pub fn set_write_cost(&mut self, write_cost: u64) -> Result<(), AppError> {
        self.write_cost = write_cost.max(1);
        self.save()
    }

    /// API가 쿼터 초과를 돌려줬을 때 장부를 소진 상태로 표시한다.
    ///
    /// 콘솔 값과 장부가 어긋난 경우 다음 실행이 같은 오류를 반복하지 않게 한다.
    pub fn mark_exhausted(&mut self) -> Result<(), AppError> {
        self.used = self.limit;
        self.save()
    }

    /// 예산을 바꾼다.
    pub fn set_limit(&mut self, limit: u64) -> Result<(), AppError> {
        self.limit = limit.max(1);
        self.save()
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

    #[test]
    fn quota_sync_overrides_used_and_persists_cost() {
        let dir = tempfile::tempdir().expect("임시 디렉터리");
        let paths = Paths::with_root(dir.path());
        let now = at("2026-10-29T12:00:00Z");

        let mut ledger = QuotaLedger::open(&paths, 10_000, now).expect("장부 열기 실패");
        ledger.reserve(9_500).expect("예약 실패");
        assert_eq!(ledger.writes_left(), 10);

        // 콘솔 실측값으로 맞추고 단가를 25로 교정한다.
        ledger.sync_used(4_929).expect("사용량 동기화 실패");
        ledger.set_write_cost(25).expect("단가 변경 실패");
        assert_eq!(ledger.used(), 4_929);
        assert_eq!(ledger.remaining(), 5_071);
        assert_eq!(ledger.writes_left(), 202, "5071 / 25");

        // 다른 프로세스도 교정된 단가를 이어받는다.
        let reopened = QuotaLedger::open(&paths, 10_000, now).expect("장부 열기 실패");
        assert_eq!(reopened.write_cost(), 25);
        assert_eq!(reopened.used(), 4_929);

        let snapshot = QuotaLedger::peek(&paths, now).expect("스냅샷");
        assert_eq!(snapshot.writes_left(), 202);
    }

    #[test]
    fn quota_write_cost_changes_daily_capacity() {
        let dir = tempfile::tempdir().expect("임시 디렉터리");
        let paths = Paths::with_root(dir.path());
        let now = at("2026-10-29T12:00:00Z");

        let mut ledger = QuotaLedger::open(&paths, 10_000, now).expect("장부 열기 실패");
        assert_eq!(ledger.writes_left(), 200, "기본 단가 50");
        ledger.set_write_cost(25).expect("단가 변경 실패");
        assert_eq!(ledger.writes_left(), 400, "단가 25면 하루 400건");

        // 단가 25로 예약하면 400번까지 통과한다.
        for _ in 0..400 {
            ledger.reserve(25).expect("예약 실패");
        }
        assert!(ledger.reserve(25).is_err());
    }

    #[test]
    fn peek_returns_none_after_pt_midnight() {
        let dir = tempfile::tempdir().expect("임시 디렉터리");
        let paths = Paths::with_root(dir.path());
        QuotaLedger::open(&paths, 9_500, at("2026-10-29T12:00:00Z"))
            .expect("장부 열기 실패")
            .reserve(50)
            .expect("예약 실패");

        assert!(QuotaLedger::peek(&paths, at("2026-10-29T12:30:00Z")).is_some());
        assert!(
            QuotaLedger::peek(&paths, at("2026-10-31T12:00:00Z")).is_none(),
            "날짜가 바뀌면 이전 장부는 무효다"
        );
    }
}
