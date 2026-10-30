//! 브라우저 페이지에서 실행하는 스크립트.
//!
//! 사용자가 로그인한 YouTube 페이지의 세션을 그대로 사용해, 웹앱이 호출하는 것과 같은
//! 내부 엔드포인트를 `fetch`로 호출한다. 브라우저 지문을 바꾸거나 자동화 흔적을 숨기는
//! 동작은 넣지 않는다.

use serde_json::{Value, json};

use crate::browser::cdp::{CdpError, PageResponse};

/// YouTube 일반 페이지(요청 실행은 이 오리진에서 한다).
pub const YOUTUBE_URL: &str = "https://www.youtube.com/";
/// 채널 ID를 노출하는 Studio 페이지(계정 확인용).
pub const STUDIO_URL: &str = "https://studio.youtube.com/";

/// Watch Later 재생목록 ID.
pub const WATCH_LATER_PLAYLIST_ID: &str = "WL";
/// Watch Later 조회용 browse ID.
pub const WATCH_LATER_BROWSE_ID: &str = "VLWL";

/// Watch Later 목록을 읽는 스크립트(선택적 continuation).
pub fn watch_later_read_js(continuation: Option<&str>) -> String {
    let mut js = String::new();
    js.push_str(
        r#"(async () => {
  const cfg = window.ytcfg;
  if (!cfg || typeof cfg.get !== 'function') {
    return { ok: false, status: 0, items: [], error: 'youtube 페이지가 아닙니다' };
  }
  const key = cfg.get('INNERTUBE_API_KEY');
  const context = cfg.get('INNERTUBE_CONTEXT');
  if (!key || !context) {
    return { ok: false, status: 0, items: [], error: 'INNERTUBE 설정을 읽을 수 없습니다' };
  }
  const origin = 'https://www.youtube.com';
  const sapisid = (document.cookie.match(/(?:^|; )SAPISID=([^;]+)/) || [])[1] || '';
  const sha1 = async (text) => Array.from(
    new Uint8Array(await crypto.subtle.digest('SHA-1', new TextEncoder().encode(text)))
  ).map((byte) => byte.toString(16).padStart(2, '0')).join('');
  const ts = Math.floor(Date.now() / 1000);
  const headers = {
    'content-type': 'application/json',
    'x-origin': origin,
    'x-youtube-client-name': String((context.client || {}).clientName || 1),
    'x-youtube-client-version': String((context.client || {}).clientVersion || ''),
  };
  if (sapisid) headers.authorization = 'SAPISIDHASH ' + ts + '_' + await sha1(ts + ' ' + sapisid + ' ' + origin);
  const body = { context: context, browseId: "#,
    );
    js.push_str(&js_literal(WATCH_LATER_BROWSE_ID));
    js.push_str(
        r#" };
  const continuation = "#,
    );
    js.push_str(&js_literal(continuation.unwrap_or_default()));
    js.push_str(
        r#";
  if (continuation) { body.continuation = continuation; }
  try {
    const response = await fetch(origin + '/youtubei/v1/browse?key=' + encodeURIComponent(key) + '&prettyPrint=false', {
      method: 'POST', credentials: 'include', headers: headers, body: JSON.stringify(body),
    });
    const text = await response.text();
    let parsed = null;
    try { parsed = JSON.parse(text); } catch (error) { return { ok: false, status: response.status, items: [], error: 'JSON 해석 실패' }; }
    const items = [];
    let next = null;
    const walk = (node) => {
      if (!node || typeof node !== 'object') return;
      if (Array.isArray(node)) { node.forEach(walk); return; }
      if (node.playlistVideoRenderer && node.playlistVideoRenderer.videoId) {
        const renderer = node.playlistVideoRenderer;
        let title = '';
        if (renderer.title) title = renderer.title.simpleText || (renderer.title.runs || []).map((run) => run.text).join('');
        items.push({ video_id: renderer.videoId, title: title });
      }
      const token = node.continuationEndpoint && node.continuationEndpoint.continuationCommand && node.continuationEndpoint.continuationCommand.token;
      if (token) next = token;
      Object.values(node).forEach(walk);
    };
    walk(parsed);
    return { ok: response.ok, status: response.status, items: items, continuation: next };
  } catch (error) {
    return { ok: false, status: 0, items: [], error: String(error) };
  }
})()"#,
    );
    js
}

