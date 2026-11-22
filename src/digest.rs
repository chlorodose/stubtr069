//! HTTP Digest (RFC 7616, qop=auth, MD5 / SHA-256) for both the client and the
//! Connection Request server side.

use std::collections::VecDeque;
use std::sync::Mutex;

use md5::{Digest, Md5};
use sha2::Sha256;

use crate::util::random_hex;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Alg {
    Md5,
    Sha256,
}

impl Alg {
    fn name(self) -> &'static str {
        match self {
            Alg::Md5 => "MD5",
            Alg::Sha256 => "SHA-256",
        }
    }

    fn hash(self, s: &str) -> String {
        match self {
            Alg::Md5 => hex::encode(Md5::digest(s.as_bytes())),
            Alg::Sha256 => hex::encode(Sha256::digest(s.as_bytes())),
        }
    }
}

/// Splits `k=v, k2="v2"` pairs, honoring quoted values that may contain commas.
pub fn parse_params(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = s.trim();
    while !rest.is_empty() {
        let Some(eq) = rest.find('=') else { break };
        let key = rest[..eq].trim().trim_start_matches(',').trim().to_ascii_lowercase();
        let after = rest[eq + 1..].trim_start();
        let (value, tail) = if let Some(q) = after.strip_prefix('"') {
            let mut value = String::new();
            let mut chars = q.char_indices();
            let mut end = q.len();
            while let Some((i, c)) = chars.next() {
                match c {
                    '\\' => {
                        if let Some((_, n)) = chars.next() {
                            value.push(n);
                        }
                    }
                    '"' => {
                        end = i + 1;
                        break;
                    }
                    _ => value.push(c),
                }
            }
            (value, &q[end..])
        } else {
            let end = after.find(',').unwrap_or(after.len());
            (after[..end].trim().to_string(), &after[end..])
        };
        out.push((key, value));
        rest = tail.trim_start().trim_start_matches(',').trim_start();
    }
    out
}

fn get<'a>(params: &'a [(String, String)], key: &str) -> Option<&'a str> {
    params
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

fn parse_alg(v: Option<&str>) -> Option<Alg> {
    match v.map(str::to_ascii_uppercase).as_deref() {
        None | Some("MD5") => Some(Alg::Md5),
        Some("SHA-256") => Some(Alg::Sha256),
        _ => None,
    }
}

#[derive(Debug, Clone)]
pub struct Challenge {
    pub realm: String,
    pub nonce: String,
    pub opaque: Option<String>,
    pub qop_auth: bool,
    pub alg: Alg,
}

/// Parses a `WWW-Authenticate: Digest ...` header value.
pub fn parse_challenge(header: &str) -> Option<Challenge> {
    let body = header.trim().strip_prefix_ci("Digest")?;
    let p = parse_params(body);
    Some(Challenge {
        realm: get(&p, "realm")?.to_string(),
        nonce: get(&p, "nonce")?.to_string(),
        opaque: get(&p, "opaque").map(str::to_string),
        qop_auth: get(&p, "qop").is_some_and(|q| q.split(',').any(|v| v.trim() == "auth")),
        alg: parse_alg(get(&p, "algorithm"))?,
    })
}

trait StripPrefixCi {
    fn strip_prefix_ci(&self, prefix: &str) -> Option<&str>;
}

impl StripPrefixCi for str {
    fn strip_prefix_ci(&self, prefix: &str) -> Option<&str> {
        let head = self.get(..prefix.len())?;
        head.eq_ignore_ascii_case(prefix)
            .then(|| &self[prefix.len()..])
    }
}

#[allow(clippy::too_many_arguments)]
fn response_hash(
    alg: Alg,
    user: &str,
    realm: &str,
    pass: &str,
    method: &str,
    uri: &str,
    nonce: &str,
    qop: Option<(&str, &str)>, // (nc, cnonce) when qop=auth
) -> String {
    let ha1 = alg.hash(&format!("{user}:{realm}:{pass}"));
    let ha2 = alg.hash(&format!("{method}:{uri}"));
    match qop {
        Some((nc, cnonce)) => alg.hash(&format!("{ha1}:{nonce}:{nc}:{cnonce}:auth:{ha2}")),
        None => alg.hash(&format!("{ha1}:{nonce}:{ha2}")),
    }
}

