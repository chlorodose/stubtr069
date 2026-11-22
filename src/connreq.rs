//! Connection Request server: lets the ACS ask the CPE to open a session.

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use tokio::net::TcpListener;
use tracing::{info, warn};

use crate::ctx::Ctx;
use crate::util::base64_decode;

pub async fn serve(ctx: Arc<Ctx>) -> anyhow::Result<()> {
    let listener = TcpListener::bind(ctx.cfg.conn_req_listen).await?;
    info!(listen = %ctx.cfg.conn_req_listen, url = %ctx.conn_req_url(), "connection request server up");
    let app = Router::new().fallback(handle).with_state(ctx);
    axum::serve(listener, app).await?;
    Ok(())
}

fn authorized(ctx: &Ctx, method: &Method, headers: &HeaderMap) -> bool {
    let root = ctx.db.root();
    let user = ctx.db.value(&format!("{root}.ManagementServer.ConnectionRequestUsername"));
    let pass = ctx.db.value(&format!("{root}.ManagementServer.ConnectionRequestPassword"));
    let Some(h) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let h = h.trim();
    if let Some(b64) = h.get(..6).filter(|p| p.eq_ignore_ascii_case("basic ")).map(|_| &h[6..]) {
        return base64_decode(b64.trim())
            .and_then(|raw| String::from_utf8(raw).ok())
            .is_some_and(|s| s == format!("{user}:{pass}"));
    }
    ctx.verifier.verify(h, method.as_str(), &user, &pass)
}

async fn handle(
    State(ctx): State<Arc<Ctx>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    if !authorized(&ctx, &method, &headers) {
        warn!(%method, %uri, "connection request rejected: authentication required");
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, ctx.verifier.challenge())],
        )
            .into_response();
    }
    info!(%method, %uri, "connection request accepted");
    if let Err(e) = ctx.db.add_event("6 CONNECTION REQUEST", "") {
        warn!("cannot record event: {e:#}");
    }
    ctx.fire();
    StatusCode::OK.into_response()
}
