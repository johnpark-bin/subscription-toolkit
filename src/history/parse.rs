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

/// Takeout 파일 형식.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryFormat {
    /// `watch-history.json`(권장).
    Json,
    /// `watch-history.html`(JSON을 선택하지 않은 내보내기).
    Html,
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

fn format_of_name(name: &str) -> Option<HistoryFormat> {
    let lower = name.to_ascii_lowercase();
    if lower == "watch-history.json" {
        Some(HistoryFormat::Json)
    } else if lower == "watch-history.html" {
        Some(HistoryFormat::Html)
    } else {
        None
    }
}

fn format_of_path(path: &Path) -> Option<HistoryFormat> {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(format_of_name)
}

fn is_history_filename(path: &Path) -> bool {
    format_of_path(path).is_some()
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

/// 후보 중 하나를 고른다. JSON이 있으면 JSON을 우선한다.
fn pick_candidate(names: Vec<String>, root: &Path) -> Result<(String, HistoryFormat), AppError> {
    let json: Vec<String> = names
        .iter()
        .filter(|name| {
            format_of_name(
                Path::new(name)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(""),
            ) == Some(HistoryFormat::Json)
        })
        .cloned()
        .collect();
    if !json.is_empty() {
        let chosen = single_candidate(json, root, "watch-history.json")?;
        return Ok((chosen, HistoryFormat::Json));
    }
    let html: Vec<String> = names
        .iter()
        .filter(|name| {
            format_of_name(
                Path::new(name)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(""),
            ) == Some(HistoryFormat::Html)
        })
        .cloned()
        .collect();
    let chosen = single_candidate(html, root, "watch-history.html")?;
    Ok((chosen, HistoryFormat::Html))
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
pub fn read_history_source(root: &Path) -> Result<(String, String, HistoryFormat), AppError> {
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
            let (entry, format) = pick_candidate(names, root)?;
            let raw = read_zip_entry(root, &entry)?;
            return Ok((format!("{}!{entry}", root.display()), raw, format));
        }
        let Some(format) = format_of_path(root) else {
            return Err(AppError::Config(format!(
                "watch-history.json 또는 watch-history.html 파일을 지정해야 합니다: {}",
                root.display()
            )));
        };
        let raw = read_plain_file(root)?;
        return Ok((root.display().to_string(), raw, format));
    }

    let mut candidates = Vec::new();
    collect_candidates(root, 0, &mut candidates);
    let (chosen, format) = pick_candidate(
        candidates
            .iter()
            .map(|path| path.display().to_string())
            .collect(),
        root,
    )?;
    let raw = read_plain_file(Path::new(&chosen))?;
    Ok((chosen, raw, format))
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
                .and_then(format_of_name)
                .is_some()
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

fn decode_entities(text: &str) -> String {
    text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&emsp;", " ")
}

fn parse_month(name: &str) -> Option<u32> {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    MONTHS
        .iter()
        .position(|month| month.eq_ignore_ascii_case(name))
        .map(|index| index as u32 + 1)
}

fn zone_offset_hours(zone: &str) -> i64 {
    match zone {
        "KST" => 9,
        "JST" => 9,
        "UTC" | "GMT" => 0,
        "PST" => -8,
        "PDT" => -7,
        "EST" => -5,
        "EDT" => -4,
        _ => 0,
    }
}

/// `Oct 30, 2026, 10:02:29 PM KST` 형식의 시각을 UTC로 바꾼다.
pub fn parse_html_time(raw: &str) -> Option<DateTime<Utc>> {
    let mut parts = raw.split_whitespace();
    let month = parse_month(parts.next()?)?;
    let day: u32 = parts.next()?.trim_end_matches(',').parse().ok()?;
    let year: i32 = parts.next()?.trim_end_matches(',').parse().ok()?;
    let clock = parts.next()?;
    let meridiem = parts.next()?;
    let zone = parts.next().unwrap_or("UTC");
    let mut clock_parts = clock.split(':');
    let mut hour: u32 = clock_parts.next()?.parse().ok()?;
    let minute: u32 = clock_parts.next()?.parse().ok()?;
    let second: u32 = clock_parts.next().unwrap_or("0").parse().ok()?;
    if meridiem.eq_ignore_ascii_case("PM") && hour < 12 {
        hour += 12;
    }
    if meridiem.eq_ignore_ascii_case("AM") && hour == 12 {
        hour = 0;
    }
    let naive =
        chrono::NaiveDate::from_ymd_opt(year, month, day)?.and_hms_opt(hour, minute, second)?;
    let offset = chrono::FixedOffset::east_opt((zone_offset_hours(zone) * 3600) as i32)?;
    Some(
        naive
            .and_local_timezone(offset)
            .single()?
            .with_timezone(&Utc),
    )
}

/// HTML 조각에서 `(href, 텍스트)` 앵커 목록을 뽑는다.
fn html_anchors(content: &str) -> Vec<(String, String)> {
    let mut anchors = Vec::new();
    let mut offset = 0usize;
    while let Some(found) = content[offset..].find("<a href=\"") {
        let href_start = offset + found + "<a href=\"".len();
        let Some(quote) = content[href_start..].find('"') else {
            break;
        };
        let href_end = href_start + quote;
        let href = content[href_start..href_end].to_string();
        let Some(gt) = content[href_end..].find('>') else {
            break;
        };
        let text_start = href_end + gt + 1;
        let Some(close) = content[text_start..].find("</a>") else {
            break;
        };
        let text = decode_entities(&content[text_start..text_start + close])
            .trim()
            .to_string();
        anchors.push((href, text));
        offset = text_start + close + "</a>".len();
    }
    anchors
}

/// `watch-history.html` 원문을 정규화한다.
///
/// JSON을 선택하지 않고 내보낸 Takeout을 지원하기 위한 경로다. 항목마다 제목 링크,
/// 채널 링크, 시각을 추출하고 광고·삭제 항목을 구분한다.
pub fn parse_history_html(raw: &str) -> Vec<HistoryEntry> {
    let mut entries = Vec::new();
    for chunk in raw.split("<div class=\"outer-cell").skip(1) {
        let header = chunk
            .split("mdl-typography--title\">")
            .nth(1)
            .and_then(|rest| rest.split('<').next())
            .map(|text| decode_entities(text).trim().to_string())
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| "YouTube".to_string());

        let content = chunk
            .split("mdl-typography--body-1\">")
            .nth(1)
            .and_then(|rest| rest.split("</div>").next())
            .unwrap_or_default();
        let anchors = html_anchors(content);

        let video_anchor = anchors.iter().find_map(|(href, text)| {
            let id = href
                .split("watch?v=")
                .nth(1)
                .and_then(|rest| rest.split(['&', '"']).next())?;
            is_video_id(id).then(|| (id.to_string(), text.clone()))
        });
        let channel_anchor = anchors
            .iter()
            .find(|(href, _)| href.contains("/channel/") || href.contains("/@"));

        let (video_id, title) = match &video_anchor {
            Some((id, text)) => (Some(id.clone()), text.clone()),
            None => (
                None,
                decode_entities(content.split("<br>").next().unwrap_or_default())
                    .trim()
                    .trim_start_matches("Watched ")
                    .trim_start_matches("Viewed ")
                    .to_string(),
            ),
        };
        let (channel_id, channel_name) = match channel_anchor {
            Some((href, name)) => (
                extract_channel_id(href),
                (!name.is_empty()).then(|| name.clone()),
            ),
            None => (None, None),
        };

        let time_text = content.split("<br>").map(|part| part.trim()).find(|part| {
            part.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                && part.contains(',')
                && part.contains("20")
        });
        let watched_at = time_text.and_then(parse_html_time);

        let is_ad = chunk.contains("From Google Ads");
        entries.push(HistoryEntry {
            video_id,
            title,
            channel_name,
            channel_id,
            watched_at,
            header,
            is_ad,
        });
    }
    entries
}