/// Watch Later 추가/삭제 요청 본문.
pub fn watch_later_edit_body(video_id: &str, add: bool) -> Value {
    if add {
        json!({
            "playlistId": WATCH_LATER_PLAYLIST_ID,
            "actions": [{"action": "ACTION_ADD_VIDEO", "addedVideoId": video_id}],
        })
    } else {
        json!({
            "playlistId": WATCH_LATER_PLAYLIST_ID,
            "actions": [{"action": "ACTION_REMOVE_VIDEO_BY_VIDEO_ID", "removedVideoId": video_id}],
        })
    }
}

/// 구독 추가 엔드포인트.
pub const SUBSCRIBE_ENDPOINT: &str = "/youtubei/v1/subscription/subscribe";
/// 플레이리스트 생성 엔드포인트.
pub const CREATE_PLAYLIST_ENDPOINT: &str = "/youtubei/v1/playlist/create";
/// 플레이리스트 편집 엔드포인트.
pub const EDIT_PLAYLIST_ENDPOINT: &str = "/youtubei/v1/browse/edit_playlist";
/// 좋아요 엔드포인트.
pub const LIKE_ENDPOINT: &str = "/youtubei/v1/like/like";

fn js_literal(text: &str) -> String {
    Value::String(text.to_string()).to_string()
}

/// 현재 로그인한 채널 정보를 읽는 스크립트.
pub fn account_js() -> String {
    r#"(() => {
  const cfg = window.ytcfg;
  if (!cfg || typeof cfg.get !== 'function') {
    return { ok: false, reason: 'youtube 페이지가 아닙니다' };
  }
  return {
    ok: true,
    logged_in: !!cfg.get('LOGGED_IN'),
    channel_id: cfg.get('CHANNEL_ID') || null,
    session_index: cfg.get('SESSION_INDEX') ?? null,
    delegated_session_id: cfg.get('DELEGATED_SESSION_ID') || null,
    client_version: cfg.get('INNERTUBE_CLIENT_VERSION') || null,
  };
})()"#
        .to_string()
}

/// INNERTUBE 요청을 보내는 스크립트를 만든다.
///
/// `body`는 `context`와 합쳐 요청 본문이 된다.
pub fn request_js(endpoint: &str, body: &Value) -> String {
    let mut js = String::new();
    js.push_str(
        r#"(async () => {
  const cfg = window.ytcfg;
  if (!cfg || typeof cfg.get !== 'function') {
    return { ok: false, status: 0, error: 'youtube 페이지가 아닙니다' };
  }
  const key = cfg.get('INNERTUBE_API_KEY');
  const context = cfg.get('INNERTUBE_CONTEXT');
  if (!key || !context) {
    return { ok: false, status: 0, error: 'INNERTUBE 설정을 읽을 수 없습니다' };
  }
  const origin = 'https://www.youtube.com';
  const client = context.client || {};
  const sapisidMatch = document.cookie.match(/(?:^|; )SAPISID=([^;]+)/);
  const sapisid = sapisidMatch ? sapisidMatch[1] : '';
  const sha1 = async (text) => Array.from(
    new Uint8Array(await crypto.subtle.digest('SHA-1', new TextEncoder().encode(text)))
  ).map((byte) => byte.toString(16).padStart(2, '0')).join('');
  const headers = {
    'content-type': 'application/json',
    'x-origin': origin,
    'x-youtube-client-name': String(client.clientName || 1),
    'x-youtube-client-version': String(client.clientVersion || ''),
  };
  if (sapisid) {
    const ts = Math.floor(Date.now() / 1000);
    headers.authorization = 'SAPISIDHASH ' + ts + '_' + await sha1(ts + ' ' + sapisid + ' ' + origin);
  }
  try {
    const payload = Object.assign({ context: context }, "#,
    );
    js.push_str(&body.to_string());
    js.push_str(
        r#");
    const response = await fetch(origin + "#,
    );
    js.push_str(&js_literal(endpoint));
    js.push_str(
        r#" + '?key=' + encodeURIComponent(key) + '&prettyPrint=false', {
      method: 'POST',
      credentials: 'include',
      headers: headers,
      body: JSON.stringify(payload),
    });
    const text = await response.text();
    return {
      ok: response.ok,
      status: response.status,
      body: text.slice(0, 200000),
      truncated: text.length > 200000,
    };
  } catch (error) {
    return { ok: false, status: 0, error: String(error) };
  }
})()"#,
    );
    js
}

