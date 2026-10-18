use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;
use uuid::Uuid;

use crate::{api::OTransApi, models::Transfer};

const MAX_FILE_BYTES: u64 = 128 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct PreparedFile {
    pub path: PathBuf,
    pub name: String,
    pub mime_type: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateBody {
    client_transfer_id: String,
    title: String,
    items: Vec<CreateItem>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateItem {
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    mime_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sha256: Option<String>,
}

pub async fn prepare_files(paths: &[String]) -> Result<Vec<PreparedFile>> {
    let mut unique = HashSet::new();
    let mut prepared = Vec::new();
    for raw in paths {
        let path = PathBuf::from(raw);
        let canonical = tokio::fs::canonicalize(&path)
            .await
            .with_context(|| format!("无法读取 {}", path.display()))?;
        if !unique.insert(canonical.clone()) {
            continue;
        }
        let metadata = tokio::fs::metadata(&canonical).await?;
        if !metadata.is_file() {
            continue;
        }
        if metadata.len() > MAX_FILE_BYTES {
            return Err(anyhow!(
                "{} 超过单文件 128 MiB 限制",
                display_name(&canonical)
            ));
        }
        let before_modified = metadata.modified().ok();
        let digest = sha256_file(&canonical).await?;
        let after = tokio::fs::metadata(&canonical).await?;
        if metadata.len() != after.len() || before_modified != after.modified().ok() {
            return Err(anyhow!("{} 仍在写入，请稍后重试", display_name(&canonical)));
        }
        prepared.push(PreparedFile {
            name: display_name(&canonical),
            mime_type: mime_guess::from_path(&canonical)
                .first_or_octet_stream()
                .essence_str()
                .into(),
            size: after.len(),
            sha256: digest,
            path: canonical,
        });
    }
    Ok(prepared)
}

pub async fn send_draft(api: &OTransApi, text: &str, files: &[PreparedFile]) -> Result<Transfer> {
    let text = text.trim();
    let count = files.len() + usize::from(!text.is_empty());
    if count == 0 {
        return Err(anyhow!("请先添加文本、图片或文件"));
    }
    if count > 64 {
        return Err(anyhow!("一条中转记录最多包含 64 项"));
    }
    let mut items = Vec::with_capacity(count);
    if !text.is_empty() {
        items.push(CreateItem {
            kind: "text".into(),
            text: Some(text.into()),
            name: None,
            mime_type: "text/plain".into(),
            size: None,
            sha256: None,
        });
    }
    for file in files {
        items.push(CreateItem {
            kind: "file".into(),
            text: None,
            name: Some(file.name.clone()),
            mime_type: file.mime_type.clone(),
            size: Some(file.size),
            sha256: Some(file.sha256.clone()),
        });
    }
    let title = match (text.is_empty(), files.len()) {
        (false, 0) => "来自 Windows 的文本".into(),
        (false, n) => format!("来自 Windows：文本和 {n} 个文件"),
        (true, 1) => format!("来自 Windows：{}", files[0].name),
        (true, n) => format!("来自 Windows 的 {n} 个文件"),
    };
    let created = api
        .create_transfer(&CreateBody {
            client_transfer_id: Uuid::new_v4().to_string(),
            title,
            items,
        })
        .await?;
    let missing: HashSet<&str> = created.missing_blobs.iter().map(String::as_str).collect();
    let mut uploaded = HashSet::new();
    for file in files {
        if missing.contains(file.sha256.as_str()) && uploaded.insert(file.sha256.clone()) {
            api.put_blob(&file.sha256, &file.path, &file.mime_type)
                .await?;
        }
    }
    api.commit_transfer(&created.transfer.id).await
}

pub async fn sha256_file(path: &Path) -> Result<String> {
    let mut file = tokio::fs::File::open(path).await?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("未命名文件")
        .to_string()
}
