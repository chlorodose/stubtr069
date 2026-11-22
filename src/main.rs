mod config;
mod connreq;
mod ctx;
mod db;
mod digest;
mod download;
mod fault;
mod http_auth;
mod model;
mod rpc;
mod session;
mod soap;
mod util;
mod xml;

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::Parser;
use tokio::net::{UdpSocket, lookup_host};
use tokio::signal::unix::{SignalKind, signal};
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use config::Config;
use ctx::Ctx;
use db::Db;

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

fn backoff(retry: u32) -> Duration {
    Duration::from_secs((5u64 << retry.min(6)).min(300))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();
    let cfg = Config::parse();

    let db = Db::open(&cfg.db, &cfg.root, cfg.strict_model)
        .with_context(|| format!("opening database {}", cfg.db.display()))?;

    // Seeds only fill gaps; explicit CLI values override what is stored.
    let known_url = cfg
        .acs_url
        .clone()
        .unwrap_or_else(|| db.value(&format!("{}.ManagementServer.URL", cfg.root)));
    let conn_req_url = match &cfg.conn_req_url {
        Some(u) => u.clone(),
        None => {
            let ip = detect_local_ip(&known_url).await;
            let host = if ip.is_ipv6() { format!("[{ip}]") } else { ip.to_string() };
            format!("http://{host}:{}/", cfg.conn_req_listen.port())
        }
    };
    db.apply_seeds(&model::seeds(&cfg, &conn_req_url))?;
    let root = &cfg.root;
    for (name, value) in [
        ("ManagementServer.URL", cfg.acs_url.as_deref()),
        ("ManagementServer.Username", cfg.acs_username.as_deref()),
        ("ManagementServer.Password", cfg.acs_password.as_deref()),
        ("ManagementServer.ConnectionRequestURL", Some(conn_req_url.as_str())),
    ] {
        if let Some(v) = value {
            db.force_value(&format!("{root}.{name}"), v)?;
        }
    }
    if db.value(&format!("{root}.ManagementServer.URL")).is_empty() {
        bail!("no ACS URL configured: pass --acs-url (or STUB_ACS_URL)");
    }

    if db.meta_get("bootstrapped").is_none() {
        db.add_event_unique("0 BOOTSTRAP", "")?;
        db.meta_set("bootstrapped", "1")?;
    } else {
        db.add_event_unique("1 BOOT", "")?;
    }

    let (ctx, mut triggers) = Ctx::new(cfg, db, conn_req_url);
    info!(
        serial = %ctx.cfg.serial_number,
        root = %ctx.cfg.root,
        strict_model = ctx.cfg.strict_model,
        db = %ctx.cfg.db.display(),
        "stubtr069 starting (no local changes will be applied)"
    );

    tokio::spawn({
        let ctx = ctx.clone();
        async move {
            if let Err(e) = connreq::serve(ctx).await {
                error!("connection request server stopped: {e:#}");
                std::process::exit(1);
            }
        }
    });
    download::resume(&ctx);
    ctx.fire(); // startup session

    let mut sigterm = signal(SignalKind::terminate())?;
    let mut retry = 0u32;
    loop {
        tokio::select! {
            t = triggers.recv() => if t.is_none() { break },
            _ = tokio::signal::ctrl_c() => break,
            _ = sigterm.recv() => break,
        }
        // Everything that triggered us is already in the event table.
        while triggers.try_recv().is_ok() {}
        loop {
            match session::run(&ctx, retry).await {
                Ok(()) => {
                    retry = 0;
                    break;
                }
                Err(e) => {
                    let wait = backoff(retry);
                    warn!("session failed: {e:#}; retrying in {}s", wait.as_secs());
                    retry += 1;
                    tokio::select! {
                        _ = tokio::time::sleep(wait) => {}
                        _ = tokio::signal::ctrl_c() => return Ok(()),
                        _ = sigterm.recv() => return Ok(()),
                    }
                }
            }
        }
    }
    info!("shutting down");
    Ok(())
}