pub fn client_authorization(
    ch: &Challenge,
    user: &str,
    pass: &str,
    method: &str,
    uri: &str,
    nc: u32,
) -> String {
    let nc_s = format!("{nc:08x}");
    let cnonce = random_hex(8);
    let qop = ch.qop_auth.then_some((nc_s.as_str(), cnonce.as_str()));
    let response = response_hash(ch.alg, user, &ch.realm, pass, method, uri, &ch.nonce, qop);
    let mut h = format!(
        "Digest username=\"{user}\", realm=\"{}\", nonce=\"{}\", uri=\"{uri}\", algorithm={}, response=\"{response}\"",
        ch.realm,
        ch.nonce,
        ch.alg.name()
    );
    if let Some(o) = &ch.opaque {
        h.push_str(&format!(", opaque=\"{o}\""));
    }
    if ch.qop_auth {
        h.push_str(&format!(", qop=auth, nc={nc_s}, cnonce=\"{cnonce}\""));
    }
    h
}

/// Server side verifier with a bounded set of issued nonces.
pub struct Verifier {
    realm: String,
    nonces: Mutex<VecDeque<(String, u32)>>, // (nonce, last seen nc)
}

impl Verifier {
    pub fn new(realm: &str) -> Self {
        Self {
            realm: realm.to_string(),
            nonces: Mutex::new(VecDeque::new()),
        }
    }

    /// Issues a fresh `WWW-Authenticate` header value.
    pub fn challenge(&self) -> String {
        let nonce = random_hex(16);
        let mut nonces = self.nonces.lock().unwrap();
        nonces.push_back((nonce.clone(), 0));
        while nonces.len() > 256 {
            nonces.pop_front();
        }
        format!(
            "Digest realm=\"{}\", qop=\"auth\", nonce=\"{nonce}\", opaque=\"{}\", algorithm=MD5",
            self.realm,
            random_hex(8)
        )
    }

    pub fn verify(&self, header: &str, method: &str, user: &str, pass: &str) -> bool {
        let Some(body) = header.trim().strip_prefix_ci("Digest") else {
            return false;
        };
        let p = parse_params(body);
        let (Some(username), Some(nonce), Some(uri), Some(response)) = (
            get(&p, "username"),
            get(&p, "nonce"),
            get(&p, "uri"),
            get(&p, "response"),
        ) else {
            return false;
        };
        let Some(alg) = parse_alg(get(&p, "algorithm")) else {
            return false;
        };
        if username != user || get(&p, "realm").is_some_and(|r| r != self.realm) {
            return false;
        }
        let qop = match get(&p, "qop") {
            Some("auth") => match (get(&p, "nc"), get(&p, "cnonce")) {
                (Some(nc), Some(cn)) => Some((nc, cn)),
                _ => return false,
            },
            Some(_) => return false,
            None => None,
        };
        let nc_val = qop.and_then(|(nc, _)| u32::from_str_radix(nc, 16).ok());
        {
            let mut nonces = self.nonces.lock().unwrap();
            let Some(entry) = nonces.iter_mut().find(|(n, _)| n == nonce) else {
                return false;
            };
            if let Some(nc) = nc_val {
                if nc <= entry.1 {
                    return false;
                }
                entry.1 = nc;
            }
        }
        response_hash(alg, user, &self.realm, pass, method, uri, nonce, qop) == response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc2617_example() {
        // RFC 2617 section 3.5 example (MD5, qop=auth).
        let r = response_hash(
            Alg::Md5,
            "Mufasa",
            "testrealm@host.com",
            "Circle Of Life",
            "GET",
            "/dir/index.html",
            "dcd98b7102dd2f0e8b11d0f600bfb0c093",
            Some(("00000001", "0a4f113b")),
        );
        assert_eq!(r, "6629fae49393a05397450978507c4ef1");
    }

    #[test]
    fn client_and_server_agree() {
        let v = Verifier::new("stub");
        let ch = parse_challenge(&v.challenge()).unwrap();
        let h = client_authorization(&ch, "u", "p", "GET", "/", 1);
        assert!(v.verify(&h, "GET", "u", "p"));
        assert!(!v.verify(&h, "GET", "u", "p"), "nc replay must fail");
        let h2 = client_authorization(&ch, "u", "bad", "GET", "/", 2);
        assert!(!v.verify(&h2, "GET", "u", "p"));
    }

    #[test]
    fn params_with_commas() {
        let p = parse_params(r#"realm="a, b", nonce=xyz, qop="auth,auth-int""#);
        assert_eq!(get(&p, "realm"), Some("a, b"));
        assert_eq!(get(&p, "nonce"), Some("xyz"));
        assert_eq!(get(&p, "qop"), Some("auth,auth-int"));
    }
}
