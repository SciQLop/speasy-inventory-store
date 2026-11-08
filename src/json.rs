//! speasy's inventory JSON (indexes.to_dict, version 2) in and out of the store.
use crate::store::{Meta, NodeFields, Store, Val, NONE};
use serde::ser::{Serialize, SerializeMap, SerializeSeq, Serializer};
use serde_json::{Map, Value};
use std::borrow::Cow;

// Python's json writes non-finite floats as bare NaN / Infinity / -Infinity, which strict JSON (serde_json) refuses.
// They cross serde_json as marker strings. simplify: a pre-pass copies the text when a token may be present; a
// tokenizer accepting them would avoid the copy.
const NONFINITE: [(&str, &str, f64); 3] = [
    ("-Infinity", "\\u0000spz:-Infinity", f64::NEG_INFINITY),
    ("Infinity", "\\u0000spz:Infinity", f64::INFINITY),
    ("NaN", "\\u0000spz:NaN", f64::NAN),
];

fn quote_nonfinite(text: &str) -> Cow<'_, str> {
    if !text.contains("NaN") && !text.contains("Infinity") {
        return Cow::Borrowed(text);
    }
    let bytes = text.as_bytes();
    let (mut out, mut copied, mut i, mut in_str) = (String::with_capacity(text.len()), 0, 0, false);
    while i < bytes.len() {
        match (in_str, bytes[i]) {
            (true, b'\\') => i += 1,
            (_, b'"') => in_str = !in_str,
            (false, _) => {
                if let Some((token, marker, _)) = NONFINITE.iter().find(|(t, _, _)| text[i..].starts_with(t)) {
                    out.push_str(&text[copied..i]);
                    out.extend(['"']);
                    out.push_str(marker);
                    out.extend(['"']);
                    i += token.len();
                    copied = i;
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
    out.push_str(&text[copied..]);
    Cow::Owned(out)
}

fn nonfinite(s: &str) -> Option<f64> {
    let token = s.strip_prefix("\0spz:")?;
    NONFINITE.iter().find(|(t, _, _)| *t == token).map(|(_, _, f)| *f)
}

/// Turns the markers written for non-finite floats back into Python's bare tokens.
pub fn unquote_nonfinite(json: String) -> String {
    if !json.contains("\\u0000spz:") {
        return json;
    }
    NONFINITE.iter().fold(json, |acc, (token, marker, _)| acc.replace(&format!("\"{marker}\""), token))
}

pub const SPZ_KEYS: [&str; 4] = ["__spz_type__", "__spz_name__", "__spz_provider__", "__spz_uid__"];

fn as_node(v: &Value) -> Option<&Map<String, Value>> {
    v.as_object().filter(|o| o.contains_key("__spz_type__"))
}

pub fn from_json(text: &str) -> serde_json::Result<Store> {
    let root: Value = serde_json::from_str(&quote_nonfinite(text))?;
    let mut store = Store::default();
    if let Some(root) = as_node(&root) {
        load(&mut store, NONE, "", root);
    }
    store.finish();
    Ok(store)
}

fn to_val(store: &mut Store, v: &Value) -> Val {
    match v {
        Value::String(s) => nonfinite(s).map_or_else(|| Val::Str(store.s.intern(s)), Val::Float),
        Value::Number(n) => n.as_i64().map_or_else(|| Val::Float(n.as_f64().unwrap_or(f64::NAN)), Val::Int),
        Value::Bool(b) => Val::Bool(*b),
        Value::Null => Val::Null,
        Value::Array(a) => Val::List(a.iter().map(|x| to_val(store, x)).collect()),
        Value::Object(o) => Val::Dict(o.iter().map(|(k, x)| (store.s.intern(k), to_val(store, x))).collect()),
    }
}

fn load(store: &mut Store, parent: u32, key: &str, node: &Map<String, Value>) {
    let field = |k: &str| node.get(k).and_then(Value::as_str).unwrap_or("");
    let fields = NodeFields {
        key,
        name: field("__spz_name__"),
        kind: field("__spz_type__"),
        provider: field("__spz_provider__"),
        uid: field("__spz_uid__"),
    };
    let id = store.start_node(parent, fields);
    let mut children = Vec::new();
    for (k, v) in node.iter().filter(|(k, _)| !SPZ_KEYS.contains(&k.as_str())) {
        match as_node(v) {
            Some(child) => children.push((k, child)),
            None => {
                let value = to_val(store, v);
                store.push_meta(k, value);
            }
        }
    }
    for (k, child) in children {
        load(store, id, k, child);
    }
}

/// A value, serialized the way speasy's to_json (version 2) writes it: tuples as arrays, opaque values as str().
struct ValRef<'a> {
    store: &'a Store,
    val: &'a Val,
}

impl Serialize for ValRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let st = self.store;
        match self.val {
            Val::Str(id) => serializer.serialize_str(st.s.str(*id)),
            Val::Int(i) => serializer.serialize_i64(*i),
            Val::Float(f) if f.is_finite() => serializer.serialize_f64(*f),
            Val::Float(f) => {
                let token = if f.is_nan() { "NaN" } else if *f > 0.0 { "Infinity" } else { "-Infinity" };
                serializer.serialize_str(&format!("\0spz:{token}"))
            }
            Val::Bool(b) => serializer.serialize_bool(*b),
            Val::Null => serializer.serialize_unit(),
            Val::Opaque(i) => serializer.serialize_str(st.opaque_text(*i)),
            Val::List(items) | Val::Tuple(items) => {
                let mut seq = serializer.serialize_seq(Some(items.len()))?;
                for v in items {
                    seq.serialize_element(&ValRef { store: st, val: v })?;
                }
                seq.end()
            }
            Val::Dict(entries) => {
                let mut map = serializer.serialize_map(Some(entries.len()))?;
                for (k, v) in entries {
                    map.serialize_entry(st.s.str(*k), &ValRef { store: st, val: v })?;
                }
                map.end()
            }
        }
    }
}

/// One value as JSON text, the way speasy's to_json writes it.
pub fn value_json(store: &Store, val: &Val) -> String {
    serde_json::to_string(&ValRef { store, val }).map(unquote_nonfinite).unwrap_or_default()
}

pub struct NodeRef<'a> {
    pub store: &'a Store,
    pub id: u32,
}

impl Serialize for NodeRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let (st, id) = (self.store, self.id);
        let mut map = serializer.serialize_map(None)?;
        for (k, v) in st.meta(id) {
            match v {
                Meta::Str(s) => map.serialize_entry(k, s)?,
                Meta::Other(val) => map.serialize_entry(k, &ValRef { store: st, val })?,
            }
        }
        for &c in st.children(id) {
            map.serialize_entry(st.key(c), &NodeRef { store: st, id: c })?;
        }
        for (k, v) in SPZ_KEYS.iter().zip([st.kind(id), st.name(id), st.provider(id), st.uid(id)]) {
            map.serialize_entry(k, v)?;
        }
        map.end()
    }
}
