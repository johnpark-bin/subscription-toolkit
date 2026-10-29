//! Google Takeout 시청기록 탐색과 정규화.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::error::AppError;
use crate::paths::Paths;

/// 파싱을 허용하는 최대 파일 크기(512MB).
pub const MAX_FILE_BYTES: u64 = 512 * 1024 * 1024;

/// 디렉터리 탐색 최대 깊이.
const MAX_DEPTH: usize = 6;

/// 정규화한 시청기록 한 건.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// 영상 ID. 추출하지 못하면 `None`.
    pub video_id: Option<String>,
    /// 영상 제목(`Watched ` 접두사 제거).
    pub title: String,
    /// 채널 이름.
    pub channel_name: Option<String>,
    /// 채널 ID.
    pub channel_id: Option<String>,
    /// 시청 시각.
    pub watched_at: Option<DateTime<Utc>>,
    /// Takeout 항목의 `header`(YouTube, YouTube Music 등).
    pub header: String,
    /// 광고 시청 항목 여부.
    pub is_ad: bool,
}

/// 정규화한 시청기록 파일.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryFile {
    /// 가져온 시각.
    pub imported_at: DateTime<Utc>,
    /// 원본 경로(또는 zip 항목 이름).
    pub source_file: String,
    /// 시청 이벤트.
    pub entries: Vec<HistoryEntry>,
}

#[derive(Debug, Deserialize)]
struct RawEntry {
    header: Option<String>,
    title: Option<String>,
    #[serde(rename = "titleUrl")]
    title_url: Option<String>,
    subtitles: Option<Vec<RawSubtitle>>,
    time: Option<String>,
    details: Option<Vec<RawDetail>>,
}

#[derive(Debug, Deserialize)]
struct RawSubtitle {
    name: Option<String>,
    url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawDetail {
    name: Option<String>,
}

fn is_history_filename(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("watch-history.json"))
}

fn collect_candidates(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.is_dir() {
            collect_candidates(&path, depth + 1, out);
        } else if is_history_filename(&path) {
            out.push(path);
        }
    }
}

fn single_candidate(
    mut candidates: Vec<String>,
    root: &Path,
    label: &str,
) -> Result<String, AppError> {
    candidates.sort();
    candidates.dedup();
    match candidates.len() {
        0 => Err(AppError::Config(format!(
            "Takeout에서 watch-history.json을 찾을 수 없습니다: {}. 경로가 맞는지 확인하세요.",
            root.display()
        ))),
        1 => Ok(candidates.remove(0)),
        _ => Err(AppError::Config(format!(
            "Takeout에서 {label}이 여러 개 발견되었습니다. 시청기록만 포함한 Takeout을 지정하세요:\n{}",
            candidates.join("\n")
        ))),
    }
}

fn read_zip_entry(zip_path: &Path, entry_name: &str) -> Result<String, AppError> {
    let file = File::open(zip_path).map_err(|err| {
        AppError::io(
            format!("Takeout zip을 열 수 없습니다: {}", zip_path.display()),
            err,
        )
    })?;
    let mut archive = zip::ZipArchive::new(file).map_err(|err| {
        AppError::Config(format!(
            "Takeout zip을 해석할 수 없습니다: {} ({err})",
            zip_path.display()
        ))
    })?;
    let mut entry = archive.by_name(entry_name).map_err(|err| {
        AppError::Config(format!(
            "Takeout zip 항목을 열 수 없습니다: {entry_name} ({err})"
        ))
    })?;
    if entry.size() > MAX_FILE_BYTES {
        return Err(AppError::Config(format!(
            "시청기록 파일이 너무 큽니다(최대 {}MB): {entry_name}",
            MAX_FILE_BYTES / (1024 * 1024)
        )));
    }
    let mut buffer = String::new();
    entry.read_to_string(&mut buffer).map_err(|err| {
        AppError::io(
            format!("Takeout zip 항목을 읽을 수 없습니다: {entry_name}"),
            err,
        )
    })?;
    Ok(buffer)
}

