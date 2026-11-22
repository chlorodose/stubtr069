//! CWMP SOAP message parsing and construction.

use crate::db::Param;
use crate::fault::Fault;
use crate::xml::{self, Node, escape};

pub const NS: &str = "urn:dslforum-org:cwmp-1-2";

pub const SUPPORTED_METHODS: &[&str] = &[
    "GetRPCMethods",
    "SetParameterValues",
    "GetParameterValues",
    "GetParameterNames",
    "SetParameterAttributes",
    "GetParameterAttributes",
    "AddObject",
    "DeleteObject",
    "Reboot",
    "FactoryReset",
    "Download",
    "ScheduleInform",
];

#[derive(Debug)]
pub struct ParamSet {
    pub name: String,
    pub value: String,
    pub ty: Option<String>,
}

#[derive(Debug)]
pub struct AttrSet {
    pub name: String,
    pub notification_change: bool,
    pub notification: u8,
    pub access_list_change: bool,
    pub access_list: Vec<String>,
}

#[derive(Debug)]
pub struct DownloadReq {
    pub command_key: String,
    pub file_type: String,
    pub url: String,
    pub username: String,
    pub password: String,
    pub file_size: u64,
    pub target_file_name: String,
    pub delay_seconds: u64,
}

#[derive(Debug)]
pub enum Rpc {
    GetRpcMethods,
    SetParameterValues { values: Vec<ParamSet>, parameter_key: String },
    GetParameterValues { names: Vec<String> },
    GetParameterNames { path: String, next_level: bool },
    SetParameterAttributes { items: Vec<AttrSet> },
    GetParameterAttributes { names: Vec<String> },
    AddObject { path: String, parameter_key: String },
    DeleteObject { path: String, parameter_key: String },
    Reboot { command_key: String },
    FactoryReset,
    Download(DownloadReq),
    ScheduleInform { delay_seconds: u64, command_key: String },
    Unsupported { method: String },
}

pub struct Message {
    pub id: String,
    pub body: Node,
}

pub fn parse_message(text: &str) -> Result<Message, String> {
    let root = xml::parse(text)?;
    if root.name != "Envelope" {
        return Err(format!("unexpected root element <{}>", root.name));
    }
    let id = root
        .child("Header")
        .map(|h| h.child_text("ID").to_string())
        .unwrap_or_default();
    let body = root
        .child("Body")
        .and_then(|b| b.children.first())
        .cloned()
        .ok_or("empty SOAP body")?;
    Ok(Message { id, body })
}

fn strings(n: &Node, name: &str) -> Vec<String> {
    n.child(name)
        .map(|c| c.children.iter().map(|s| s.text().to_string()).collect())
        .unwrap_or_default()
}

fn flag(s: &str) -> bool {
    matches!(s.trim(), "1" | "true")
}

fn num(n: &Node, name: &str) -> u64 {
    n.child_text(name).parse::<i64>().unwrap_or(0).max(0) as u64
}

