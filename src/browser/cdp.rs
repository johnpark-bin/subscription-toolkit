//! Chrome DevTools Protocol 클라이언트.
//!
//! 사용자가 직접 로그인한 Chrome에 `--remote-debugging-port`로 붙어, 페이지 컨텍스트에서
//! 스크립트를 실행한다. 지문 위장이나 자동화 탐지 우회는 하지 않는다.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};

/// WebSocket 스트림 타입.
pub type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// CDP 오류.
#[derive(Debug, thiserror::Error)]
pub enum CdpError {
    /// DevTools 엔드포인트에 연결할 수 없다.
    #[error("DevTools 엔드포인트에 연결할 수 없습니다({url}): {detail}")]
    Connect {
        /// 시도한 주소.
        url: String,
        /// 원인.
        detail: String,
    },
    /// 대상/버전 정보를 해석할 수 없다.
    #[error("DevTools 대상 정보를 읽을 수 없습니다: {0}")]
    Target(String),
    /// 프로토콜 오류.
    #[error("CDP 오류: {0}")]
    Protocol(String),
    /// 응답 대기 시간 초과.
    #[error("CDP 응답 시간 초과: {0}")]
    Timeout(String),
}

impl CdpError {
    /// 사용자 안내에 쓸 한국어 요약.
    pub fn hint(&self) -> &'static str {
        match self {
            Self::Connect { .. } => {
                "Chrome을 `--remote-debugging-port=<포트>`로 실행했는지, 포트가 맞는지 확인하세요."
            }
            Self::Target(_) => {
                "포트가 Chrome의 DevTools 포트인지, Chrome 창에 웹 탭이 열려 있는지 확인하세요."
            }
            Self::Protocol(_) => "Chrome 버전과 CDP 응답을 확인하세요.",
            Self::Timeout(_) => "페이지가 응답하지 않습니다. 다른 탭을 닫고 다시 시도하세요.",
        }
    }
}

/// 붙을 페이지 대상을 고른다.
///
/// Electron 앱(`app://`)은 CDP 페이지 제어를 지원하지 않는 경우가 많으므로,
/// 웹 페이지(http/https/about)만 후보로 삼는다.
fn select_target(targets: &[TargetInfo]) -> Result<&TargetInfo, CdpError> {
    let web: Vec<&TargetInfo> = targets
        .iter()
        .filter(|item| {
            item.r#type == "page"
                && (item.url.starts_with("http://")
                    || item.url.starts_with("https://")
                    || item.url.starts_with("about:"))
        })
        .collect();
    web.iter()
        .find(|item| item.url.contains("youtube.com"))
        .or_else(|| web.first())
        .copied()
        .ok_or_else(|| {
            CdpError::Target(
                "웹 페이지 탭을 찾지 못했습니다. Chrome 창에 탭을 하나 열어 두세요.".to_string(),
            )
        })
}

/// `/json/version` 응답.
#[derive(Debug, Clone, Deserialize)]
struct VersionInfo {
    #[serde(default, rename = "Browser")]
    browser: String,
    #[serde(default, rename = "User-Agent")]
    user_agent: String,
}

/// DevTools 엔드포인트가 Chrome 계열인지 확인한다.
///
/// Electron 앱(예: Obsidian)도 같은 포트 형식을 쓰므로, 엉뚱한 앱에 붙는 것을 막는다.
fn ensure_chrome_browser(browser: &str, user_agent: &str) -> Result<(), CdpError> {
    if user_agent.contains("Electron/") {
        return Err(CdpError::Target(
            "이 포트는 Chrome이 아니라 Electron 앱입니다(예: Obsidian). Chrome을 --remote-debugging-port로 실행한 포트를 지정하세요."
                .to_string(),
        ));
    }
    let name = browser.split('/').next().unwrap_or_default();
    let supported = ["Chrome", "HeadlessChrome", "Chromium", "Microsoft Edge"];
    if supported
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(name))
    {
        Ok(())
    } else if browser.is_empty() {
        Err(CdpError::Target(
            "DevTools 버전 정보를 읽을 수 없습니다. Chrome인지 확인하세요.".to_string(),
        ))
    } else {
        Err(CdpError::Target(format!(
            "이 포트는 Chrome이 아닙니다({browser}). Chrome을 --remote-debugging-port로 실행하세요."
        )))
    }
}

