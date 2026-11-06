//! A small NSKeyedArchiver encoder for DTX method arguments.
//!
//! ns-keyed-archive only writes `NSDictionary` and has no `NSNull`; the
//! accessibility inspector is driven with `NSMutableDictionary` wrappers and
//! `NSNull` arguments (that is what Xcode and go-ios send), so this writes
//! exactly those. Decoding goes through ns-keyed-archive.

use plist::{Dictionary, Uid, Value};

/// One archived value.
#[derive(Clone, Debug, PartialEq)]
pub enum Arch {
    Null,
    Bool(bool),
    Int(i64),
    Str(String),
    Data(Vec<u8>),
    /// Key order is kept; `mutable` picks NSMutableDictionary over NSDictionary.
    Dict {
        entries: Vec<(String, Arch)>,
        mutable: bool,
    },
}

impl Arch {
    pub fn str(s: impl Into<String>) -> Self {
        Arch::Str(s.into())
    }

    /// An NSMutableDictionary.
    pub fn mdict<const N: usize>(entries: [(&str, Arch); N]) -> Self {
        Arch::Dict {
            entries: entries
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            mutable: true,
        }
    }

    /// An NSDictionary.
    pub fn dict<const N: usize>(entries: [(&str, Arch); N]) -> Self {
        Arch::Dict {
            entries: entries
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            mutable: false,
        }
    }

    /// `{ObjectType: passthrough, Value: v}`, the AX protocol's wrapper for a plain value.
    pub fn passthrough(v: Arch) -> Self {
        Arch::typed("passthrough", v)
    }

    /// `{ObjectType: ty, Value: v}`.
    pub fn typed(ty: &str, v: Arch) -> Self {
        Arch::mdict([("ObjectType", Arch::str(ty)), ("Value", v)])
    }
}

/// Archives `v` as a binary plist, as NSKeyedArchiver would.
pub fn archive(v: &Arch) -> Vec<u8> {
    let mut objects = vec![Value::String("$null".into())];
    let mut classes = Vec::new();
    let root = encode(v, &mut objects, &mut classes);

    let mut top = Dictionary::new();
    top.insert("root".into(), Value::Uid(root));
    let mut doc = Dictionary::new();
    doc.insert("$version".into(), Value::Integer(100_000.into()));
    doc.insert("$archiver".into(), Value::String("NSKeyedArchiver".into()));
    doc.insert("$top".into(), Value::Dictionary(top));
    doc.insert("$objects".into(), Value::Array(objects));

    let mut out = Vec::new();
    plist::to_writer_binary(&mut out, &Value::Dictionary(doc)).expect("writing to a Vec");
    out
}

fn push(objects: &mut Vec<Value>, v: Value) -> Uid {
    objects.push(v);
    Uid::new(objects.len() as u64 - 1)
}

