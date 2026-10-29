//! append-only 실행 journal.

use std::collections::{HashMap, HashSet};
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::AppError;
use crate::migrate::DATA_RETENTION_DAYS;
use crate::migrate::model::DataType;
use crate::paths::{Paths, write_file_atomic};

/// journal 항목 상태.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalStatus {
    /// 성공.
    Success,
    /// 의도적으로 건너뜀(중복, 삭제된 영상 등).
    Skipped,
    /// 실패. 다음 실행에서 재시도한다.
    Failed,
}

impl JournalStatus {
    /// 다시 시도할 필요가 없는 상태인지 확인한다.
    pub fn is_complete(self) -> bool {
        matches!(self, Self::Success | Self::Skipped)
    }
}

/// journal 한 줄.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalEntry {
    /// 액션 키.
    pub key: String,
    /// 상태.
    pub status: JournalStatus,
    /// 기록 시각.
    pub at: DateTime<Utc>,
    /// 부가 정보(대상 플레이리스트 ID, 건너뜀 사유 등).
    #[serde(default)]
    pub detail: Option<Value>,
}

/// `<home>/data/journal.jsonl`에 실행 결과를 남기는 저장소.
///
/// 각 액션마다 한 줄을 덧붙이고 즉시 디스크로 내려 재개 시 신뢰할 수 있게 한다.
#[derive(Debug)]
pub struct Journal {
    path: PathBuf,
    entries: Vec<JournalEntry>,
    complete: HashSet<String>,
    created: HashMap<String, String>,
}

impl Journal {
    /// 기존 journal을 읽어 완료 키를 복원한다.
    ///
    /// 잘못된 줄은 경고 후 건너뛰고, 30일이 지난 항목은 파일에서 제거한다.
    pub fn open(paths: &Paths, now: DateTime<Utc>) -> Result<Self, AppError> {
        let path = paths.journal();
        let mut journal = Self {
            path,
            entries: Vec::new(),
            complete: HashSet::new(),
            created: HashMap::new(),
        };
        let raw = match std::fs::read_to_string(&journal.path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(journal),
            Err(err) => {
                return Err(AppError::io(
                    format!("journal을 읽을 수 없습니다: {}", journal.path.display()),
                    err,
                ));
            }
        };

        let mut skipped_lines = 0usize;
        let mut kept = Vec::new();
        for line in BufReader::new(raw.as_bytes()).lines() {
            let Ok(line) = line else { break };
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<JournalEntry>(&line) {
                Ok(entry) => {
                    if now.signed_duration_since(entry.at)
                        > chrono::Duration::days(DATA_RETENTION_DAYS)
                    {
                        continue;
                    }
                    kept.push(entry);
                }
                Err(_) => skipped_lines += 1,
            }
        }
        if skipped_lines > 0 {
            tracing::warn!(skipped_lines, "journal의 잘못된 줄을 건너뛰었습니다");
        }

        let pruned = kept.len() != raw.lines().filter(|line| !line.trim().is_empty()).count();
        journal.entries = kept;
        journal.rebuild_indexes();
        if pruned {
            journal.rewrite()?;
        }
        Ok(journal)
    }

    fn rebuild_indexes(&mut self) {
        self.complete.clear();
        self.created.clear();
        for entry in &self.entries {
            if entry.status.is_complete() {
                self.complete.insert(entry.key.clone());
            }
            if entry.status == JournalStatus::Success
                && let Some(source) = entry.key.strip_prefix("playlist:")
            {
                let target = entry
                    .detail
                    .as_ref()
                    .and_then(|detail| detail.get("target_playlist_id"))
                    .and_then(|value| value.as_str())
                    .map(str::to_string);
                if let Some(target) = target {
                    self.created.insert(source.to_string(), target);
                }
            }
        }
    }

