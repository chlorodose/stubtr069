//! Executes ACS RPCs against the virtual data model. Nothing here touches the host.

use std::sync::Arc;
use std::time::Duration;

use tracing::info;

use crate::ctx::{Ctx, unsupported};
use crate::download;
use crate::fault::Fault;
use crate::model;
use crate::soap::{self, Rpc, SUPPORTED_METHODS};
use crate::xml::escape;

const UNKNOWN_TIME: &str = "0001-01-01T00:00:00Z";

/// Returns the SOAP body of the response (or the fault to be reported).
pub fn execute(ctx: &Arc<Ctx>, rpc: Rpc) -> Result<String, Fault> {
    let db = &ctx.db;
    match rpc {
        Rpc::GetRpcMethods => {
            let mut s = format!(
                "<cwmp:GetRPCMethodsResponse><MethodList soap-enc:arrayType=\"xsd:string[{}]\">",
                SUPPORTED_METHODS.len()
            );
            for m in SUPPORTED_METHODS {
                s.push_str(&format!("<string>{m}</string>"));
            }
            s.push_str("</MethodList></cwmp:GetRPCMethodsResponse>");
            Ok(s)
        }
        Rpc::SetParameterValues { values, parameter_key } => {
            let applied = db.set_values(&values, &parameter_key)?;
            for (name, value) in &applied {
                note_unsupported_setting(name, value);
            }
            Ok("<cwmp:SetParameterValuesResponse><Status>0</Status></cwmp:SetParameterValuesResponse>".into())
        }
        Rpc::GetParameterValues { names } => {
            let mut all = Vec::new();
            for n in &names {
                all.extend(db.get_values(n)?);
            }
            Ok(format!(
                "<cwmp:GetParameterValuesResponse>{}</cwmp:GetParameterValuesResponse>",
                soap::parameter_list(&all)
            ))
        }
        Rpc::GetParameterNames { path, next_level } => {
            let names = db.names(&path, next_level)?;
            let mut s = format!(
                "<cwmp:GetParameterNamesResponse><ParameterList soap-enc:arrayType=\"cwmp:ParameterInfoStruct[{}]\">",
                names.len()
            );
            for (n, w) in names {
                s.push_str(&format!(
                    "<ParameterInfoStruct><Name>{}</Name><Writable>{}</Writable></ParameterInfoStruct>",
                    escape(&n),
                    u8::from(w)
                ));
            }
            s.push_str("</ParameterList></cwmp:GetParameterNamesResponse>");
            Ok(s)
        }
        Rpc::SetParameterAttributes { items } => {
            db.set_attrs(&items)?;
            for it in items.iter().filter(|i| i.notification_change && i.notification > 0) {
                unsupported(
                    "value-change notification",
                    format!("{} notification={}", it.name, it.notification),
                );
            }
            Ok("<cwmp:SetParameterAttributesResponse/>".into())
        }
        Rpc::GetParameterAttributes { names } => {
            let mut all = Vec::new();
            for n in &names {
                all.extend(db.get_attrs(n)?);
            }
            let mut s = format!(
                "<cwmp:GetParameterAttributesResponse><ParameterList soap-enc:arrayType=\"cwmp:ParameterAttributeStruct[{}]\">",
                all.len()
            );
            for p in all {
                s.push_str(&format!(
                    "<ParameterAttributeStruct><Name>{}</Name><Notification>{}</Notification>\
                     <AccessList soap-enc:arrayType=\"xsd:string[{}]\">{}</AccessList></ParameterAttributeStruct>",
                    escape(&p.name),
                    p.notification,
                    p.access_list.len(),
                    p.access_list
                        .iter()
                        .map(|a| format!("<string>{}</string>", escape(a)))
                        .collect::<String>()
                ));
            }
            s.push_str("</ParameterList></cwmp:GetParameterAttributesResponse>");
            Ok(s)
        }
        Rpc::AddObject { path, parameter_key } => {
            let n = db.add_object(&path, &parameter_key)?;
            Ok(format!(
                "<cwmp:AddObjectResponse><InstanceNumber>{n}</InstanceNumber><Status>0</Status></cwmp:AddObjectResponse>"
            ))
        }
        Rpc::DeleteObject { path, parameter_key } => {
            db.delete_object(&path, &parameter_key)?;
            Ok("<cwmp:DeleteObjectResponse><Status>0</Status></cwmp:DeleteObjectResponse>".into())
        }
        Rpc::Reboot { command_key } => {
            // Simulated: only the post-reboot session (BOOT + M Reboot) is produced.
            db.add_event("1 BOOT", "").map_err(Fault::internal)?;
            db.add_event("M Reboot", &command_key).map_err(Fault::internal)?;
            info!("simulated reboot: a new session will start shortly");
            ctx.fire_after(Duration::from_secs(2));
            Ok("<cwmp:RebootResponse/>".into())
        }
        Rpc::FactoryReset => {
            db.factory_reset(&model::seeds(&ctx.cfg, &ctx.conn_req_url()))
                .map_err(Fault::internal)?;
            db.add_event("0 BOOTSTRAP", "").map_err(Fault::internal)?;
            info!("simulated factory reset: data model restored to defaults");
            ctx.fire_after(Duration::from_secs(2));
            Ok("<cwmp:FactoryResetResponse/>".into())
        }
        Rpc::Download(req) => {
            if !req.url.to_ascii_lowercase().starts_with("http://") {
                unsupported("download protocol", &req.url);
                return Err(Fault::new(9013, "Unsupported protocol for file transfer"));
            }
            let id = db.insert_download(&req).map_err(Fault::internal)?;
            download::spawn(ctx.clone(), id);
            Ok(format!(
                "<cwmp:DownloadResponse><Status>1</Status><StartTime>{UNKNOWN_TIME}</StartTime>\
                 <CompleteTime>{UNKNOWN_TIME}</CompleteTime></cwmp:DownloadResponse>"
            ))
        }
        Rpc::ScheduleInform { delay_seconds, command_key } => {
            if delay_seconds == 0 {
                return Err(Fault::new(9003, "DelaySeconds must be positive"));
            }
            let ctx = ctx.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(delay_seconds)).await;
                let _ = ctx.db.add_event("3 SCHEDULED", "");
                let _ = ctx.db.add_event("M ScheduleInform", &command_key);
                ctx.fire();
            });
            Ok("<cwmp:ScheduleInformResponse/>".into())
        }
        Rpc::Unsupported { method } => {
            unsupported("RPC method", &method);
            Err(Fault::new(9000, "Method not supported"))
        }
    }
}

/// Settings the ACS can store but that would need behavior this stub lacks.
fn note_unsupported_setting(name: &str, value: &str) {
    if name.ends_with("ManagementServer.PeriodicInformEnable") && value == "1" {
        unsupported("periodic inform", "PeriodicInformEnable set to true; value is stored only");
    }
}
