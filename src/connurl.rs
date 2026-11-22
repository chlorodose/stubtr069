//! Determination of the advertised ConnectionRequestURL: fixed, auto-detected or script-provided.

use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::net::{UdpSocket, lookup_host};
use tokio::process::Command;
use tracing::warn;

use crate::config::Config;
use crate::ctx::Ctx;

const SCRIPT_TIMEOUT: Duration = Duration::from_secs(10);

/// Determines the local address the ACS would see, by "connecting" a UDP socket to it.
async fn detect_local_ip(acs_url: &str) -> IpAddr {
    let probe = async {
        let url = reqwest::Url::parse(acs_url).ok()?;
        let addr = lookup_host((url.host_str()?, url.port_or_known_default()?))
            .await
            .ok()?
            .next()?;
        let bind = if addr.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };
        let sock = UdpSocket::bind(bind).await.ok()?;
        sock.connect(addr).await.ok()?;
        Some(sock.local_addr().ok()?.ip())
    };
    probe.await.unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST))
}

pub async fn auto_url(cfg: &Config, acs_url: &str) -> String {
    let ip = detect_local_ip(acs_url).await;
    let host = if ip.is_ipv6() { format!("[{ip}]") } else { ip.to_string() };
    format!("http://{host}:{}/", cfg.conn_req_listen.port())
}

/// Validates the script output: the first non-empty line must be an http(s) URL.
pub fn parse_url(stdout: &str) -> Result<String> {
    let line = stdout
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .context("script printed no URL")?;
    let url = reqwest::Url::parse(line).with_context(|| format!("invalid URL {line:?}"))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        bail!("not an http(s) URL: {line:?}");
    }
    Ok(line.to_string())
}

/// Runs the user script; it learns the context through `STUB_ACS_URL`,
/// `STUB_CONN_REQ_LISTEN` and `STUB_CONN_REQ_PORT`.
pub async fn run_script(script: &Path, cfg: &Config, acs_url: &str) -> Result<String> {
    let mut cmd = Command::new(script);
    cmd.env("STUB_ACS_URL", acs_url)
        .env("STUB_CONN_REQ_LISTEN", cfg.conn_req_listen.to_string())
        .env("STUB_CONN_REQ_PORT", cfg.conn_req_listen.port().to_string())
        .kill_on_drop(true);
    let out = tokio::time::timeout(SCRIPT_TIMEOUT, cmd.output())
        .await
        .with_context(|| format!("{} timed out after {}s", script.display(), SCRIPT_TIMEOUT.as_secs()))?
        .with_context(|| format!("cannot run {}", script.display()))?;
    if !out.status.success() {
        bail!(
            "{} exited with {}: {}",
            script.display(),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    parse_url(&String::from_utf8_lossy(&out.stdout))
}

/// Re-evaluates the script (if configured). On change the stored parameter is updated and a
/// `4 VALUE CHANGE` event queued. Returns whether the URL changed.
pub async fn refresh(ctx: &Arc<Ctx>) -> Result<bool> {
    let Some(script) = &ctx.cfg.conn_req_url_script else {
        return Ok(false);
    };
    let acs_url = ctx.db.value(&format!("{}.ManagementServer.URL", ctx.db.root()));
    let url = run_script(script, &ctx.cfg, &acs_url).await?;
    if !ctx.set_conn_req_url(&url)? {
        return Ok(false);
    }
    ctx.db.add_event_unique("4 VALUE CHANGE", "")?;
    Ok(true)
}

/// Polls the script so an address change reaches the ACS even when no session is running.
pub async fn watch(ctx: Arc<Ctx>) {
    let every = Duration::from_secs(ctx.cfg.conn_req_url_interval);
    loop {
        tokio::time::sleep(every).await;
        match refresh(&ctx).await {
            Ok(true) => ctx.fire(),
            Ok(false) => {}
            Err(e) => warn!("ConnectionRequestURL script failed, keeping the previous URL: {e:#}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_first_nonempty_line() {
        assert_eq!(parse_url("\n  http://10.0.0.2:7547/ \nignored\n").unwrap(), "http://10.0.0.2:7547/");
        assert!(parse_url("https://[fd00::1]:7547/x").is_ok());
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_url("").is_err());
        assert!(parse_url("10.0.0.2:7547").is_err());
        assert!(parse_url("ftp://10.0.0.2/").is_err());
    }
}
