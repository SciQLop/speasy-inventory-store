//! speasy's inventory JSON (indexes.to_dict, version 2) in and out of the store.
use crate::store::{Meta, NodeFields, Store, NONE};
use serde::ser::{Serialize, SerializeMap, Serializer};
use serde_json::{Map, Value};

pub const SPZ_KEYS: [&str; 4] = ["__spz_type__", "__spz_name__", "__spz_provider__", "__spz_uid__"];

fn as_node(v: &Value) -> Option<&Map<String, Value>> {
    v.as_object().filter(|o| o.contains_key("__spz_type__"))
}

pub fn from_json(text: &str) -> serde_json::Result<Store> {
    let root: Value = serde_json::from_str(text)?;
    let mut store = Store::default();
    if let Some(root) = as_node(&root) {
        load(&mut store, NONE, "", root);
    }
    store.finish();
    Ok(store)
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
        match (as_node(v), v) {
            (Some(child), _) => children.push((k, child)),
            (None, Value::String(s)) => store.push_str(k, s),
            (None, other) => store.push_other(k, other.clone()),
        }
    }
    for (k, child) in children {
        load(store, id, k, child);
    }
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
                Meta::Other(o) => map.serialize_entry(k, o)?,
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
