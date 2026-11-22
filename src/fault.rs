use std::fmt;

/// A CWMP fault (TR-069 Table 24), optionally carrying per-parameter details.
#[derive(Debug, Clone)]
pub struct Fault {
    pub code: u32,
    pub text: String,
    pub details: Vec<(String, u32, String)>,
}

impl Fault {
    pub fn new(code: u32, text: impl Into<String>) -> Self {
        Self {
            code,
            text: text.into(),
            details: Vec::new(),
        }
    }

    pub fn invalid_name(name: &str) -> Self {
        Self::new(9005, format!("Invalid parameter name: {name}"))
    }

    pub fn internal(e: impl fmt::Display) -> Self {
        Self::new(9002, format!("Internal error: {e}"))
    }
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.code, self.text)
    }
}

impl std::error::Error for Fault {}
