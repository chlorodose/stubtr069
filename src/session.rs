//! One CWMP session: Inform, TransferComplete, then serve ACS RPCs until the ACS is done.

use std::collections::VecDeque;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use reqwest::Method;
use tracing::{info, warn};

use crate::connurl;
use crate::ctx::{Ctx, unsupported};
use crate::db::Download;
use crate::fault::Fault;
use crate::http_auth::AuthClient;
use crate::model;
use crate::rpc;
use crate::soap::{self, InformData};
use crate::util::now_iso;
use crate::xml::escape;

const MAX_ROUNDS: usize = 1000;

/// What the next message from the ACS is expected to answer.
enum Awaiting {
    InformResponse,
    TransferCompleteResponse(i64),
    Request,
}

pub async fn run(ctx: &Arc<Ctx>, retry_count: u32) -> Result<()> {
    if let Err(e) = connurl::refresh(ctx).await {
        warn!("cannot refresh ConnectionRequestURL, keeping the previous one: {e:#}");
    }
    let events = ctx.db.pending_events()?;
    if events.is_empty() {
        // A trigger can outlive its events when another session already reported them.
        tracing::debug!("no pending events, skipping session");
        return Ok(());
    }
    let session = ctx.db.next_session_id()?;
    let root = ctx.db.root().to_string();
    let url = ctx.db.value(&format!("{root}.ManagementServer.URL"));
    if url.is_empty() {
        bail!("ManagementServer.URL is empty; pass --acs-url");
    }
    if !url.to_ascii_lowercase().starts_with("http://") {
        unsupported("ACS transport", format!("{url} (only http:// is implemented)"));
        bail!("unsupported ACS URL scheme: {url}");
    }
    let mut client = AuthClient::new(
        &ctx.db.value(&format!("{root}.ManagementServer.Username")),
        &ctx.db.value(&format!("{root}.ManagementServer.Password")),
        true,
    )?;

    let max_event = events.iter().map(|e| e.id).max().unwrap_or(0);
    let event_list: Vec<(String, String)> = events
        .iter()
        .map(|e| (e.code.clone(), e.command_key.clone()))
        .collect();
    let has_transfer = events.iter().any(|e| e.code == "7 TRANSFER COMPLETE");
    let inform_params: Vec<_> = model::inform_names(&root)
        .iter()
        .filter_map(|n| ctx.db.param(n))
        .collect();
    let inform = soap::envelope(
        &format!("stub-{session}-0"),
        &soap::inform_body(&InformData {
            manufacturer: &ctx.cfg.manufacturer,
            oui: &ctx.cfg.oui,
            product_class: &ctx.cfg.product_class,
            serial_number: &ctx.cfg.serial_number,
            events: &event_list,
            current_time: &now_iso(),
            retry_count,
            params: &inform_params,
        }),
    );
    info!(
        session,
        %url,
        events = ?event_list.iter().map(|e| e.0.as_str()).collect::<Vec<_>>(),
        "session started"
    );

    let mut body = Some(inform);
    let mut awaiting = Awaiting::InformResponse;
    let mut transfers: VecDeque<Download> = VecDeque::new();
    let mut seq = 0u32;
    let mut commands = 0u32;

    for _ in 0..MAX_ROUNDS {
        let resp = client
            .send(Method::POST, &url, body.take())
            .await
            .context("HTTP request to ACS failed")?;
        let status = resp.status();
        if !status.is_success() {
            bail!("ACS answered HTTP {status}");
        }
        let text = resp.text().await.context("reading ACS response")?;
        if text.trim().is_empty() {
            if matches!(awaiting, Awaiting::InformResponse) {
                bail!("ACS closed the session without answering Inform");
            }
            ctx.db.ack_events(max_event)?;
            info!(session, commands, "session finished");
            return Ok(());
        }

        let msg = soap::parse_message(&text).map_err(|e| anyhow::anyhow!("bad SOAP from ACS: {e}"))?;
        let name = msg.body.name.as_str();

        if name == "Fault" || name.ends_with("Response") {
            let faulted = name == "Fault";
            if faulted {
                warn!(session, id = %msg.id, "ACS returned a SOAP fault: {text}");
            }
            match awaiting {
                Awaiting::InformResponse => {
                    if faulted || name != "InformResponse" {
                        bail!("Inform rejected by ACS ({name})");
                    }
                    if has_transfer {
                        transfers.extend(ctx.db.downloads_where(
                            "state IN ('completed', 'failed') AND reported = 0",
                        )?);
                    }
                }
                Awaiting::TransferCompleteResponse(id) => {
                    // A fault is not retried, otherwise a bad ACS would loop forever.
                    ctx.db.download_reported(id)?;
                }
                Awaiting::Request => warn!(session, "unexpected {name} from ACS, ignored"),
            }
            match transfers.pop_front() {
                Some(d) => {
                    seq += 1;
                    body = Some(soap::envelope(
                        &format!("stub-{session}-{seq}"),
                        &transfer_complete_body(&d),
                    ));
                    awaiting = Awaiting::TransferCompleteResponse(d.id);
                }
                None => awaiting = Awaiting::Request, // empty POST: CPE has nothing more to send
            }
            continue;
        }

        // An RPC request from the ACS.
        let rpc = soap::parse_rpc(&msg.body);
        commands += 1;
        let summary = rpc.summary();
        info!(
            target: "acs_command",
            session,
            rpc_id = %msg.id,
            method = rpc.method(),
            "ACS command {}({summary})",
            rpc.method()
        );
        if let Err(e) = ctx.db.log_command(&now_iso(), session, &msg.id, rpc.method(), &summary, &text) {
            warn!("cannot record command in DB: {e:#}");
        }
        tracing::debug!(target: "acs_command", raw = %text, "raw ACS command");

        let reply = match rpc::execute(ctx, rpc) {
            Ok(b) => b,
            Err(f) => {
                log_fault(&f);
                soap::fault_body(&f)
            }
        };
        body = Some(soap::envelope(&msg.id, &reply));
        awaiting = Awaiting::Request;
    }
    bail!("session exceeded {MAX_ROUNDS} message exchanges")
}

fn log_fault(f: &Fault) {
    warn!(target: "acs_command", code = f.code, "RPC failed with fault: {}", f.text);
}

fn transfer_complete_body(d: &Download) -> String {
    format!(
        "<cwmp:TransferComplete><CommandKey>{}</CommandKey><FaultStruct><FaultCode>{}</FaultCode>\
         <FaultString>{}</FaultString></FaultStruct><StartTime>{}</StartTime><CompleteTime>{}</CompleteTime>\
         </cwmp:TransferComplete>",
        escape(&d.command_key),
        d.fault_code,
        escape(&d.fault_string),
        d.start_time,
        d.complete_time
    )
}
