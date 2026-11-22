use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;

/// Stub TR-069 (CWMP) CPE daemon: answers ACS RPCs against a SQLite-backed
/// virtual data model without touching the local system.
#[derive(Parser, Debug, Clone)]
#[command(version)]
pub struct Config {
    /// ACS URL (http only). Overrides the stored ManagementServer.URL when given.
    #[arg(long, env = "STUB_ACS_URL")]
    pub acs_url: Option<String>,

    /// Username used to authenticate to the ACS (Basic/Digest).
    #[arg(long, env = "STUB_ACS_USERNAME")]
    pub acs_username: Option<String>,

    /// Password used to authenticate to the ACS (Basic/Digest).
    #[arg(long, env = "STUB_ACS_PASSWORD")]
    pub acs_password: Option<String>,

    /// SQLite database path.
    #[arg(long, env = "STUB_DB", default_value = "stubtr069.db")]
    pub db: PathBuf,

    /// Directory receiving files fetched by Download RPCs (never applied).
    #[arg(long, env = "STUB_DOWNLOAD_DIR", default_value = "downloads")]
    pub download_dir: PathBuf,

    /// Listen address of the Connection Request server.
    #[arg(long, env = "STUB_CONN_REQ_LISTEN", default_value = "0.0.0.0:7547")]
    pub conn_req_listen: SocketAddr,

    /// ConnectionRequestURL advertised to the ACS (auto-detected when omitted).
    #[arg(long, env = "STUB_CONN_REQ_URL")]
    pub conn_req_url: Option<String>,

    /// Connection Request username expected from the ACS.
    #[arg(long, env = "STUB_CONN_REQ_USERNAME", default_value = "stub")]
    pub conn_req_username: String,

    /// Connection Request password expected from the ACS.
    #[arg(long, env = "STUB_CONN_REQ_PASSWORD", default_value = "stub")]
    pub conn_req_password: String,

    /// Root object of the virtual data model.
    #[arg(long, default_value = "Device", value_parser = ["Device", "InternetGatewayDevice"])]
    pub root: String,

    #[arg(long, default_value = "StubVendor")]
    pub manufacturer: String,

    #[arg(long, default_value = "001122")]
    pub oui: String,

    #[arg(long, default_value = "StubCPE")]
    pub product_class: String,

    #[arg(long, default_value = "StubModel")]
    pub model_name: String,

    #[arg(long, default_value = "STUB000001")]
    pub serial_number: String,
}
