//! YouTube API 클라이언트의 HTTP 계약 테스트.

mod support;

use std::sync::Arc;

use serde_json::json;
use support::{
    RefreshingToken, Sequence, api_error_response, authorization, client, json_response,
    query_param, static_client,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use subscription_toolkit::youtube::{ApiError, HttpYouTube, Privacy, YouTubeApi};

const SUBSCRIPTIONS_PATH: &str = "/youtube/v3/subscriptions";
const PLAYLISTS_PATH: &str = "/youtube/v3/playlists";
const PLAYLIST_ITEMS_PATH: &str = "/youtube/v3/playlistItems";
const VIDEOS_PATH: &str = "/youtube/v3/videos";

fn subscription_item(channel_id: &str, title: &str) -> serde_json::Value {
    json!({
        "snippet": {
            "title": title,
            "resourceId": {"kind": "youtube#channel", "channelId": channel_id},
        }
    })
}

#[tokio::test]
async fn list_subscriptions_paginates() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(SUBSCRIPTIONS_PATH))
        .and(wiremock::matchers::query_param_is_missing("pageToken"))
        .respond_with(json_response(
            200,
            json!({
                "items": [subscription_item("UC_A", "채널 A")],
                "nextPageToken": "p2",
            }),
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(SUBSCRIPTIONS_PATH))
        .and(wiremock::matchers::query_param("pageToken", "p2"))
        .respond_with(json_response(
            200,
            json!({"items": [subscription_item("UC_B", "채널 B")]}),
        ))
        .mount(&server)
        .await;

    let api = static_client(&server);
    let subscriptions = api.list_subscriptions().await.expect("구독 조회 실패");

    assert_eq!(subscriptions.len(), 2);
    assert_eq!(subscriptions[0].channel_id, "UC_A");
    assert_eq!(subscriptions[1].channel_id, "UC_B");
    assert!(subscriptions[1].title == "채널 B");

    let requests = server.received_requests().await.expect("요청 기록 없음");
    assert_eq!(requests.len(), 2);
    assert_eq!(
        query_param(&requests[0], "maxResults").as_deref(),
        Some("50")
    );
    assert_eq!(query_param(&requests[0], "mine").as_deref(), Some("true"));
}

#[tokio::test]
async fn subscribe_posts_expected_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(SUBSCRIPTIONS_PATH))
        .respond_with(json_response(200, json!({"id": "new-subscription"})))
        .mount(&server)
        .await;

    let api = static_client(&server);
    api.subscribe("UC_TARGET").await.expect("구독 실패");

    let requests = server.received_requests().await.expect("요청 기록 없음");
    assert_eq!(requests.len(), 1);
    let body: serde_json::Value = requests[0].body_json().expect("본문 해석 실패");
    assert_eq!(
        body,
        json!({
            "snippet": {
                "resourceId": {"kind": "youtube#channel", "channelId": "UC_TARGET"}
            }
        })
    );
    assert_eq!(
        query_param(&requests[0], "part").as_deref(),
        Some("snippet")
    );
    assert_eq!(
        authorization(&requests[0]).as_deref(),
        Some("Bearer test-token")
    );
}

#[tokio::test]
async fn playlist_insert_sets_private_status() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PLAYLISTS_PATH))
        .respond_with(json_response(200, json!({"id": "PL_NEW"})))
        .mount(&server)
        .await;

    let api = static_client(&server);
    let playlist_id = api
        .create_playlist("음악", "설명", Privacy::Private)
        .await
        .expect("플레이리스트 생성 실패");
    assert_eq!(playlist_id, "PL_NEW");

    let requests = server.received_requests().await.expect("요청 기록 없음");
    let body: serde_json::Value = requests[0].body_json().expect("본문 해석 실패");
    assert_eq!(body["status"]["privacyStatus"], json!("private"));
    assert_eq!(body["snippet"]["title"], json!("음악"));
    assert_eq!(body["snippet"]["description"], json!("설명"));
}

