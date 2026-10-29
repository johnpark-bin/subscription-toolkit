//! 이전 진행률과 남은 일수 계산.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::migrate::journal::Journal;
use crate::migrate::model::{DataType, Plan};
use crate::migrate::quota::QuotaSnapshot;

/// 데이터 유형별 진행률.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TypeProgress {
    /// 데이터 유형.
    pub data_type: DataType,
    /// 계획된 전체 액션 수.
    pub total: u64,
    /// journal 기준 완료 수.
    pub done: u64,
    /// 남은 수.
    pub pending: u64,
}

/// 이전 진행 상황.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationStatus {
    /// 계획 생성 시각.
    pub plan_generated_at: DateTime<Utc>,
    /// 원본 스냅샷 수집 시각.
    pub snapshot_fetched_at: DateTime<Utc>,
    /// 전체 액션 수.
    pub total_actions: u64,
    /// 완료 액션 수.
    pub done_actions: u64,
    /// 남은 액션 수.
    pub pending_actions: u64,
    /// 쓰기 단가.
    pub write_cost: u64,
    /// 하루 예산(units).
    pub budget: u64,
    /// 남은 액션의 예상 쿼터.
    pub pending_units: u64,
    /// 하루 예산 기준 예상 일수.
    pub pending_days: u64,
    /// 유형별 진행률.
    pub types: Vec<TypeProgress>,
    /// 오늘 쿼터 장부(없으면 날짜가 바뀐 상태).
    pub quota: Option<QuotaSnapshot>,
}

/// 계획과 journal을 비교해 진행 상황을 계산한다.
pub fn build_status(
    plan: &Plan,
    journal: &Journal,
    quota: Option<QuotaSnapshot>,
    write_cost: u64,
    budget: u64,
) -> MigrationStatus {
    let write_cost = write_cost.max(1);
    let mut per_type: BTreeMap<DataType, (u64, u64)> = BTreeMap::new();
    for action in &plan.actions {
        let entry = per_type.entry(action.kind.data_type()).or_default();
        entry.0 += 1;
        if journal.is_complete(&action.key) {
            entry.1 += 1;
        }
    }

    let types: Vec<TypeProgress> = per_type
        .into_iter()
        .map(|(data_type, (total, done))| TypeProgress {
            data_type,
            total,
            done,
            pending: total - done,
        })
        .collect();

    let total_actions: u64 = types.iter().map(|item| item.total).sum();
    let done_actions: u64 = types.iter().map(|item| item.done).sum();
    let pending_actions = total_actions - done_actions;
    let pending_units = pending_actions * write_cost;
    let pending_days = if pending_units == 0 {
        0
    } else {
        pending_units.div_ceil(budget.max(1))
    };

    MigrationStatus {
        plan_generated_at: plan.generated_at,
        snapshot_fetched_at: plan.source.fetched_at,
        total_actions,
        done_actions,
        pending_actions,
        write_cost,
        budget,
        pending_units,
        pending_days,
        types,
        quota,
    }
}

fn format_time(value: DateTime<Utc>) -> String {
    value.format("%Y-%m-%d %H:%M UTC").to_string()
}

impl MigrationStatus {
    /// 사람이 읽는 요약(줄바꿈 포함).
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "진행률: {}/{} 완료 · 남은 {}건\n",
            self.done_actions, self.total_actions, self.pending_actions
        ));
        for item in &self.types {
            out.push_str(&format!(
                "  {}: {}/{} (남은 {})\n",
                item.data_type.label(),
                item.done,
                item.total,
                item.pending
            ));
        }
        out.push_str(&format!(
            "남은 쿼터: {} units (단가 {} units) · 예상 {}일\n",
            self.pending_units, self.write_cost, self.pending_days
        ));
        match &self.quota {
            Some(quota) => out.push_str(&format!(
                "오늘({}): {}/{} units 사용 · 남은 쓰기 {}건\n",
                quota.date_pt,
                quota.used,
                quota.limit,
                quota.writes_left()
            )),
            None => out.push_str(&format!(
                "오늘 장부 없음(태평양 자정 이후 첫 실행 시 초기화) · 예산 {} units\n",
                self.budget
            )),
        }
        out.push_str(&format!(
            "계획 {} · 스냅샷 {}\n",
            format_time(self.plan_generated_at),
            format_time(self.snapshot_fetched_at)
        ));
        out
    }
}
