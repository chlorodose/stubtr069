//! Fetches resources named by Download RPCs into the download directory.
//! Files are only stored and hashed; they are never applied to the host.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::{Method, StatusCode};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tracing::{info, warn};

use crate::ctx::Ctx;
use crate::db::Download;
use crate::http_auth::AuthClient;
use crate::util::{now_iso, unix_now};

pub fn spawn(ctx: Arc<Ctx>, id: i64) {
    tokio::spawn(async move {
        if let Err(e) = run(&ctx, id).await {
            warn!(download = id, "download task failed: {e:#}");
        }
    });
}

/// Restarts transfers that were interrupted by a daemon restart.
pub fn resume(ctx: &Arc<Ctx>) {
    match ctx.db.downloads_where("state IN ('queued', 'downloading')") {
        Ok(list) => {
            for d in list {
                info!(download = d.id, url = %d.url, "resuming queued download");
                spawn(ctx.clone(), d.id);
            }
        }
        Err(e) => warn!("cannot list pending downloads: {e:#}"),
    }
}

async fn run(ctx: &Arc<Ctx>, id: i64) -> anyhow::Result<()> {
    let Some(d) = ctx.db.download(id) else {
        return Ok(());
    };
    let wait = d.received_at + d.delay_seconds - unix_now();
    if wait > 0 {
        tokio::time::sleep(Duration::from_secs(wait as u64)).await;
    }
    ctx.db.download_started(id, &now_iso())?;
    info!(download = id, url = %d.url, "download started");

    let (fault, path, size, sha) = match fetch(ctx, &d).await {
        Ok((path, size, sha)) => (None, path, size, sha),
        Err((code, text)) => (Some((code, text)), PathBuf::new(), 0, String::new()),
    };
    let path = path.to_string_lossy();
    ctx.db.download_finished(
        id,
        &now_iso(),
        fault.as_ref().map(|(c, t)| (*c, t.as_str())),
        &path,
        size,
        &sha,
    )?;
    match &fault {
        None => info!(download = id, %path, size, sha256 = %sha, "download completed"),
        Some((c, t)) => warn!(download = id, fault = c, "download failed: {t}"),
    }
    ctx.db.add_event("7 TRANSFER COMPLETE", "")?;
    ctx.db.add_event("M Download", &d.command_key)?;
    ctx.fire();
    Ok(())
}

fn local_name(d: &Download) -> String {
    let base = if d.target_file_name.is_empty() {
        d.url
            .split(['?', '#'])
            .next()
            .and_then(|u| u.rsplit('/').next())
            .unwrap_or("")
    } else {
        &d.target_file_name
    };
    let clean: String = base
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' })
        .collect();
    let clean = clean.trim_start_matches('.');
    format!("{}_{}", d.id, if clean.is_empty() { "file" } else { clean })
}

type Failure = (u32, String);

async fn fetch(ctx: &Arc<Ctx>, d: &Download) -> Result<(PathBuf, u64, String), Failure> {
    let io = |e: std::io::Error| (9017u32, format!("cannot write file: {e}"));
    tokio::fs::create_dir_all(&ctx.cfg.download_dir).await.map_err(io)?;
    let path = ctx.cfg.download_dir.join(local_name(d));

    let mut client = AuthClient::new(&d.username, &d.password, false)
        .map_err(|e| (9010, format!("{e:#}")))?;
    let resp = client
        .send(Method::GET, &d.url, None)
        .await
        .map_err(|e| (9016, format!("cannot contact file server: {e}")))?;
    match resp.status() {
        s if s.is_success() => {}
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            return Err((9012, format!("file server authentication failure: {}", resp.status())));
        }
        s => return Err((9010, format!("download failure: HTTP {s}"))),
    }

    let mut file = tokio::fs::File::create(&path).await.map_err(io)?;
    let mut hasher = Sha256::new();
    let mut size = 0u64;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| (9018u32, format!("incomplete download: {e}")))?;
        hasher.update(&chunk);
        size += chunk.len() as u64;
        file.write_all(&chunk).await.map_err(io)?;
    }
    file.flush().await.map_err(io)?;
    if d.file_size > 0 && d.file_size as u64 != size {
        return Err((9019, format!("size mismatch: expected {}, got {size}", d.file_size)));
    }
    Ok((path, size, hex::encode(hasher.finalize())))
}
