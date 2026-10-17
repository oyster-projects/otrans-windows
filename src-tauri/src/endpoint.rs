use std::{sync::Arc, time::Duration};

use anyhow::{anyhow, Result};
use reqwest::Client;
use tokio::sync::Mutex;

use crate::{api::OTransApi, state::StateStore};

#[derive(Clone)]
pub struct ResolvedEndpoint {
    pub api: OTransApi,
    pub route: String,
    pub server_id: String,
}

pub async fn resolve(store: &Arc<Mutex<StateStore>>, client: &Client, authenticated: bool) -> Result<ResolvedEndpoint> {
    let (settings, token) = {
        let guard = store.lock().await;
        (guard.state.settings.clone(), if authenticated { guard.token()? } else { String::new() })
    };
    let candidates = [
        ("lan", settings.lan_server_url.trim().trim_end_matches('/').to_string(), Duration::from_millis(1600)),
        ("wan", settings.wan_server_url.trim().trim_end_matches('/').to_string(), Duration::from_secs(5)),
    ];
    let mut failures = Vec::new();
    for (route, url, timeout) in candidates {
        if url.is_empty() || (route == "wan" && url == settings.lan_server_url.trim().trim_end_matches('/')) {
            continue;
        }
        let result = async {
            let api = OTransApi::new(url.clone(), token.clone(), client.clone())?;
            let info = api.server_info(timeout).await?;
            if info.server_id.is_empty() { return Err(anyhow!("服务端未返回 serverId，请先升级飞牛端")); }
            if !settings.server_id.is_empty() && settings.server_id != info.server_id {
                return Err(anyhow!("这不是已配对的飞牛服务器"));
            }
            if authenticated { api.device().await?; }
            Ok::<ResolvedEndpoint, anyhow::Error>(ResolvedEndpoint {
                api, route: route.into(), server_id: info.server_id,
            })
        }.await;
        match result {
            Ok(endpoint) => {
                let mut guard = store.lock().await;
                guard.state.settings.server_id.clone_from(&endpoint.server_id);
                guard.state.connection.active_server_url.clone_from(&endpoint.api.server_url);
                guard.state.connection.active_route.clone_from(&endpoint.route);
                let _ = guard.save();
                return Ok(endpoint);
            }
            Err(error) => failures.push(format!("{}：{error}", if route == "lan" { "内网" } else { "外网" })),
        }
    }
    let mut guard = store.lock().await;
    guard.state.connection.active_server_url.clear();
    guard.state.connection.active_route = "offline".into();
    let _ = guard.save();
    if failures.is_empty() { Err(anyhow!("请至少填写一个飞牛服务地址")) }
    else { Err(anyhow!("内外网均无法连接（{}）", failures.join("；"))) }
}