/// 구독 요청 본문.
pub fn subscribe_body(channel_id: &str) -> Value {
    json!({"channelIds": [channel_id]})
}

/// 플레이리스트 생성 요청 본문.
pub fn create_playlist_body(title: &str, description: &str, privacy: &str) -> Value {
    json!({
        "title": title,
        "description": description,
        "privacy": privacy.to_ascii_uppercase(),
    })
}

/// 플레이리스트 항목 추가 요청 본문.
pub fn add_item_body(playlist_id: &str, video_id: &str) -> Value {
    json!({
        "playlistId": playlist_id,
        "actions": [{"action": "ACTION_ADD_VIDEO", "addedVideoId": video_id}],
    })
}

/// 좋아요 요청 본문.
pub fn like_body(video_id: &str) -> Value {
    json!({"target": {"videoId": video_id}})
}

/// 페이지 스크립트 결과를 [`PageResponse`]로 해석한다.
pub fn response_from(value: Value) -> Result<PageResponse, CdpError> {
    serde_json::from_value(value)
        .map_err(|error| CdpError::Protocol(format!("페이지 응답을 해석할 수 없습니다: {error}")))
}

/// browse 기반 목록 읽기 스크립트를 만든다. `extract`는 `items`/`next`를 채우는 JS 조각이다.
fn browse_reader_js(browse_id: &str, continuation: Option<&str>, extract: &str) -> String {
    let mut js = String::new();
    js.push_str(
        r#"(async () => {
  const cfg = window.ytcfg;
  if (!cfg || typeof cfg.get !== 'function') {
    return { ok: false, status: 0, items: [], error: 'youtube 페이지가 아닙니다' };
  }
  const key = cfg.get('INNERTUBE_API_KEY');
  const context = cfg.get('INNERTUBE_CONTEXT');
  if (!key || !context) {
    return { ok: false, status: 0, items: [], error: 'INNERTUBE 설정을 읽을 수 없습니다' };
  }
  const origin = 'https://www.youtube.com';
  const sapisid = (document.cookie.match(/(?:^|; )SAPISID=([^;]+)/) || [])[1] || '';
  const sha1 = async (text) => Array.from(
    new Uint8Array(await crypto.subtle.digest('SHA-1', new TextEncoder().encode(text)))
  ).map((byte) => byte.toString(16).padStart(2, '0')).join('');
  const ts = Math.floor(Date.now() / 1000);
  const headers = {
    'content-type': 'application/json',
    'x-origin': origin,
    'x-youtube-client-name': String((context.client || {}).clientName || 1),
    'x-youtube-client-version': String((context.client || {}).clientVersion || ''),
  };
  if (sapisid) headers.authorization = 'SAPISIDHASH ' + ts + '_' + await sha1(ts + ' ' + sapisid + ' ' + origin);
  const body = { context: context, browseId: "#,
    );
    js.push_str(&js_literal(browse_id));
    js.push_str(
        r#" };
  const continuation = "#,
    );
    js.push_str(&js_literal(continuation.unwrap_or_default()));
    js.push_str(
        r#";
  if (continuation) body.continuation = continuation;
  try {
    const response = await fetch(origin + '/youtubei/v1/browse?key=' + encodeURIComponent(key) + '&prettyPrint=false', {
      method: 'POST', credentials: 'include', headers: headers, body: JSON.stringify(body),
    });
    const text = await response.text();
    let parsed = null;
    try { parsed = JSON.parse(text); } catch (error) {
      return { ok: false, status: response.status, items: [], error: 'JSON 해석 실패' };
    }
    const items = [];
    let next = null;
"#,
    );
    js.push_str(extract);
    js.push_str(
        r#"
    return { ok: response.ok, status: response.status, items: items, continuation: next };
  } catch (error) {
    return { ok: false, status: 0, items: [], error: String(error) };
  }
})()"#,
    );
    js
}

