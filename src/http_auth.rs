//! reqwest wrapper that answers Basic/Digest challenges and remembers the scheme.

use std::time::Duration;

use anyhow::{Context, Result};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, WWW_AUTHENTICATE};
use reqwest::{Client, Method, Response, StatusCode, Url};

use crate::digest::{self, Challenge};
use crate::util::base64_encode;

enum Scheme {
    Basic,
    Digest(Challenge),
}

pub struct AuthClient {
    client: Client,
    creds: Option<(String, String)>,
    scheme: Option<Scheme>,
    nc: u32,
}

impl AuthClient {
    /// `session` clients keep cookies and have an overall timeout (CWMP sessions);
    /// the others only bound connection setup (file downloads).
    pub fn new(user: &str, pass: &str, session: bool) -> Result<Self> {
        let mut b = Client::builder()
            .user_agent(concat!("stubtr069/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(30));
        if session {
            b = b.cookie_store(true).timeout(Duration::from_secs(60));
        }
        Ok(Self {
            client: b.build().context("building HTTP client")?,
            creds: (!user.is_empty()).then(|| (user.to_string(), pass.to_string())),
            scheme: None,
            nc: 0,
        })
    }

    fn authorization(&mut self, method: &Method, url: &Url) -> Option<String> {
        let (user, pass) = self.creds.as_ref()?;
        match self.scheme.as_ref()? {
            Scheme::Basic => {
                let raw = format!("{user}:{pass}");
                Some(format!("Basic {}", base64_encode(raw.as_bytes())))
            }
            Scheme::Digest(ch) => {
                self.nc += 1;
                let mut uri = url.path().to_string();
                if let Some(q) = url.query() {
                    uri.push('?');
                    uri.push_str(q);
                }
                Some(digest::client_authorization(
                    ch,
                    user,
                    pass,
                    method.as_str(),
                    &uri,
                    self.nc,
                ))
            }
        }
    }

    fn learn(&mut self, resp: &Response) -> bool {
        let headers: Vec<&str> = resp
            .headers()
            .get_all(WWW_AUTHENTICATE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect();
        if let Some(ch) = headers.iter().find_map(|h| digest::parse_challenge(h)) {
            self.scheme = Some(Scheme::Digest(ch));
            self.nc = 0;
            true
        } else if headers
            .iter()
            .any(|h| h.trim_start().to_ascii_lowercase().starts_with("basic"))
        {
            self.scheme = Some(Scheme::Basic);
            true
        } else {
            false
        }
    }

    /// Sends a request; an empty `body` with POST produces a zero-length POST.
    pub async fn send(&mut self, method: Method, url: &str, body: Option<String>) -> Result<Response> {
        let parsed = Url::parse(url).with_context(|| format!("invalid URL {url}"))?;
        for attempt in 0..3 {
            let mut req = self.client.request(method.clone(), parsed.clone());
            if method == Method::POST {
                req = req.header("SOAPAction", "");
                match &body {
                    Some(b) => {
                        req = req
                            .header(CONTENT_TYPE, "text/xml; charset=utf-8")
                            .body(b.clone())
                    }
                    None => req = req.body(String::new()),
                }
            }
            if let Some(h) = self.authorization(&method, &parsed) {
                req = req.header(AUTHORIZATION, h);
            }
            let resp = req.send().await?;
            if resp.status() == StatusCode::UNAUTHORIZED
                && attempt < 2
                && self.creds.is_some()
                && self.learn(&resp)
            {
                continue;
            }
            return Ok(resp);
        }
        unreachable!("loop returns on the last attempt")
    }
}