#[derive(Debug, Clone, Deserialize)]
struct TargetInfo {
    #[serde(default)]
    r#type: String,
    #[serde(default)]
    url: String,
    #[serde(rename = "webSocketDebuggerUrl", default)]
    web_socket_debugger_url: Option<String>,
}

/// 페이지 하나에 연결된 CDP 세션.
pub struct CdpClient {
    ws: Ws,
    next_id: u64,
}

impl CdpClient {
    /// `http://127.0.0.1:<port>`의 DevTools에서 youtube.com 탭을 찾아 연결한다.
    pub async fn attach(port: u16) -> Result<Self, CdpError> {
        let version_url = format!("http://127.0.0.1:{port}/json/version");
        let version: VersionInfo = reqwest::get(&version_url)
            .await
            .map_err(|error| CdpError::Connect {
                url: version_url.clone(),
                detail: error.to_string(),
            })?
            .json()
            .await
            .map_err(|error| CdpError::Target(format!("{version_url}: {error}")))?;
        ensure_chrome_browser(&version.browser, &version.user_agent)?;

        let list_url = format!("http://127.0.0.1:{port}/json/list");
        let response = reqwest::get(&list_url)
            .await
            .map_err(|error| CdpError::Connect {
                url: list_url.clone(),
                detail: error.to_string(),
            })?;
        let targets: Vec<TargetInfo> = response
            .json()
            .await
            .map_err(|error| CdpError::Target(format!("{list_url}: {error}")))?;

        let target = select_target(&targets)?;
        let ws_url = target
            .web_socket_debugger_url
            .clone()
            .ok_or_else(|| CdpError::Target("webSocketDebuggerUrl이 없습니다.".to_string()))?;
        Self::connect(&ws_url).await
    }

    /// WebSocket 주소로 직접 연결한다.
    pub async fn connect(ws_url: &str) -> Result<Self, CdpError> {
        let (ws, _) = connect_async(ws_url)
            .await
            .map_err(|error| CdpError::Connect {
                url: ws_url.to_string(),
                detail: error.to_string(),
            })?;
        Ok(Self { ws, next_id: 1 })
    }

    async fn call(&mut self, method: &str, params: Value) -> Result<Value, CdpError> {
        let id = self.next_id;
        self.next_id += 1;
        let payload = json!({"id": id, "method": method, "params": params});
        self.ws
            .send(Message::Text(payload.to_string()))
            .await
            .map_err(|error| CdpError::Protocol(error.to_string()))?;

        loop {
            let message = tokio::time::timeout(Duration::from_secs(10), self.ws.next())
                .await
                .map_err(|_| CdpError::Timeout(method.to_string()))?
                .ok_or_else(|| CdpError::Protocol("CDP 연결이 닫혔습니다.".to_string()))?
                .map_err(|error| CdpError::Protocol(error.to_string()))?;

            let text = match message {
                Message::Text(text) => text.to_string(),
                Message::Binary(bytes) => String::from_utf8_lossy(&bytes).to_string(),
                Message::Close(_) => {
                    return Err(CdpError::Protocol("CDP가 연결을 닫았습니다.".to_string()));
                }
                _ => continue,
            };
            let value: Value = match serde_json::from_str(&text) {
                Ok(value) => value,
                Err(_) => continue,
            };
            if value["id"].as_u64() != Some(id) {
                continue;
            }
            if let Some(error) = value.get("error") {
                return Err(CdpError::Protocol(error.to_string()));
            }
            return Ok(value["result"].clone());
        }
    }

    /// 페이지에서 표현식을 실행하고 값을 돌려받는다.
    pub async fn evaluate(&mut self, expression: &str) -> Result<Value, CdpError> {
        let result = self
            .call(
                "Runtime.evaluate",
                json!({
                    "expression": expression,
                    "awaitPromise": true,
                    "returnByValue": true,
                    "userGesture": true,
                }),
            )
            .await?;
        if let Some(exception) = result.get("exceptionDetails") {
            return Err(CdpError::Protocol(format!(
                "페이지 스크립트 오류: {exception}"
            )));
        }
        Ok(result["result"]["value"].clone())
    }