const COLLECT_IDS_JS: &str = r#"
    const walk = (node) => {
      if (!node || typeof node !== 'object') return;
      if (Array.isArray(node)) { node.forEach(walk); return; }
      if (node.lockupViewModel && node.lockupViewModel.contentId) {
        const vm = node.lockupViewModel;
        const meta = vm.metadata && vm.metadata.lockupMetadataViewModel;
        let title = '';
        if (meta && meta.title && meta.title.content) title = meta.title.content;
        items.push({ video_id: vm.contentId, title: title });
      }
      if (node.channelRenderer && node.channelRenderer.channelId) {
        items.push({ video_id: node.channelRenderer.channelId, title: '' });
      }
      const token = node.continuationEndpoint && node.continuationEndpoint.continuationCommand && node.continuationEndpoint.continuationCommand.token;
      if (token) next = token;
      Object.values(node).forEach(walk);
    };
    walk(parsed);
"#;

/// 좋아요(LL) 목록을 읽는 스크립트.
pub fn liked_videos_read_js(continuation: Option<&str>) -> String {
    browse_reader_js("VLLL", continuation, COLLECT_IDS_JS)
}

/// 구독 목록(FEchannels)을 읽는 스크립트.
pub fn subscriptions_read_js(continuation: Option<&str>) -> String {
    browse_reader_js("FEchannels", continuation, COLLECT_IDS_JS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_js_embeds_endpoint_and_body() {
        let js = request_js(SUBSCRIBE_ENDPOINT, &subscribe_body("UC_A"));
        assert!(js.contains("\"/youtubei/v1/subscription/subscribe\""));
        assert!(js.contains(r#""channelIds":["UC_A"]"#));
        assert!(js.contains("SAPISIDHASH"));
        assert!(js.contains("INNERTUBE_API_KEY"));
        assert!(
            !js.contains("navigator.webdriver"),
            "자동화 흔적 은닉은 넣지 않는다"
        );
    }

    #[test]
    fn create_playlist_body_uppercases_privacy() {
        let body = create_playlist_body("음악", "설명", "private");
        assert_eq!(body["privacy"], "PRIVATE");
        assert_eq!(body["title"], "음악");
    }

    #[test]
    fn add_item_and_like_bodies_match_innertube_shape() {
        let body = add_item_body("PL_NEW", "VID_1");
        assert_eq!(body["playlistId"], "PL_NEW");
        assert_eq!(body["actions"][0]["action"], "ACTION_ADD_VIDEO");
        assert_eq!(body["actions"][0]["addedVideoId"], "VID_1");
        assert_eq!(like_body("VID_1")["target"]["videoId"], "VID_1");
    }

    #[test]
    fn response_from_reads_page_result() {
        let response =
            response_from(json!({"ok": true, "status": 200, "body": "{}"})).expect("해석 실패");
        assert!(response.ok);
        assert_eq!(response.status, 200);
        assert!(!response.is_auth_problem());
        assert!(!response.is_rate_limited());
    }
}
