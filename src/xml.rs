//! Minimal XML tree parser, sufficient for CWMP SOAP envelopes.
//! Namespace prefixes are stripped from element and attribute names.

#[derive(Debug, Default, Clone)]
pub struct Node {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Node>,
    pub text: String,
}

impl Node {
    pub fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name == name)
    }

    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }

    pub fn text(&self) -> &str {
        self.text.trim()
    }

    pub fn child_text(&self, name: &str) -> &str {
        self.child(name).map(Node::text).unwrap_or("")
    }
}

pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find('&') {
        out.push_str(&rest[..p]);
        rest = &rest[p..];
        let Some(end) = rest.find(';') else { break };
        let ent = &rest[1..end];
        let decoded = match ent {
            "lt" => Some('<'),
            "gt" => Some('>'),
            "amp" => Some('&'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => ent
                .strip_prefix("#x")
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn find_from(input: &str, from: usize, pat: &str) -> Result<usize, String> {
    input[from..]
        .find(pat)
        .map(|p| from + p)
        .ok_or_else(|| format!("unterminated construct, expected `{pat}`"))
}

/// Finds the `>` closing the tag that starts at `from`, ignoring quoted attribute values.
fn tag_end(input: &str, from: usize) -> Result<usize, String> {
    let mut quote: Option<u8> = None;
    for (i, &b) in input.as_bytes()[from..].iter().enumerate() {
        match (quote, b) {
            (Some(q), _) if b == q => quote = None,
            (Some(_), _) => {}
            (None, b'"' | b'\'') => quote = Some(b),
            (None, b'>') => return Ok(from + i),
            _ => {}
        }
    }
    Err("unterminated tag".into())
}

fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

fn parse_tag(inner: &str) -> Result<Node, String> {
    let inner = inner.trim_start();
    let name_end = inner
        .find(|c: char| c.is_whitespace())
        .unwrap_or(inner.len());
    let mut node = Node {
        name: local(&inner[..name_end]).to_string(),
        ..Node::default()
    };
    let mut rest = inner[name_end..].trim_start();
    while !rest.is_empty() {
        let eq = rest.find('=').ok_or("attribute without value")?;
        let key = rest[..eq].trim();
        let after = rest[eq + 1..].trim_start();
        let quote = after.chars().next().ok_or("missing attribute value")?;
        if quote != '"' && quote != '\'' {
            return Err("unquoted attribute value".into());
        }
        let close = after[1..].find(quote).ok_or("unterminated attribute value")?;
        let value = unescape(&after[1..1 + close]);
        if key != "xmlns" && !key.starts_with("xmlns:") {
            node.attrs.push((local(key).to_string(), value));
        }
        rest = after[close + 2..].trim_start();
    }
    Ok(node)
}

fn attach(stack: &mut [Node], root: &mut Option<Node>, node: Node) -> Result<(), String> {
    match stack.last_mut() {
        Some(parent) => parent.children.push(node),
        None if root.is_none() => *root = Some(node),
        None => return Err("multiple root elements".into()),
    }
    Ok(())
}

pub fn parse(input: &str) -> Result<Node, String> {
    let mut stack: Vec<Node> = Vec::new();
    let mut root = None;
    let mut i = 0;
    while i < input.len() {
        if input.as_bytes()[i] == b'<' {
            let at = &input[i..];
            if at.starts_with("<?") {
                i = find_from(input, i, "?>")? + 2;
            } else if at.starts_with("<!--") {
                i = find_from(input, i, "-->")? + 3;
            } else if at.starts_with("<![CDATA[") {
                let end = find_from(input, i, "]]>")?;
                let parent = stack.last_mut().ok_or("text outside root")?;
                parent.text.push_str(&input[i + 9..end]);
                i = end + 3;
            } else if at.starts_with("<!") {
                i = find_from(input, i, ">")? + 1;
            } else if at.starts_with("</") {
                let end = find_from(input, i, ">")?;
                let node = stack.pop().ok_or("unbalanced closing tag")?;
                if node.name != local(input[i + 2..end].trim()) {
                    return Err(format!("mismatched closing tag for <{}>", node.name));
                }
                attach(&mut stack, &mut root, node)?;
                i = end + 1;
            } else {
                let end = tag_end(input, i + 1)?;
                let inner = &input[i + 1..end];
                let (inner, self_closing) = match inner.strip_suffix('/') {
                    Some(s) => (s, true),
                    None => (inner, false),
                };
                let node = parse_tag(inner)?;
                if self_closing {
                    attach(&mut stack, &mut root, node)?;
                } else {
                    stack.push(node);
                }
                i = end + 1;
            }
        } else {
            let end = input[i..].find('<').map_or(input.len(), |p| i + p);
            if let Some(parent) = stack.last_mut() {
                parent.text.push_str(&unescape(&input[i..end]));
            }
            i = end;
        }
    }
    if !stack.is_empty() {
        return Err("unclosed element".into());
    }
    root.ok_or_else(|| "empty document".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_soap_subset() {
        let doc = r#"<?xml version="1.0"?>
<!-- c --><soap:Envelope xmlns:soap="x"><soap:Header><cwmp:ID soap:mustUnderstand="1">a&amp;b</cwmp:ID></soap:Header>
<soap:Body><cwmp:Set><V xsi:type="xsd:string"><![CDATA[<x>]]></V><E/></cwmp:Set></soap:Body></soap:Envelope>"#;
        let n = parse(doc).unwrap();
        assert_eq!(n.name, "Envelope");
        assert_eq!(n.child("Header").unwrap().child_text("ID"), "a&b");
        let set = n.child("Body").unwrap().child("Set").unwrap();
        assert_eq!(set.child("V").unwrap().text(), "<x>");
        assert_eq!(set.child("V").unwrap().attrs[0], ("type".to_string(), "xsd:string".to_string()));
        assert!(set.child("E").is_some());
    }

    #[test]
    fn rejects_mismatch() {
        assert!(parse("<a><b></a>").is_err());
    }

    #[test]
    fn escape_roundtrip() {
        let s = "a<b>&\"'c";
        assert_eq!(unescape(&escape(s)), s);
    }
}