pub fn parse_rpc(n: &Node) -> Rpc {
    match n.name.as_str() {
        "GetRPCMethods" => Rpc::GetRpcMethods,
        "SetParameterValues" => Rpc::SetParameterValues {
            values: n
                .child("ParameterList")
                .map(|l| {
                    l.children_named("ParameterValueStruct")
                        .map(|s| {
                            let v = s.child("Value");
                            ParamSet {
                                name: s.child_text("Name").to_string(),
                                value: v.map(|v| v.text.clone()).unwrap_or_default(),
                                ty: v
                                    .and_then(|v| v.attrs.iter().find(|(k, _)| k == "type"))
                                    .map(|(_, t)| t.clone()),
                            }
                        })
                        .collect()
                })
                .unwrap_or_default(),
            parameter_key: n.child_text("ParameterKey").to_string(),
        },
        "GetParameterValues" => Rpc::GetParameterValues {
            names: strings(n, "ParameterNames"),
        },
        "GetParameterNames" => Rpc::GetParameterNames {
            path: n.child_text("ParameterPath").to_string(),
            next_level: flag(n.child_text("NextLevel")),
        },
        "SetParameterAttributes" => Rpc::SetParameterAttributes {
            items: n
                .child("ParameterList")
                .map(|l| {
                    l.children_named("SetParameterAttributesStruct")
                        .map(|s| AttrSet {
                            name: s.child_text("Name").to_string(),
                            notification_change: flag(s.child_text("NotificationChange")),
                            notification: s.child_text("Notification").parse().unwrap_or(0),
                            access_list_change: flag(s.child_text("AccessListChange")),
                            access_list: strings(s, "AccessList"),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        },
        "GetParameterAttributes" => Rpc::GetParameterAttributes {
            names: strings(n, "ParameterNames"),
        },
        "AddObject" => Rpc::AddObject {
            path: n.child_text("ObjectName").to_string(),
            parameter_key: n.child_text("ParameterKey").to_string(),
        },
        "DeleteObject" => Rpc::DeleteObject {
            path: n.child_text("ObjectName").to_string(),
            parameter_key: n.child_text("ParameterKey").to_string(),
        },
        "Reboot" => Rpc::Reboot {
            command_key: n.child_text("CommandKey").to_string(),
        },
        "FactoryReset" => Rpc::FactoryReset,
        "Download" => Rpc::Download(DownloadReq {
            command_key: n.child_text("CommandKey").to_string(),
            file_type: n.child_text("FileType").to_string(),
            url: n.child_text("URL").to_string(),
            username: n.child_text("Username").to_string(),
            password: n.child_text("Password").to_string(),
            file_size: num(n, "FileSize"),
            target_file_name: n.child_text("TargetFileName").to_string(),
            delay_seconds: num(n, "DelaySeconds"),
        }),
        "ScheduleInform" => Rpc::ScheduleInform {
            delay_seconds: num(n, "DelaySeconds"),
            command_key: n.child_text("CommandKey").to_string(),
        },
        other => Rpc::Unsupported {
            method: other.to_string(),
        },
    }
}

fn redact<'a>(name: &str, value: &'a str) -> &'a str {
    if name.to_ascii_lowercase().contains("password") {
        "***"
    } else {
        value
    }
}

impl Rpc {
    pub fn method(&self) -> &str {
        match self {
            Rpc::GetRpcMethods => "GetRPCMethods",
            Rpc::SetParameterValues { .. } => "SetParameterValues",
            Rpc::GetParameterValues { .. } => "GetParameterValues",
            Rpc::GetParameterNames { .. } => "GetParameterNames",
            Rpc::SetParameterAttributes { .. } => "SetParameterAttributes",
            Rpc::GetParameterAttributes { .. } => "GetParameterAttributes",
            Rpc::AddObject { .. } => "AddObject",
            Rpc::DeleteObject { .. } => "DeleteObject",
            Rpc::Reboot { .. } => "Reboot",
            Rpc::FactoryReset => "FactoryReset",
            Rpc::Download(_) => "Download",
            Rpc::ScheduleInform { .. } => "ScheduleInform",
            Rpc::Unsupported { method } => method,
        }
    }

    /// Human readable argument summary for the command log (secrets redacted).
    pub fn summary(&self) -> String {
        match self {
            Rpc::GetRpcMethods | Rpc::FactoryReset | Rpc::Unsupported { .. } => String::new(),
            Rpc::SetParameterValues { values, parameter_key } => {
                let v: Vec<String> = values
                    .iter()
                    .map(|p| format!("{}={:?}", p.name, redact(&p.name, &p.value)))
                    .collect();
                format!("ParameterKey={parameter_key:?} values=[{}]", v.join(", "))
            }
            Rpc::GetParameterValues { names } | Rpc::GetParameterAttributes { names } => {
                format!("names={names:?}")
            }
            Rpc::GetParameterNames { path, next_level } => {
                format!("path={path:?} next_level={next_level}")
            }
            Rpc::SetParameterAttributes { items } => format!("{items:?}"),
            Rpc::AddObject { path, parameter_key } | Rpc::DeleteObject { path, parameter_key } => {
                format!("object={path:?} ParameterKey={parameter_key:?}")
            }
            Rpc::Reboot { command_key } => format!("CommandKey={command_key:?}"),
            Rpc::Download(d) => format!(
                "CommandKey={:?} FileType={:?} URL={:?} Username={:?} Password={} FileSize={} \
                 TargetFileName={:?} DelaySeconds={}",
                d.command_key,
                d.file_type,
                d.url,
                d.username,
                if d.password.is_empty() { "\"\"" } else { "***" },
                d.file_size,
                d.target_file_name,
                d.delay_seconds
            ),
            Rpc::ScheduleInform { delay_seconds, command_key } => {
                format!("DelaySeconds={delay_seconds} CommandKey={command_key:?}")
            }
        }
    }
}

// ---- builders ----

pub fn envelope(id: &str, body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<soap-env:Envelope xmlns:soap-env="http://schemas.xmlsoap.org/soap/envelope/" xmlns:soap-enc="http://schemas.xmlsoap.org/soap/encoding/" xmlns:xsd="http://www.w3.org/2001/XMLSchema" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xmlns:cwmp="{NS}"><soap-env:Header><cwmp:ID soap-env:mustUnderstand="1">{}</cwmp:ID></soap-env:Header><soap-env:Body>{body}</soap-env:Body></soap-env:Envelope>"#,
        escape(id)
    )
}

pub fn fault_body(f: &Fault) -> String {
    let mut s = format!(
        "<soap-env:Fault><faultcode>Client</faultcode><faultstring>CWMP fault</faultstring><detail>\
         <cwmp:Fault><FaultCode>{}</FaultCode><FaultString>{}</FaultString>",
        f.code,
        escape(&f.text)
    );
    for (name, code, text) in &f.details {
        s.push_str(&format!(
            "<SetParameterValuesFault><ParameterName>{}</ParameterName><FaultCode>{code}</FaultCode>\
             <FaultString>{}</FaultString></SetParameterValuesFault>",
            escape(name),
            escape(text)
        ));
    }
    s.push_str("</cwmp:Fault></detail></soap-env:Fault>");
    s
}

pub fn parameter_list(params: &[Param]) -> String {
    let mut s = format!(
        "<ParameterList soap-enc:arrayType=\"cwmp:ParameterValueStruct[{}]\">",
        params.len()
    );
    for p in params {
        // Passwords are never disclosed (TR-069 A.2.2.1 requires an empty value).
        let value = if p.name.ends_with("Password") { "" } else { &p.value };
        s.push_str(&format!(
            "<ParameterValueStruct><Name>{}</Name><Value xsi:type=\"{}\">{}</Value></ParameterValueStruct>",
            escape(&p.name),
            escape(&p.ty),
            escape(value)
        ));
    }
    s.push_str("</ParameterList>");
    s
}

pub struct InformData<'a> {
    pub manufacturer: &'a str,
    pub oui: &'a str,
    pub product_class: &'a str,
    pub serial_number: &'a str,
    pub events: &'a [(String, String)],
    pub current_time: &'a str,
    pub retry_count: u32,
    pub params: &'a [Param],
}

