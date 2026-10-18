use std::{path::Path, time::Duration};

use anyhow::{Context, Result, anyhow};
use reqwest::{Client, Method, Response};
use serde::{Serialize, de::DeserializeOwned};
use url::Url;

use crate::models::{
    CreateTransferResponse, Device, ErrorResponse, EventPage, PairResponse, PeerSourcesResponse,
    ServerInfo, Transfer, WsTicket,
};

#[derive(Clone)]
pub struct OTransApi {
    pub server_url: String,
    token: String,
    client: Client,
}

impl OTransApi {
    pub fn new(
        server_url: impl Into<String>,
        token: impl Into<String>,
        client: Client,
    ) -> Result<Self> {
        let server_url = server_url.into().trim().trim_end_matches('/').to_string();
        let parsed = Url::parse(&server_url).context("服务地址格式不正确")?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(anyhow!("服务地址必须以 http:// 或 https:// 开头"));
        }
        Ok(Self {
            server_url,
            token: token.into(),
            client,
        })
    }

    pub async fn server_info(&self, timeout: Duration) -> Result<ServerInfo> {
        self.request(
            Method::GET,
            "/api/v1/server-info",
            Option::<&()>::None,
            false,
            timeout,
        )
        .await
    }

    pub async fn pair(&self, code: &str, name: &str) -> Result<PairResponse> {
        #[derive(Serialize)]
        struct Body<'a> {
            code: &'a str,
            name: &'a str,
            platform: &'static str,
        }
        self.request(
            Method::POST,
            "/api/v1/pair",
            Some(&Body {
                code,
                name,
                platform: "windows",
            }),
            false,
            Duration::from_secs(30),
        )
        .await
    }

    pub async fn device(&self) -> Result<Device> {
        self.request(
            Method::GET,
            "/api/v1/devices/me",
            Option::<&()>::None,
            true,
            Duration::from_secs(10),
        )
        .await
    }

    pub async fn events(&self, after: i64) -> Result<EventPage> {
        self.request(
            Method::GET,
            &format!("/api/v1/events?after={after}&limit=100"),
            Option::<&()>::None,
            true,
            Duration::from_secs(30),
        )
        .await
    }

    pub async fn update_cursor(&self, cursor: i64) -> Result<()> {
        #[derive(Serialize)]
        struct Body {
            cursor: i64,
        }
        let _: serde_json::Value = self
            .request(
                Method::PUT,
                "/api/v1/devices/me/cursor",
                Some(&Body { cursor }),
                true,
                Duration::from_secs(30),
            )
            .await?;
        Ok(())
    }

    pub async fn create_transfer<T: Serialize>(&self, body: &T) -> Result<CreateTransferResponse> {
        self.request(
            Method::POST,
            "/api/v1/transfers",
            Some(body),
            true,
            Duration::from_secs(30),
        )
        .await
    }

    pub async fn commit_transfer(&self, id: &str) -> Result<Transfer> {
        self.request(
            Method::POST,
            &format!("/api/v1/transfers/{id}/commit"),
            Option::<&()>::None,
            true,
            Duration::from_secs(30),
        )
        .await
    }

    pub async fn websocket_ticket(&self) -> Result<WsTicket> {
        self.request(
            Method::POST,
            "/api/v1/ws-ticket",
            Option::<&()>::None,
            true,
            Duration::from_secs(10),
        )
        .await
    }

    pub fn websocket_url(&self, ticket: &str) -> Result<Url> {
        let mut url = Url::parse(&self.server_url)?;
        url.set_scheme(if url.scheme() == "https" { "wss" } else { "ws" })
            .map_err(|_| anyhow!("无法创建 WebSocket 地址"))?;
        let path = format!("{}/api/v1/ws", url.path().trim_end_matches('/'));
        url.set_path(&path);
        url.query_pairs_mut().clear().append_pair("ticket", ticket);
        Ok(url)
    }

    pub async fn put_blob(&self, digest: &str, path: &Path, mime_type: &str) -> Result<()> {
        let content = tokio::fs::read(path).await?;
        let response = self
            .client
            .put(format!("{}/api/v1/blobs/{digest}", self.server_url))
            .bearer_auth(&self.token)
            .header(reqwest::header::CONTENT_TYPE, mime_type)
            .timeout(Duration::from_secs(180))
            .body(content)
            .send()
            .await?;
        self.ensure_success(response).await.map(|_| ())
    }

    pub async fn get_blob(&self, digest: &str, name: &str) -> Result<Response> {
        let response = self
            .client
            .get(format!("{}/api/v1/blobs/{digest}", self.server_url))
            .query(&[("name", name)])
            .bearer_auth(&self.token)
            .timeout(Duration::from_secs(180))
            .send()
            .await?;
        self.ensure_success(response).await
    }

    pub async fn announce_peer(&self, addresses: &[String], digests: &[String]) -> Result<()> {
        #[derive(Serialize)]
        struct Body<'a> {
            addresses: &'a [String],
            digests: &'a [String],
        }
        let _: serde_json::Value = self
            .request(
                Method::POST,
                "/api/v1/lan/announce",
                Some(&Body { addresses, digests }),
                true,
                Duration::from_secs(10),
            )
            .await?;
        Ok(())
    }

    pub async fn peer_sources(&self, digest: &str) -> Result<PeerSourcesResponse> {
        self.request(
            Method::GET,
            &format!("/api/v1/lan/sources/{digest}"),
            Option::<&()>::None,
            true,
            Duration::from_secs(10),
        )
        .await
    }

    pub async fn redeem_peer_ticket(&self, ticket: &str, digest: &str) -> Result<()> {
        #[derive(Serialize)]
        struct Body<'a> {
            ticket: &'a str,
            digest: &'a str,
        }
        let response = self
            .client
            .post(format!("{}/api/v1/lan/tickets/redeem", self.server_url))
            .bearer_auth(&self.token)
            .json(&Body { ticket, digest })
            .timeout(Duration::from_secs(10))
            .send()
            .await?;
        self.ensure_success(response).await.map(|_| ())
    }

    async fn request<T: DeserializeOwned, B: Serialize + ?Sized>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
        authenticated: bool,
        timeout: Duration,
    ) -> Result<T> {
        let mut request = self
            .client
            .request(method, format!("{}{}", self.server_url, path))
            .timeout(timeout);
        if authenticated {
            if self.token.is_empty() {
                return Err(anyhow!("尚未配对，请先生成配对码并连接"));
            }
            request = request.bearer_auth(&self.token);
        }
        if let Some(value) = body {
            request = request.json(value);
        }
        let response = self.ensure_success(request.send().await?).await?;
        if response.status() == reqwest::StatusCode::NO_CONTENT {
            return serde_json::from_value(serde_json::Value::Null).context("服务返回了空响应");
        }
        Ok(response.json().await?)
    }

    async fn ensure_success(&self, response: Response) -> Result<Response> {
        if response.status().is_success() {
            return Ok(response);
        }
        let status = response.status();
        let payload = response.json::<ErrorResponse>().await.ok();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(anyhow!("设备令牌无效，请重新配对"));
        }
        Err(anyhow!(
            payload
                .and_then(|value| value.error)
                .unwrap_or_else(|| format!("服务返回 {status}"))
        ))
    }
}