#[tokio::test]
async fn playlist_item_insert_omits_position() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PLAYLIST_ITEMS_PATH))
        .respond_with(json_response(200, json!({"id": "PI_NEW"})))
        .mount(&server)
        .await;

    let api = static_client(&server);
    api.add_playlist_item("PL_DST", "VID_1")
        .await
        .expect("항목 추가 실패");

    let requests = server.received_requests().await.expect("요청 기록 없음");
    let body: serde_json::Value = requests[0].body_json().expect("본문 해석 실패");
    assert_eq!(body["snippet"]["playlistId"], json!("PL_DST"));
    assert_eq!(body["snippet"]["resourceId"]["videoId"], json!("VID_1"));
    assert!(
        body["snippet"].get("position").is_none(),
        "playlistItems.insert 요청에 position을 넣으면 안 된다: {body}"
    );
}

#[tokio::test]
async fn duplicate_subscription_maps_to_already_exists() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(SUBSCRIPTIONS_PATH))
        .respond_with(api_error_response(
            400,
            "subscriptionDuplicate",
            "The subscription that you are trying to add already exists.",
        ))
        .mount(&server)
        .await;

    let api = static_client(&server);
    let error = api
        .subscribe("UC_A")
        .await
        .expect_err("중복 구독은 오류여야 한다");
    assert!(matches!(error, ApiError::AlreadyExists(_)), "{error:?}");
}

#[tokio::test]
async fn quota_exceeded_maps_to_quota_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(SUBSCRIPTIONS_PATH))
        .respond_with(api_error_response(
            403,
            "quotaExceeded",
            "The request cannot be completed because you have exceeded your quota.",
        ))
        .mount(&server)
        .await;

    let api = static_client(&server);
    let error = api
        .list_subscriptions()
        .await
        .expect_err("쿼터 초과는 오류여야 한다");
    assert!(matches!(error, ApiError::QuotaExceeded), "{error:?}");
    let app_error: subscription_toolkit::error::AppError = error.into();
    assert_eq!(app_error.exit_code(), 4);
}

#[tokio::test]
async fn retry_after_429_then_succeeds() {
    let server = MockServer::start().await;
    let sequence = Sequence::new(vec![
        api_error_response(429, "rateLimitExceeded", "Too many requests."),
        json_response(200, json!({"items": [subscription_item("UC_A", "채널 A")]})),
    ]);
    Mock::given(method("GET"))
        .and(path(SUBSCRIPTIONS_PATH))
        .respond_with(sequence)
        .mount(&server)
        .await;

    let api = static_client(&server);
    let subscriptions = api
        .list_subscriptions()
        .await
        .expect("재시도 후 성공해야 한다");
    assert_eq!(subscriptions.len(), 1);
    assert_eq!(
        server.received_requests().await.expect("요청 기록").len(),
        2,
        "429 후 한 번 재시도해야 한다"
    );
}

#[tokio::test]
async fn retries_aborted_playlist_insert() {
    let server = MockServer::start().await;
    let sequence = Sequence::new(vec![
        json_response(
            409,
            json!({
                "error": {
                    "code": 409,
                    "message": "The operation was aborted.",
                    "errors": [{"domain": "youtube.CoreErrorDomain", "reason": "SERVICE_UNAVAILABLE"}],
                    "status": "ABORTED",
                }
            }),
        ),
        json_response(200, json!({"id": "PI_NEW"})),
    ]);
    Mock::given(method("POST"))
        .and(path(PLAYLIST_ITEMS_PATH))
        .respond_with(sequence)
        .mount(&server)
        .await;

    let api = static_client(&server);
    api.add_playlist_item("PL_DST", "VID_1")
        .await
        .expect("409 ABORTED는 재시도 후 성공해야 한다");
    assert_eq!(
        server.received_requests().await.expect("요청 기록").len(),
        2,
        "409 후 한 번 재시도한다"
    );
}