    fn rewrite(&self) -> Result<(), AppError> {
        let mut payload = String::new();
        for entry in &self.entries {
            let line = serde_json::to_string(entry)
                .map_err(|err| AppError::json("journal 항목을 직렬화할 수 없습니다", err))?;
            payload.push_str(&line);
            payload.push('\n');
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                AppError::io(
                    format!("journal 디렉터리를 만들 수 없습니다: {}", parent.display()),
                    err,
                )
            })?;
        }
        write_file_atomic(&self.path, payload.as_bytes())
    }

    /// 완료(성공/건너뜀)된 적이 있는 키인지 확인한다.
    pub fn is_complete(&self, key: &str) -> bool {
        self.complete.contains(key)
    }

    /// journal에 기록된 완료 키 수.
    pub fn complete_count(&self) -> usize {
        self.complete.len()
    }

    /// 데이터 유형별 완료 키 수(성공/건너뜀).
    ///
    /// 계획이 새로 계산되어 이미 완료한 액션이 plan.json에서 빠져도 누적 진행률을 셀 수 있다.
    pub fn complete_counts(&self) -> std::collections::BTreeMap<DataType, u64> {
        let mut counts: std::collections::BTreeMap<DataType, u64> =
            std::collections::BTreeMap::new();
        for key in &self.complete {
            let data_type = if key.starts_with("subscription:") {
                DataType::Subscriptions
            } else if key.starts_with("playlist:") || key.starts_with("playlist-item:") {
                DataType::Playlists
            } else if key.starts_with("like:") {
                DataType::Likes
            } else {
                continue;
            };
            *counts.entry(data_type).or_insert(0) += 1;
        }
        counts
    }

    /// 이전 실행에서 만든 플레이리스트 매핑(소스 ID → 대상 ID).
    pub fn created_playlists(&self) -> &HashMap<String, String> {
        &self.created
    }

    /// 모든 항목(시간순).
    pub fn entries(&self) -> &[JournalEntry] {
        &self.entries
    }

    /// 항목을 추가하고 파일에 즉시 기록한다.
    pub fn record(
        &mut self,
        key: &str,
        status: JournalStatus,
        detail: Option<Value>,
        now: DateTime<Utc>,
    ) -> Result<(), AppError> {
        let entry = JournalEntry {
            key: key.to_string(),
            status,
            at: now,
            detail,
        };
        let line = serde_json::to_string(&entry)
            .map_err(|err| AppError::json("journal 항목을 직렬화할 수 없습니다", err))?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                AppError::io(
                    format!("journal 디렉터리를 만들 수 없습니다: {}", parent.display()),
                    err,
                )
            })?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|err| {
                AppError::io(
                    format!("journal을 열 수 없습니다: {}", self.path.display()),
                    err,
                )
            })?;
        file.write_all(line.as_bytes())
            .and_then(|()| file.write_all(b"\n"))
            .and_then(|()| file.sync_data())
            .map_err(|err| {
                AppError::io(
                    format!("journal에 기록할 수 없습니다: {}", self.path.display()),
                    err,
                )
            })?;

        self.entries.push(entry);
        self.rebuild_indexes();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-29T12:00:00Z")
            .expect("시각 파싱")
            .with_timezone(&Utc)
    }

    #[test]
    fn journal_resume_skips_completed() {
        let dir = tempfile::tempdir().expect("임시 디렉터리");
        let paths = Paths::with_root(dir.path());
        let mut journal = Journal::open(&paths, now()).expect("journal 열기 실패");
        assert!(!journal.is_complete("subscription:UC_A"));

        journal
            .record("subscription:UC_A", JournalStatus::Success, None, now())
            .expect("기록 실패");
        journal
            .record(
                "playlist-item:PL_A:VID",
                JournalStatus::Skipped,
                Some(Value::String("videoNotFound".to_string())),
                now(),
            )
            .expect("기록 실패");
        journal
            .record("like:VID_X", JournalStatus::Failed, None, now())
            .expect("기록 실패");

        let reopened = Journal::open(&paths, now()).expect("journal 열기 실패");
        assert!(reopened.is_complete("subscription:UC_A"));
        assert!(reopened.is_complete("playlist-item:PL_A:VID"));
        assert!(
            !reopened.is_complete("like:VID_X"),
            "실패 항목은 재시도 대상이다"
        );
        assert_eq!(reopened.complete_count(), 2);
    }

    #[test]
    fn journal_restores_created_playlist_mapping() {
        let dir = tempfile::tempdir().expect("임시 디렉터리");
        let paths = Paths::with_root(dir.path());
        let mut journal = Journal::open(&paths, now()).expect("journal 열기 실패");
        journal
            .record(
                "playlist:PL_SRC",
                JournalStatus::Success,
                Some(serde_json::json!({"target_playlist_id": "PL_NEW"})),
                now(),
            )
            .expect("기록 실패");

        let reopened = Journal::open(&paths, now()).expect("journal 열기 실패");
        assert_eq!(
            reopened
                .created_playlists()
                .get("PL_SRC")
                .map(String::as_str),
            Some("PL_NEW")
        );
    }

    #[test]
    fn journal_skips_corrupt_lines() {
        let dir = tempfile::tempdir().expect("임시 디렉터리");
        let paths = Paths::with_root(dir.path());
        paths.ensure_layout().expect("디렉터리 생성");
        let mut file = std::fs::File::create(paths.journal()).expect("파일 생성");
        writeln!(file, "이건 JSON이 아니다").expect("쓰기");
        writeln!(
            file,
            r#"{{"key":"subscription:UC_A","status":"success","at":"2026-10-29T12:00:00Z","detail":null}}"#
        )
        .expect("쓰기");

        let journal = Journal::open(&paths, now()).expect("journal 열기 실패");
        assert_eq!(journal.complete_count(), 1);
        assert!(journal.is_complete("subscription:UC_A"));
    }

    #[test]
    fn journal_prunes_entries_older_than_30_days() {
        let dir = tempfile::tempdir().expect("임시 디렉터리");
        let paths = Paths::with_root(dir.path());
        let mut journal = Journal::open(&paths, now()).expect("journal 열기 실패");
        journal
            .record(
                "subscription:OLD",
                JournalStatus::Success,
                None,
                now() - chrono::Duration::days(31),
            )
            .expect("기록 실패");
        journal
            .record("subscription:NEW", JournalStatus::Success, None, now())
            .expect("기록 실패");

        let reopened = Journal::open(&paths, now()).expect("journal 열기 실패");
        assert!(!reopened.is_complete("subscription:OLD"));
        assert!(reopened.is_complete("subscription:NEW"));
        let raw = std::fs::read_to_string(paths.journal()).expect("읽기");
        assert!(
            !raw.contains("subscription:OLD"),
            "오래된 줄은 파일에서 제거된다"
        );
    }
}