fn encode(v: &Arch, objects: &mut Vec<Value>, classes: &mut Vec<(&'static str, Uid)>) -> Uid {
    match v {
        Arch::Null => Uid::new(0),
        Arch::Bool(b) => push(objects, Value::Boolean(*b)),
        Arch::Int(i) => push(objects, Value::Integer((*i).into())),
        Arch::Str(s) => push(objects, Value::String(s.clone())),
        Arch::Data(d) => push(objects, Value::Data(d.clone())),
        Arch::Dict { entries, mutable } => {
            // Reserve the dictionary's slot first, so it precedes its contents
            // as NSKeyedArchiver lays them out.
            let at = push(objects, Value::String(String::new()));
            let keys: Vec<Value> = entries
                .iter()
                .map(|(k, _)| Value::Uid(push(objects, Value::String(k.clone()))))
                .collect();
            let vals: Vec<Value> = entries
                .iter()
                .map(|(_, v)| Value::Uid(encode(v, objects, classes)))
                .collect();
            let name = if *mutable {
                "NSMutableDictionary"
            } else {
                "NSDictionary"
            };
            let class = class_uid(name, objects, classes);
            let mut d = Dictionary::new();
            d.insert("NS.keys".into(), Value::Array(keys));
            d.insert("NS.objects".into(), Value::Array(vals));
            d.insert("$class".into(), Value::Uid(class));
            objects[at.get() as usize] = Value::Dictionary(d);
            at
        }
    }
}

fn class_uid(
    name: &'static str,
    objects: &mut Vec<Value>,
    classes: &mut Vec<(&'static str, Uid)>,
) -> Uid {
    if let Some((_, uid)) = classes.iter().find(|(n, _)| *n == name) {
        return *uid;
    }
    let chain: &[&str] = match name {
        "NSMutableDictionary" => &["NSMutableDictionary", "NSDictionary", "NSObject"],
        _ => &["NSDictionary", "NSObject"],
    };
    let mut d = Dictionary::new();
    d.insert("$classname".into(), Value::String(name.into()));
    d.insert(
        "$classes".into(),
        Value::Array(chain.iter().map(|c| Value::String((*c).into())).collect()),
    );
    let uid = push(objects, Value::Dictionary(d));
    classes.push((name, uid));
    uid
}

/// Unarchives an NSKeyedArchiver blob into a plain plist value.
pub fn unarchive(bytes: &[u8]) -> Option<Value> {
    ns_keyed_archive::decode::from_bytes(bytes).ok()
}

/// Follows `{ObjectType, Value}` wrappers down `path` (keys of plain dictionaries
/// along the way) and returns what is there.
pub fn dig<'a>(v: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut cur = v;
    for key in path {
        cur = cur.as_dictionary()?.get(key)?;
    }
    Some(cur)
}

/// Strips any number of `{ObjectType, Value}` wrappers.
pub fn unwrap(v: &Value) -> &Value {
    let mut cur = v;
    while let Some(inner) = cur
        .as_dictionary()
        .filter(|d| d.contains_key("ObjectType"))
        .and_then(|d| d.get("Value"))
    {
        cur = inner;
    }
    cur
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_the_decoder() {
        let v = Arch::typed(
            "AXAuditElement_v1",
            Arch::passthrough(Arch::mdict([(
                "PlatformElementValue_v1",
                Arch::passthrough(Arch::Data(vec![0xfc, 0x0f, 0, 0])),
            )])),
        );
        let back = unarchive(&archive(&v)).expect("decodes");
        let leaf = dig(&back, &["Value", "Value", "PlatformElementValue_v1"]).map(unwrap);
        assert_eq!(leaf, Some(&Value::Data(vec![0xfc, 0x0f, 0, 0])));
        assert_eq!(
            dig(&back, &["ObjectType"]),
            Some(&Value::String("AXAuditElement_v1".into()))
        );
    }

    #[test]
    fn writes_mutable_dictionaries_and_null() {
        let bytes = archive(&Arch::mdict([("a", Arch::Int(4)), ("b", Arch::Bool(true))]));
        let raw: Value = plist::from_bytes(&bytes).unwrap();
        let objects = raw.as_dictionary().unwrap()["$objects"].as_array().unwrap();
        assert_eq!(objects[0], Value::String("$null".into()));
        let class = objects
            .iter()
            .filter_map(|o| o.as_dictionary())
            .find(|d| d.contains_key("$classname"))
            .unwrap();
        assert_eq!(
            class["$classname"],
            Value::String("NSMutableDictionary".into())
        );

        let null: Value = plist::from_bytes(&archive(&Arch::Null)).unwrap();
        assert_eq!(
            null.as_dictionary().unwrap()["$top"]
                .as_dictionary()
                .unwrap()["root"],
            Value::Uid(Uid::new(0))
        );
    }

    #[test]
    fn one_class_entry_per_class() {
        let bytes = archive(&Arch::mdict([("x", Arch::mdict([("y", Arch::Int(1))]))]));
        let raw: Value = plist::from_bytes(&bytes).unwrap();
        let objects = raw.as_dictionary().unwrap()["$objects"].as_array().unwrap();
        let n = objects
            .iter()
            .filter_map(|o| o.as_dictionary())
            .filter(|d| d.contains_key("$classname"))
            .count();
        assert_eq!(n, 1);
    }
}
