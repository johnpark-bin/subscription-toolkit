//! 통합 테스트 공용 도우미.

#![allow(dead_code)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use wiremock::{MockServer, Request, Respond, ResponseTemplate};

use subscription_toolkit::auth::{StaticTokenSource, TokenSource};
use subscription_toolkit::youtube::ApiError;
use subscription_toolkit::youtube::HttpYouTube;

/// 호출 순서대로 다른 응답을 돌려주는 wiremock 응답기.
pub struct Sequence {
    responses: Vec<ResponseTemplate>,
    index: AtomicUsize,
}

impl Sequence {
    /// 응답 목록으로 만든다.
    pub fn new(responses: Vec<ResponseTemplate>) -> Self {
        assert!(!responses.is_empty(), "응답 목록이 비어 있습니다");
        Self {
            responses,
            index: AtomicUsize::new(0),
        }
    }

    /// 지금까지 처리한 요청 수.
    pub fn hits(&self) -> usize {
        self.index.load(Ordering::SeqCst)
    }
}

impl Respond for Sequence {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        let index = self.index.fetch_add(1, Ordering::SeqCst);
        self.responses
            .get(index)
            .or_else(|| self.responses.last())
            .cloned()
            .unwrap_or_else(|| ResponseTemplate::new(500))
    }
}

/// JSON 본문 응답.
pub fn json_response(status: u16, body: Value) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_json(body)
}

/// Google API 형식 오류 응답.
pub fn api_error_response(status: u16, reason: &str, message: &str) -> ResponseTemplate {
    json_response(
        status,
        serde_json::json!({
            "error": {
                "code": status,
                "message": message,
                "errors": [{"reason": reason, "message": message}],
            }
        }),
    )
}

/// 고정 토큰을 쓰는 테스트 클라이언트.
pub fn static_client(server: &MockServer) -> HttpYouTube {
    client(server, Arc::new(StaticTokenSource::new("test-token")))
}

/// 토큰 소스를 지정한 테스트 클라이언트. 재시도 지연은 1ms로 줄인다.
pub fn client(server: &MockServer, tokens: Arc<dyn TokenSource>) -> HttpYouTube {
    HttpYouTube::new(format!("{}/youtube/v3", server.uri()), tokens)
        .expect("클라이언트 생성 실패")
        .with_retry_base(Duration::from_millis(1))
}

/// 강제 갱신 요청에 다른 토큰을 돌려주는 토큰 소스.
pub struct RefreshingToken;

#[async_trait]
impl TokenSource for RefreshingToken {
    async fn access_token(&self, force_refresh: bool) -> Result<String, ApiError> {
        Ok(if force_refresh {
            "refreshed-token".to_string()
        } else {
            "initial-token".to_string()
        })
    }
}

/// 요청의 Authorization 헤더 값.
pub fn authorization(request: &Request) -> Option<String> {
    request
        .headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// 요청 URL의 쿼리 값.
pub fn query_param(request: &Request, key: &str) -> Option<String> {
    request
        .url
        .query_pairs()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.into_owned())
}
