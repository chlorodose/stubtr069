//! Default contents of the virtual data model.

use crate::config::Config;

pub struct Seed {
    pub name: String,
    pub value: String,
    pub ty: &'static str,
    pub writable: bool,
}

const STR: &str = "xsd:string";
const BOOL: &str = "xsd:boolean";
const UINT: &str = "xsd:unsignedInt";

pub fn seeds(cfg: &Config, conn_req_url: &str) -> Vec<Seed> {
    let root = &cfg.root;
    let mut v = Vec::new();
    let mut add = |suffix: &str, value: &str, ty: &'static str, writable: bool| {
        v.push(Seed {
            name: format!("{root}.{suffix}"),
            value: value.to_string(),
            ty,
            writable,
        });
    };

    add("DeviceInfo.Manufacturer", &cfg.manufacturer, STR, false);
    add("DeviceInfo.ManufacturerOUI", &cfg.oui, STR, false);
    add("DeviceInfo.ModelName", &cfg.model_name, STR, false);
    add("DeviceInfo.Description", "Stub TR-069 CPE", STR, false);
    add("DeviceInfo.ProductClass", &cfg.product_class, STR, false);
    add("DeviceInfo.SerialNumber", &cfg.serial_number, STR, false);
    add("DeviceInfo.HardwareVersion", "1.0", STR, false);
    add("DeviceInfo.SoftwareVersion", "1.0.0", STR, false);
    add("DeviceInfo.ProvisioningCode", "", STR, true);
    add("DeviceInfo.UpTime", "0", UINT, false);

    if root == "Device" {
        add("RootDataModelVersion", "2.15", STR, false);
        add("ManagementServer.EnableCWMP", "1", BOOL, true);
    } else {
        add("DeviceSummary", "InternetGatewayDevice:1.4[](Baseline:1)", STR, false);
        add("DeviceInfo.SpecVersion", "1.0", STR, false);
    }

    add(
        "ManagementServer.URL",
        cfg.acs_url.as_deref().unwrap_or(""),
        STR,
        true,
    );
    add(
        "ManagementServer.Username",
        cfg.acs_username.as_deref().unwrap_or(""),
        STR,
        true,
    );
    add(
        "ManagementServer.Password",
        cfg.acs_password.as_deref().unwrap_or(""),
        STR,
        true,
    );
    add("ManagementServer.PeriodicInformEnable", "0", BOOL, true);
    add("ManagementServer.PeriodicInformInterval", "86400", UINT, true);
    add("ManagementServer.PeriodicInformTime", "0001-01-01T00:00:00Z", "xsd:dateTime", true);
    add("ManagementServer.ParameterKey", "", STR, false);
    add("ManagementServer.ConnectionRequestURL", conn_req_url, STR, false);
    add(
        "ManagementServer.ConnectionRequestUsername",
        &cfg.conn_req_username,
        STR,
        true,
    );
    add(
        "ManagementServer.ConnectionRequestPassword",
        &cfg.conn_req_password,
        STR,
        true,
    );
    v
}

/// Parameters reported in every Inform (TR-069 section A.3.3.1).
pub fn inform_names(root: &str) -> Vec<String> {
    let mut suffixes = vec![
        "DeviceInfo.HardwareVersion",
        "DeviceInfo.SoftwareVersion",
        "DeviceInfo.ProvisioningCode",
        "ManagementServer.ConnectionRequestURL",
        "ManagementServer.ParameterKey",
    ];
    if root == "Device" {
        suffixes.insert(0, "RootDataModelVersion");
    } else {
        suffixes.insert(0, "DeviceSummary");
        suffixes.insert(1, "DeviceInfo.SpecVersion");
    }
    suffixes.into_iter().map(|s| format!("{root}.{s}")).collect()
}
