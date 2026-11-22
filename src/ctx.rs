use std::fmt::Display;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tracing::warn;

use crate::config::Config;
use crate::db::Db;
use crate::digest::Verifier;

pub struct Ctx {
    pub cfg: Config,
    pub db: Db,
    pub conn_req_url: String,
    pub verifier: Verifier,
    trigger: mpsc::Sender<()>,
}

impl Ctx {
    pub fn new(cfg: Config, db: Db, conn_req_url: String) -> (Arc<Self>, mpsc::Receiver<()>) {
        let (tx, rx) = mpsc::channel(16);
        let ctx = Arc::new(Self {
            cfg,
            db,
            conn_req_url,
            verifier: Verifier::new("stubtr069"),
            trigger: tx,
        });
        (ctx, rx)
    }

    /// Asks the main loop to open a CWMP session (pending events are already in the DB).
    pub fn fire(&self) {
        let _ = self.trigger.try_send(());
    }

    pub fn fire_after(self: &Arc<Self>, delay: Duration) {
        let ctx = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            ctx.fire();
        });
    }
}

/// Logs a feature the ACS (or its URLs) asked for that this stub does not implement.
pub fn unsupported(feature: &str, detail: impl Display) {
    warn!(target: "unsupported", feature, "requested but not supported: {detail}");
}