#[tokio::test]
async fn unauthorized_refreshes_once() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(SUBSCRIPTIONS_PATH))
        .respond_with(Sequence::new(vec![
            api_error_response(401, "authError", "Invalid Credentials"),
            json_response(200, json!({"items": [subscription_item("UC_A", "채널 A")]})),
        ]))
        .mount(&server)
        .await;

    let api = client(&server, Arc::new(RefreshingToken));
    let subscriptions = api
        .list_subscriptions()
        .await
        .expect("갱신 후 성공해야 한다");
    assert_eq!(subscriptions.len(), 1);

    let requests = server.received_requests().await.expect("요청 기록");
    assert_eq!(requests.len(), 2);
    assert_eq!(
        authorization(&requests[0]).as_deref(),
        Some("Bearer initial-token")
    );
    assert_eq!(
        authorization(&requests[1]).as_deref(),
        Some("Bearer refreshed-token"),
        "401 이후에는 강제 갱신한 토큰을 써야 한다"
    );
}

#[tokio::test]
async fn like_accepts_empty_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/youtube/v3/videos/rate"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let api = static_client(&server);
    api.like("VID_1").await.expect("좋아요 실패");

    let requests = server.received_requests().await.expect("요청 기록");
    assert_eq!(query_param(&requests[0], "id").as_deref(), Some("VID_1"));
    assert_eq!(query_param(&requests[0], "rating").as_deref(), Some("like"));
}

#[tokio::test]
async fn list_playlists_requests_own_playlists() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(PLAYLISTS_PATH))
        .respond_with(json_response(
            200,
            json!({"items": [{
                "id": "PL_SRC",
                "snippet": {"title": "음악", "description": "설명"},
                "status": {"privacyStatus": "private"},
                "contentDetails": {"itemCount": 3},
            }]}),
        ))
        .mount(&server)
        .await;

    let api: HttpYouTube = static_client(&server);
    let playlists = api.list_playlists().await.expect("플레이리스트 조회 실패");
    assert_eq!(playlists.len(), 1);
    assert_eq!(playlists[0].title, "음악");
    assert_eq!(playlists[0].item_count, 3);
    assert_eq!(playlists[0].privacy, Privacy::Private);

    let requests = server.received_requests().await.expect("요청 기록");
    assert_eq!(query_param(&requests[0], "mine").as_deref(), Some("true"));
}

#[tokio::test]
async fn list_playlist_items_sorts_by_position() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(PLAYLIST_ITEMS_PATH))
        .respond_with(json_response(
            200,
            json!({"items": [
                {"id": "PI_2", "snippet": {"title": "둘째", "position": 1, "resourceId": {"videoId": "VID_2"}}},
                {"id": "PI_1", "snippet": {"title": "첫째", "position": 0, "resourceId": {"videoId": "VID_1"}}},
            ]}),
        ))
        .mount(&server)
        .await;

    let api = static_client(&server);
    let items = api
        .list_playlist_items("PL_SRC")
        .await
        .expect("항목 조회 실패");
    assert_eq!(
        items
            .iter()
            .map(|item| item.video_id.as_str())
            .collect::<Vec<_>>(),
        vec!["VID_1", "VID_2"]
    );
    assert_eq!(items[0].item_id, "PI_1");
}

#[tokio::test]
async fn list_liked_videos_requests_my_rating() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(VIDEOS_PATH))
        .respond_with(json_response(
            200,
            json!({"items": [{"id": "VID_1", "snippet": {"title": "영상"}}]}),
        ))
        .mount(&server)
        .await;

    let api = static_client(&server);
    let likes = api.list_liked_videos().await.expect("좋아요 조회 실패");
    assert_eq!(likes.len(), 1);
    assert_eq!(likes[0].video_id, "VID_1");

    let requests = server.received_requests().await.expect("요청 기록");
    assert_eq!(
        query_param(&requests[0], "myRating").as_deref(),
        Some("like")
    );
}

#[tokio::test]
async fn video_not_found_maps_to_not_found() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(PLAYLIST_ITEMS_PATH))
        .respond_with(api_error_response(
            404,
            "videoNotFound",
            "The video specified by the videoId was not found.",
        ))
        .mount(&server)
        .await;

    let api = static_client(&server);
    let error = api
        .add_playlist_item("PL_DST", "MISSING")
        .await
        .expect_err("없는 영상은 오류여야 한다");
    assert!(matches!(error, ApiError::NotFound(_)), "{error:?}");
}
