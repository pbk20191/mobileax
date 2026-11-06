//! WDA's XML page source, read into the shape of its JSON source.
//!
//! WDA sends JSON pretty-printed: 281 KB for a Settings page where the XML
//! is 36 KB, and the XML comes back in half the time (0.10 s against 0.22 s
//! on a simulator; more over USB). So the source is read as XML, and this
//! turns it into `{type, name, label, value, rect, isEnabled, isFocused,
//! isVisible, children}` for [crate::wda::rows]. It reads what WDA writes: elements
//! with quoted attributes and the five entities plus character references;
//! no comments, CDATA or namespaces.

use serde_json::{json, Map, Value};

/// The tree in `xml`, or what is wrong with it.
pub fn tree(xml: &str) -> Result<Value, String> {
    let mut stack: Vec<Value> = Vec::new();
    let mut i = 0;
    while let Some(off) = xml[i..].find('<') {
        let at = i + off;
        let rest = &xml[at..];
        if rest.starts_with("<?") || rest.starts_with("<!") {
            i = at + rest.find('>').ok_or("unclosed declaration")? + 1;
            continue;
        }
        if rest.starts_with("</") {
            i = at + rest.find('>').ok_or("unclosed end tag")? + 1;
            let node = stack.pop().ok_or("end tag without a start")?;
            match stack.last_mut() {
                Some(parent) => push_child(parent, node),
                None => return Ok(node),
            }
            continue;
        }
        let (node, end, closed) = element(xml, at + 1)?;
        i = end;
        if !closed {
            stack.push(node);
        } else {
            match stack.last_mut() {
                Some(parent) => push_child(parent, node),
                None => return Ok(node),
            }
        }
    }
    Err("no root element".into())
}

fn push_child(parent: &mut Value, node: Value) {
    if let Some(kids) = parent["children"].as_array_mut() {
        kids.push(node);
    }
}

/// The element whose name starts at `i`: the node, where its tag ends, and
/// whether it closed itself (`/>`).
fn element(xml: &str, mut i: usize) -> Result<(Value, usize, bool), String> {
    let b = xml.as_bytes();
    let name_end = i + xml[i..]
        .find(|c: char| c.is_whitespace() || c == '>' || c == '/')
        .ok_or("unclosed tag")?;
    let tag = &xml[i..name_end];
    i = name_end;
    let mut attrs: Map<String, Value> = Map::new();
    loop {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        match b.get(i) {
            None => return Err("unclosed tag".into()),
            Some(b'>') => return Ok((node(tag, attrs), i + 1, false)),
            Some(b'/') if b.get(i + 1) == Some(&b'>') => {
                return Ok((node(tag, attrs), i + 2, true))
            }
            _ => {}
        }
        let eq = i + xml[i..].find('=').ok_or("attribute without a value")?;
        let key = xml[i..eq].trim().to_string();
        let quote = *b.get(eq + 1).ok_or("attribute without a value")?;
        if quote != b'"' && quote != b'\'' {
            return Err(format!("unquoted attribute {key}"));
        }
        let start = eq + 2;
        let len = xml[start..]
            .find(quote as char)
            .ok_or("unclosed attribute")?;
        attrs.insert(key, Value::String(unescape(&xml[start..start + len])));
        i = start + len + 1;
    }
}

/// An element as the JSON source has it.
fn node(tag: &str, a: Map<String, Value>) -> Value {
    let s = |k: &str| a.get(k).cloned().unwrap_or(Value::Null);
    let n = |k: &str| {
        a.get(k)
            .and_then(Value::as_str)
            .and_then(|v| v.parse::<f64>().ok())
            .map_or(Value::Null, Value::from)
    };
    let ty = a
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or(tag)
        .to_string();
    json!({
        "type": ty.strip_prefix("XCUIElementType").unwrap_or(&ty),
        "name": s("name"),
        "label": s("label"),
        "value": s("value"),
        "isEnabled": s("enabled"),
        "isFocused": s("focused"),
        "isVisible": s("visible"),
        "rect": {"x": n("x"), "y": n("y"), "width": n("width"), "height": n("height")},
        "children": [],
    })
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(semi) = rest.find(';') else { break };
        let ent = &rest[1..semi];
        let ch = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => ent
                .strip_prefix("#x")
                .or_else(|| ent.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[semi + 1..];
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_wdas_source_into_the_json_shape() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<XCUIElementTypeApplication type="XCUIElementTypeApplication" name="설정" label="설정" enabled="true" x="0" y="0" width="402" height="874" index="0">
  <XCUIElementTypeCell type="XCUIElementTypeCell" enabled="true" x="16" y="125" width="370" height="53">
    <XCUIElementTypeButton type="XCUIElementTypeButton" name="com.apple.settings.general" label="일반 &amp; &quot;기타&quot;&#10;설정" enabled="false" x="16" y="125" width="370" height="53"/>
  </XCUIElementTypeCell>
  <XCUIElementTypeSearchField type="XCUIElementTypeSearchField" label="검색" value="Wi" focused="true" x="20" y="800" width="300" height="40"/>
</XCUIElementTypeApplication>"#;
        let t = tree(xml).unwrap();
        assert_eq!(t["type"], "Application");
        assert_eq!(t["label"], "설정");
        assert_eq!(t["rect"]["width"], 402.0);
        let button = &t["children"][0]["children"][0];
        assert_eq!(button["label"], "일반 & \"기타\"\n설정");
        assert_eq!(button["isEnabled"], "false");
        assert_eq!(t["children"][1]["isFocused"], "true");
        assert_eq!(t["children"][1]["value"], "Wi");
        assert!(t["children"][0]["label"].is_null());
    }

    #[test]
    fn rejects_what_is_not_a_tree() {
        assert!(tree("").is_err());
        assert!(tree("<a x=\"1\"").is_err());
        assert!(tree("</a>").is_err());
    }
}