fn read_plain_file(path: &Path) -> Result<String, AppError> {
    let metadata = std::fs::metadata(path).map_err(|err| {
        AppError::io(
            format!("시청기록 파일을 열 수 없습니다: {}", path.display()),
            err,
        )
    })?;
    if metadata.len() > MAX_FILE_BYTES {
        return Err(AppError::Config(format!(
            "시청기록 파일이 너무 큽니다(최대 {}MB): {}",
            MAX_FILE_BYTES / (1024 * 1024),
            path.display()
        )));
    }
    std::fs::read_to_string(path).map_err(|err| {
        AppError::io(
            format!("시청기록 파일을 읽을 수 없습니다: {}", path.display()),
            err,
        )
    })
}

/// Takeout 디렉터리·zip·단일 파일에서 `watch-history.json` 원문을 읽는다.
///
/// 후보가 없거나 여러 개면 오류다.
pub fn read_history_source(root: &Path) -> Result<(String, String), AppError> {
    if !root.exists() {
        return Err(AppError::Config(format!(
            "Takeout 경로를 찾을 수 없습니다: {}",
            root.display()
        )));
    }

    if root.is_file() {
        if root
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
        {
            let names = zip_entry_names(root)?;
            let entry = single_candidate(names, root, "watch-history.json")?;
            let raw = read_zip_entry(root, &entry)?;
            return Ok((format!("{}!{entry}", root.display()), raw));
        }
        if !is_history_filename(root) {
            return Err(AppError::Config(format!(
                "watch-history.json 파일을 지정해야 합니다: {}",
                root.display()
            )));
        }
        let raw = read_plain_file(root)?;
        return Ok((root.display().to_string(), raw));
    }

    let mut candidates = Vec::new();
    collect_candidates(root, 0, &mut candidates);
    let chosen = single_candidate(
        candidates
            .iter()
            .map(|path| path.display().to_string())
            .collect(),
        root,
        "watch-history.json",
    )?;
    let raw = read_plain_file(Path::new(&chosen))?;
    Ok((chosen, raw))
}

fn zip_entry_names(zip_path: &Path) -> Result<Vec<String>, AppError> {
    let file = File::open(zip_path).map_err(|err| {
        AppError::io(
            format!("Takeout zip을 열 수 없습니다: {}", zip_path.display()),
            err,
        )
    })?;
    let archive = zip::ZipArchive::new(file).map_err(|err| {
        AppError::Config(format!(
            "Takeout zip을 해석할 수 없습니다: {} ({err})",
            zip_path.display()
        ))
    })?;
    Ok(archive
        .file_names()
        .filter(|name| {
            Path::new(name)
                .file_name()
                .and_then(|file| file.to_str())
                .is_some_and(|file| file.eq_ignore_ascii_case("watch-history.json"))
        })
        .map(str::to_string)
        .collect())
}

/// 시청기록 URL에서 11자 영상 ID를 추출한다.
pub fn extract_video_id(raw: &str) -> Option<String> {
    let url = Url::parse(raw).ok()?;
    let host = url
        .host_str()?
        .trim_start_matches("www.")
        .to_ascii_lowercase();
    let candidate = if host == "youtu.be" {
        url.path_segments()?
            .find(|segment| !segment.is_empty())?
            .to_string()
    } else if host == "youtube.com" || host.ends_with(".youtube.com") {
        let segments: Vec<&str> = url
            .path_segments()?
            .filter(|segment| !segment.is_empty())
            .collect();
        match segments.as_slice() {
            ["watch"] => url
                .query_pairs()
                .find(|(key, _)| key == "v")
                .map(|(_, value)| value.into_owned())?,
            ["shorts", id] => (*id).to_string(),
            _ => return None,
        }
    } else {
        return None;
    };
    is_video_id(&candidate).then_some(candidate)
}

fn is_video_id(value: &str) -> bool {
    value.len() == 11
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
}

/// 채널 URL에서 `UC...` 채널 ID를 추출한다.
pub fn extract_channel_id(raw: &str) -> Option<String> {
    let url = Url::parse(raw).ok()?;
    let segments: Vec<&str> = url
        .path_segments()?
        .filter(|part| !part.is_empty())
        .collect();
    match segments.as_slice() {
        ["channel", id] if id.starts_with("UC") && id.len() > 2 => Some((*id).to_string()),
        _ => None,
    }
}

