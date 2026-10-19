use std::{
    net::{IpAddr, Ipv4Addr},
    path::PathBuf,
    time::{Duration, UNIX_EPOCH},
};

use anyhow::{Result, anyhow};
use if_addrs::get_if_addrs;
use tauri::AppHandle;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use url::Url;

use crate::{models::PeerSource, sync::AppRuntime};

const MAX_REQUEST_BYTES: usize = 16 * 1024;

pub fn private_ipv4_addresses(port: u16) -> Vec<String> {
    let mut result = get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|interface| match interface.ip() {
            IpAddr::V4(ip) if is_lan_ipv4(ip) && !ip.is_loopback() => {
                Some(format!("http://{ip}:{port}"))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    result.sort();
    result.dedup();
    result.truncate(8);
    result
}

fn is_lan_ipv4(ip: Ipv4Addr) -> bool {
    ip.is_private() || ip.is_link_local() || ip.is_loopback()
}

pub async fn run(listener: TcpListener, runtime: AppRuntime, app: AppHandle) {
    let port = listener.local_addr().map(|addr| addr.port()).unwrap_or(0);
    let mut announce = tokio::time::interval(Duration::from_secs(45));
    announce.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = announce.tick() => {
                runtime.announce_peer(&app, port).await;
            }
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let runtime = runtime.clone();
                    let app = app.clone();
                    tauri::async_runtime::spawn(async move {
                        let _ = handle_connection(stream, runtime, app).await;
                    });
                }
                Err(error) => {
                    runtime.set_peer_state(&app, "error", port, &format!("局域网监听失败：{error}")).await;
                    break;
                }
            }
        }
    }
}

async fn handle_connection(
    mut stream: TcpStream,
    runtime: AppRuntime,
    app: AppHandle,
) -> Result<()> {
    let request = read_request(&mut stream).await?;
    let Some((digest, ticket)) = parse_request(&request) else {
        return send_error(&mut stream, 404, "Not Found").await;
    };
    let source = {
        let store = runtime.store.lock().await;
        store.state.sync.peer_sources.get(&digest).cloned()
    };
    let Some(source) = source else {
        return send_error(&mut stream, 404, "Not Found").await;
    };
    if !source_is_valid(&source).await {
        runtime.remove_peer_source(&digest).await;
        return send_error(&mut stream, 404, "Not Found").await;
    }
    let endpoint = match crate::endpoint::resolve(&runtime.store, &runtime.client, true).await {
        Ok(endpoint) => endpoint,
        Err(_) => return send_error(&mut stream, 503, "Service Unavailable").await,
    };
    if endpoint
        .api
        .redeem_peer_ticket(&ticket, &digest)
        .await
        .is_err()
    {
        return send_error(&mut stream, 401, "Unauthorized").await;
    }
    let mut file = tokio::fs::File::open(&source.path).await?;
    let header = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: {}\r\nAccess-Control-Allow-Origin: *\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        source.size,
        safe_mime(&source.mime_type)
    );
    stream.write_all(header.as_bytes()).await?;
    tokio::io::copy(&mut file, &mut stream).await?;
    let _ = stream.shutdown().await;
    {
        let mut store = runtime.store.lock().await;
        store.add_activity_with_route(
            "lan",
            format!("已向局域网设备直传：{}", source.name),
            Some("lan"),
        );
    }
    runtime.emit_state(&app).await;
    Ok(())
}

async fn read_request(stream: &mut TcpStream) -> Result<String> {
    let mut bytes = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let read = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut chunk)).await??;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.windows(4).any(|part| part == b"\r\n\r\n") {
            break;
        }
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err(anyhow!("HTTP 请求过大"));
        }
    }
    String::from_utf8(bytes).map_err(Into::into)
}

fn parse_request(request: &str) -> Option<(String, String)> {
    let mut parts = request.lines().next()?.split_whitespace();
    if parts.next()? != "GET" {
        return None;
    }
    let target = parts.next()?;
    let parsed = Url::parse(&format!("http://peer{target}")).ok()?;
    let digest = parsed
        .path()
        .strip_prefix("/v1/blobs/")?
        .to_ascii_lowercase();
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let ticket = parsed
        .query_pairs()
        .find_map(|(key, value)| (key == "ticket").then(|| value.into_owned()))?;
    if ticket.is_empty() {
        return None;
    }
    Some((digest, ticket))
}

async fn source_is_valid(source: &PeerSource) -> bool {
    let path = PathBuf::from(&source.path);
    let Ok(metadata) = tokio::fs::metadata(path).await else {
        return false;
    };
    metadata.is_file()
        && metadata.len() == source.size
        && metadata_signature(&metadata) == source.signature
}

pub fn metadata_signature(metadata: &std::fs::Metadata) -> String {
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    format!("{}:{modified}", metadata.len())
}

fn safe_mime(value: &str) -> &str {
    if value.bytes().all(|byte| byte.is_ascii_graphic()) && !value.contains(['\r', '\n']) {
        value
    } else {
        "application/octet-stream"
    }
}

async fn send_error(stream: &mut TcpStream, code: u16, label: &str) -> Result<()> {
    let body = format!("{code} {label}\n");
    let response = format!(
        "HTTP/1.1 {code} {label}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    let _ = stream.shutdown().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_lan_ipv4() {
        assert!(is_lan_ipv4(Ipv4Addr::new(10, 0, 0, 1)));
        assert!(is_lan_ipv4(Ipv4Addr::new(172, 16, 0, 1)));
        assert!(is_lan_ipv4(Ipv4Addr::new(192, 168, 1, 8)));
        assert!(is_lan_ipv4(Ipv4Addr::new(169, 254, 1, 8)));
        assert!(!is_lan_ipv4(Ipv4Addr::new(8, 8, 8, 8)));
    }

    #[test]
    fn parses_ticketed_blob_request() {
        let digest = "a".repeat(64);
        let request = format!("GET /v1/blobs/{digest}?ticket=one%2Btwo HTTP/1.1\r\n\r\n");
        assert_eq!(parse_request(&request), Some((digest, "one+two".into())));
        assert!(parse_request("POST /v1/blobs/x HTTP/1.1\r\n\r\n").is_none());
    }
}