/// Takeout에서 시청기록을 읽어 `<home>/data/history.json`에 저장한다.
pub fn import(paths: &Paths, root: &Path, now: DateTime<Utc>) -> Result<HistoryFile, AppError> {
    let (source_file, raw, format) = read_history_source(root)?;
    let entries = match format {
        HistoryFormat::Json => parse_history(&raw)?,
        HistoryFormat::Html => parse_history_html(&raw),
    };
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
    fn parse_html_extracts_entries() {
        let raw = include_str!("../../tests/fixtures/takeout/watch-history.html");
        let entries = parse_history_html(raw);
        assert_eq!(entries.len(), 4);

        let first = &entries[0];
        assert_eq!(first.video_id.as_deref(), Some("aaaaaaaaaaa"));
        assert_eq!(first.title, "영상 A");
        assert_eq!(first.channel_name.as_deref(), Some("Channel A"));
        assert!(
            first
                .channel_id
                .as_deref()
                .is_some_and(|id| id.starts_with("UC"))
        );
        assert_eq!(
            first.watched_at,
            Some(
                DateTime::parse_from_rfc3339("2026-10-30T13:02:29Z")
                    .expect("시각")
                    .with_timezone(&Utc)
            ),
            "KST를 UTC로 변환한다"
        );
        assert!(!first.is_ad);

        assert!(entries[1].is_ad, "Google Ads 항목");
        assert_eq!(entries[2].video_id, None, "삭제된 영상");
        assert_eq!(entries[3].header, "YouTube Music");
        assert_eq!(entries[3].title, "영상 & 노래 E", "HTML 엔티티 복원");
    }

    #[test]
    fn parse_html_time_handles_meridiem() {
        assert_eq!(
            parse_html_time("Oct 28, 2026, 9:00:00 AM KST"),
            Some(
                DateTime::parse_from_rfc3339("2026-10-28T00:00:00Z")
                    .expect("시각")
                    .with_timezone(&Utc)
            )
        );
        assert_eq!(
            parse_html_time("Oct 28, 2026, 12:00:00 AM UTC"),
            Some(
                DateTime::parse_from_rfc3339("2026-10-28T00:00:00Z")
                    .expect("시각")
                    .with_timezone(&Utc)
            )
        );
        assert_eq!(parse_html_time("not a time"), None);
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