    /// 페이지를 이동하고 로딩이 끝날 때까지 기다린다.
    pub async fn navigate(&mut self, url: &str) -> Result<(), CdpError> {
        self.call("Page.navigate", json!({"url": url})).await?;
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(200)).await;
            if self.evaluate("document.readyState").await?.as_str() == Some("complete") {
                return Ok(());
            }
        }
        Err(CdpError::Timeout("페이지 로딩".to_string()))
    }

    /// 세션을 닫는다.
    pub async fn close(mut self) {
        let _ = self.ws.close(None).await;
    }
}

/// 페이지 스크립트가 돌려주는 결과.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct PageResponse {
    /// 요청이 성공했는지.
    pub ok: bool,
    /// HTTP 상태 코드(0이면 요청 전 실패).
    pub status: u16,
    /// 응답 본문 앞부분.
    #[serde(default)]
    pub body: String,
    /// 스크립트 수준 오류 메시지.
    #[serde(default)]
    pub error: String,
}

impl PageResponse {
    /// 본문을 JSON으로 해석한다.
    pub fn json(&self) -> Option<Value> {
        serde_json::from_str(&self.body).ok()
    }

    /// 로그인/권한 문제로 보이는지.
    pub fn is_auth_problem(&self) -> bool {
        matches!(self.status, 401 | 403)
            || self.body.contains("로그인이 필요")
            || self.body.contains("Sign in to")
    }

    /// 요청 제한으로 보이는지.
    pub fn is_rate_limited(&self) -> bool {
        self.status == 429 || self.body.contains("rate limit") || self.body.contains("too many")
    }

    /// 일시적 서버 오류로 보이는지.
    pub fn is_server_error(&self) -> bool {
        self.status >= 500
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_chrome_browser_accepts_chrome_and_rejects_electron() {
        let chrome_ua = "Mozilla/5.0 Chrome/157.0.0.0 Safari/537.36";
        assert!(ensure_chrome_browser("Chrome/157.0.0.0", chrome_ua).is_ok());
        assert!(ensure_chrome_browser("HeadlessChrome/157.0.0.0", chrome_ua).is_ok());
        assert!(ensure_chrome_browser("Chromium/130.0", chrome_ua).is_ok());

        let electron_ua = "Mozilla/5.0 obsidian/1.14 Chrome/150.0 Electron/43.7 Safari/537.36";
        let error = ensure_chrome_browser("Chrome/150.0.7871.250", electron_ua)
            .expect_err("Electron 앱은 거부");
        assert!(error.to_string().contains("Electron"));

        assert!(ensure_chrome_browser("", "").is_err());
    }

    #[test]
    fn select_target_prefers_youtube_and_skips_electron() {
        let electron = vec![TargetInfo {
            r#type: "page".to_string(),
            url: "app://obsidian.md/index.html".to_string(),
            web_socket_debugger_url: Some("ws://x".to_string()),
        }];
        assert!(select_target(&electron).is_err(), "Electron 탭은 제외한다");

        let mixed = vec![
            electron[0].clone(),
            TargetInfo {
                r#type: "page".to_string(),
                url: "about:blank".to_string(),
                web_socket_debugger_url: Some("ws://blank".to_string()),
            },
            TargetInfo {
                r#type: "page".to_string(),
                url: "https://www.youtube.com/".to_string(),
                web_socket_debugger_url: Some("ws://youtube".to_string()),
            },
        ];
        assert_eq!(
            select_target(&mixed)
                .expect("선택 실패")
                .web_socket_debugger_url
                .as_deref(),
            Some("ws://youtube")
        );

        let blank_only = vec![mixed[1].clone()];
        assert_eq!(
            select_target(&blank_only)
                .expect("about:blank 허용")
                .web_socket_debugger_url
                .as_deref(),
            Some("ws://blank")
        );
    }

    #[test]
    fn page_response_flags_auth_and_rate_limit() {
        let auth = PageResponse {
            ok: false,
            status: 401,
            body: String::new(),
            error: String::new(),
        };
        assert!(auth.is_auth_problem());
        let limited = PageResponse {
            ok: false,
            status: 429,
            body: String::new(),
            error: String::new(),
        };
        assert!(limited.is_rate_limited());
        let server = PageResponse {
            ok: false,
            status: 503,
            body: String::new(),
            error: String::new(),
        };
        assert!(server.is_server_error());
    }
}