fn normalize(raw: RawEntry) -> HistoryEntry {
    let raw_title = raw.title.unwrap_or_default();
    let title = raw_title
        .strip_prefix("Watched ")
        .unwrap_or(&raw_title)
        .to_string();
    let video_id = raw.title_url.as_deref().and_then(extract_video_id);
    let subtitle = raw.subtitles.as_ref().and_then(|list| list.first());
    let channel_name = subtitle.and_then(|item| item.name.clone());
    let channel_id = subtitle
        .and_then(|item| item.url.as_deref())
        .and_then(extract_channel_id);
    let watched_at = raw
        .time
        .as_deref()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc));
    let is_ad = raw.details.as_ref().is_some_and(|details| {
        details
            .iter()
            .filter_map(|detail| detail.name.as_deref())
            .any(|name| name.contains("Google Ads"))
    });

    HistoryEntry {
        video_id,
        title,
        channel_name,
        channel_id,
        watched_at,
        header: raw.header.unwrap_or_else(|| "YouTube".to_string()),
        is_ad,
    }
}

/// `watch-history.json` 원문을 정규화한다.
pub fn parse_history(raw: &str) -> Result<Vec<HistoryEntry>, AppError> {
    let entries: Vec<RawEntry> = serde_json::from_str(raw)
        .map_err(|err| AppError::json("watch-history.json을 해석할 수 없습니다", err))?;
    Ok(entries.into_iter().map(normalize).collect())
}

/// Takeout에서 시청기록을 읽어 `<home>/data/history.json`에 저장한다.
pub fn import(paths: &Paths, root: &Path, now: DateTime<Utc>) -> Result<HistoryFile, AppError> {
    let (source_file, raw) = read_history_source(root)?;
    let entries = parse_history(&raw)?;
    let file = HistoryFile {
        imported_at: now,
        source_file,
        entries,
    };
    let payload = serde_json::to_vec_pretty(&file)
        .map_err(|err| AppError::json("시청기록을 직렬화할 수 없습니다", err))?;
    paths.write_atomic(&paths.history(), &payload)?;
    Ok(file)
}

/// 저장된 시청기록을 읽는다.
pub fn load(paths: &Paths) -> Result<HistoryFile, AppError> {
    let path = paths.history();
    let raw = std::fs::read_to_string(&path).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            AppError::Config(
                "시청기록이 없습니다. `stk history import --takeout <경로>`를 먼저 실행하세요."
                    .to_string(),
            )
        } else {
            AppError::io(
                format!("시청기록을 읽을 수 없습니다: {}", path.display()),
                err,
            )
        }
    })?;
    serde_json::from_str(&raw).map_err(|err| {
        AppError::json(
            format!("시청기록을 해석할 수 없습니다: {}", path.display()),
            err,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_video_id_handles_known_urls() {
        assert_eq!(
            extract_video_id("https://www.youtube.com/watch?v=dQw4w9WgXcQ"),
            Some("dQw4w9WgXcQ".to_string())
        );
        assert_eq!(
            extract_video_id("https://youtu.be/dQw4w9WgXcQ"),
            Some("dQw4w9WgXcQ".to_string())
        );
        assert_eq!(
            extract_video_id("https://music.youtube.com/watch?v=dQw4w9WgXcQ"),
            Some("dQw4w9WgXcQ".to_string())
        );
        assert_eq!(
            extract_video_id("https://www.youtube.com/shorts/dQw4w9WgXcQ"),
            Some("dQw4w9WgXcQ".to_string())
        );
        assert_eq!(extract_video_id("https://example.com/x"), None);
        assert_eq!(
            extract_video_id("https://www.youtube.com/watch?v=too-short"),
            None
        );
    }

    #[test]
    fn extract_channel_id_reads_channel_path() {
        assert_eq!(
            extract_channel_id("https://www.youtube.com/channel/UC_TEST_CHANNEL"),
            Some("UC_TEST_CHANNEL".to_string())
        );
        assert_eq!(extract_channel_id("https://www.youtube.com/@handle"), None);
    }
}