pub fn inform_body(d: &InformData) -> String {
    let mut s = format!(
        "<cwmp:Inform><DeviceId><Manufacturer>{}</Manufacturer><OUI>{}</OUI><ProductClass>{}</ProductClass>\
         <SerialNumber>{}</SerialNumber></DeviceId><Event soap-enc:arrayType=\"cwmp:EventStruct[{}]\">",
        escape(d.manufacturer),
        escape(d.oui),
        escape(d.product_class),
        escape(d.serial_number),
        d.events.len()
    );
    for (code, key) in d.events {
        s.push_str(&format!(
            "<EventStruct><EventCode>{}</EventCode><CommandKey>{}</CommandKey></EventStruct>",
            escape(code),
            escape(key)
        ));
    }
    s.push_str(&format!(
        "</Event><MaxEnvelopes>1</MaxEnvelopes><CurrentTime>{}</CurrentTime><RetryCount>{}</RetryCount>{}</cwmp:Inform>",
        d.current_time,
        d.retry_count,
        parameter_list(d.params)
    ));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_set_parameter_values() {
        let m = parse_message(
            r#"<soapenv:Envelope xmlns:soapenv="s" xmlns:cwmp="c"><soapenv:Header><cwmp:ID>7</cwmp:ID></soapenv:Header>
<soapenv:Body><cwmp:SetParameterValues><ParameterList><ParameterValueStruct><Name>A.B</Name>
<Value xsi:type="xsd:boolean">1</Value></ParameterValueStruct></ParameterList><ParameterKey>pk</ParameterKey>
</cwmp:SetParameterValues></soapenv:Body></soapenv:Envelope>"#,
        )
        .unwrap();
        assert_eq!(m.id, "7");
        match parse_rpc(&m.body) {
            Rpc::SetParameterValues { values, parameter_key } => {
                assert_eq!(parameter_key, "pk");
                assert_eq!(values[0].name, "A.B");
                assert_eq!(values[0].value, "1");
                assert_eq!(values[0].ty.as_deref(), Some("xsd:boolean"));
            }
            r => panic!("{r:?}"),
        }
    }

    #[test]
    fn summary_redacts_secrets() {
        let rpc = Rpc::SetParameterValues {
            values: vec![ParamSet {
                name: "X.Password".into(),
                value: "hunter2".into(),
                ty: None,
            }],
            parameter_key: String::new(),
        };
        assert!(!rpc.summary().contains("hunter2"));
    }

    #[test]
    fn envelope_roundtrip() {
        let m = parse_message(&envelope("a<b", "<cwmp:GetRPCMethods/>")).unwrap();
        assert_eq!(m.id, "a<b");
        assert!(matches!(parse_rpc(&m.body), Rpc::GetRpcMethods));
    }
}
