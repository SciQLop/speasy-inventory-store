//! The inventory as columns: one row per node, metadata as (key, value) runs, children as CSR.
use crate::interner::Interner;
use hashbrown::HashMap;
use serde_json::Value;

pub const NONE: u32 = u32::MAX;
/// A metadata value with this bit set indexes `others` (non-string values, ~1% of them), else it's a string id.
pub const OTHER: u32 = 1 << 31;

pub enum Meta<'a> {
    Str(&'a str),
    Other(&'a Value),
}

#[derive(Default)]
pub struct Store {
    pub s: Interner,
    key: Vec<u32>, // the attribute the parent stores it under: a Python identifier, may differ from name
    name: Vec<u32>,
    kind: Vec<u32>,
    provider: Vec<u32>,
    uid: Vec<u32>,
    parent: Vec<u32>,
    meta_start: Vec<u32>,
    meta_key: Vec<u32>,
    meta_val: Vec<u32>,
    others: Vec<Value>,
    child_start: Vec<u32>,
    child: Vec<u32>,
    by_uid: HashMap<(u32, u32), u32>,
}

pub struct NodeFields<'a> {
    pub key: &'a str,
    pub name: &'a str,
    pub kind: &'a str,
    pub provider: &'a str,
    pub uid: &'a str,
}

impl Store {
    /// Starts a node; its metadata must be pushed before the next node starts.
    pub fn start_node(&mut self, parent: u32, f: NodeFields) -> u32 {
        let id = self.name.len() as u32;
        let interned = [f.key, f.name, f.kind, f.provider, f.uid].map(|s| self.s.intern(s));
        for (column, value) in [&mut self.key, &mut self.name, &mut self.kind, &mut self.provider, &mut self.uid]
            .into_iter()
            .zip(interned)
        {
            column.push(value);
        }
        self.parent.push(parent);
        self.meta_start.push(self.meta_key.len() as u32);
        id
    }

    pub fn push_str(&mut self, key: &str, value: &str) {
        let (k, v) = (self.s.intern(key), self.s.intern(value));
        self.meta_key.push(k);
        self.meta_val.push(v);
    }

    pub fn push_other(&mut self, key: &str, value: Value) {
        let k = self.s.intern(key);
        self.meta_key.push(k);
        self.meta_val.push(OTHER | self.others.len() as u32);
        self.others.push(value);
    }

    /// Builds the children index and the uid index, once every node is in.
    pub fn finish(&mut self) {
        let n = self.len();
        self.meta_start.push(self.meta_key.len() as u32);
        let mut start = vec![0u32; n + 1];
        for &p in self.parent.iter().filter(|&&p| p != NONE) {
            start[p as usize + 1] += 1;
        }
        for i in 0..n {
            start[i + 1] += start[i];
        }
        let mut next = start.clone();
        let mut child = vec![0u32; start[n] as usize];
        for (id, &p) in self.parent.iter().enumerate().filter(|(_, p)| **p != NONE) {
            child[next[p as usize] as usize] = id as u32;
            next[p as usize] += 1;
        }
        self.child_start = start;
        self.child = child;
        self.by_uid = (0..n as u32).rev().map(|i| ((self.provider[i as usize], self.uid[i as usize]), i)).collect();
    }

    pub fn len(&self) -> usize {
        self.name.len()
    }

    pub fn key(&self, i: u32) -> &str {
        self.s.str(self.key[i as usize])
    }
    pub fn name(&self, i: u32) -> &str {
        self.s.str(self.name[i as usize])
    }
    pub fn kind(&self, i: u32) -> &str {
        self.s.str(self.kind[i as usize])
    }
    pub fn provider(&self, i: u32) -> &str {
        self.s.str(self.provider[i as usize])
    }
    pub fn uid(&self, i: u32) -> &str {
        self.s.str(self.uid[i as usize])
    }
    pub fn parent(&self, i: u32) -> Option<u32> {
        Some(self.parent[i as usize]).filter(|&p| p != NONE)
    }

    pub fn children(&self, i: u32) -> &[u32] {
        let i = i as usize;
        &self.child[self.child_start[i] as usize..self.child_start[i + 1] as usize]
    }

    pub fn meta(&self, i: u32) -> impl Iterator<Item = (&str, Meta<'_>)> {
        let i = i as usize;
        let range = self.meta_start[i] as usize..self.meta_start[i + 1] as usize;
        range.map(|m| (self.s.str(self.meta_key[m]), self.value(self.meta_val[m])))
    }

    fn value(&self, v: u32) -> Meta<'_> {
        if v & OTHER != 0 { Meta::Other(&self.others[(v & !OTHER) as usize]) } else { Meta::Str(self.s.str(v)) }
    }

    /// (key id, value) pairs, for callers that cache one object per string id.
    pub fn raw_meta(&self, i: u32) -> impl Iterator<Item = (u32, u32)> + '_ {
        let i = i as usize;
        (self.meta_start[i] as usize..self.meta_start[i + 1] as usize).map(|m| (self.meta_key[m], self.meta_val[m]))
    }

    /// key, name, kind, provider, uid string ids.
    pub fn raw(&self, i: u32) -> [u32; 5] {
        let i = i as usize;
        [self.key[i], self.name[i], self.kind[i], self.provider[i], self.uid[i]]
    }

    pub fn other(&self, v: u32) -> &Value {
        &self.others[(v & !OTHER) as usize]
    }

    pub fn get(&self, i: u32, key: &str) -> Option<Meta<'_>> {
        let k = self.s.get(key)?;
        let i = i as usize;
        (self.meta_start[i] as usize..self.meta_start[i + 1] as usize)
            .find(|&m| self.meta_key[m] == k)
            .map(|m| self.value(self.meta_val[m]))
    }

    pub fn find_uid(&self, provider: &str, uid: &str) -> Option<u32> {
        self.by_uid.get(&(self.s.get(provider)?, self.s.get(uid)?)).copied()
    }

    /// Walks attribute keys from the root, like `tree.cda.MMS.MMS1`.
    pub fn find_path(&self, path: &str) -> Option<u32> {
        path.split('/').try_fold(0u32, |node, key| {
            let k = self.s.get(key)?;
            self.children(node).iter().copied().find(|&c| self.key[c as usize] == k)
        })
    }

    /// Nodes matching every given predicate; `contains` is a substring test on one metadata value.
    pub fn filter(&self, provider: Option<&str>, kind: Option<&str>, contains: Option<(&str, &str)>) -> Vec<u32> {
        let id = |s: Option<&str>| s.map(|s| self.s.get(s).unwrap_or(NONE));
        let (provider, kind) = (id(provider), id(kind));
        (0..self.len() as u32)
            .filter(|&i| provider.is_none_or(|p| self.provider[i as usize] == p))
            .filter(|&i| kind.is_none_or(|k| self.kind[i as usize] == k))
            .filter(|&i| {
                contains.is_none_or(|(key, text)| matches!(self.get(i, key), Some(Meta::Str(v)) if v.contains(text)))
            })
            .collect()
    }

    pub fn heap_bytes(&self) -> usize {
        let u32s = [&self.key, &self.name, &self.kind, &self.provider, &self.uid, &self.parent, &self.meta_start,
            &self.meta_key, &self.meta_val, &self.child_start, &self.child]
            .iter()
            .map(|v| v.capacity() * 4)
            .sum::<usize>();
        u32s + self.s.heap_bytes() + self.by_uid.capacity() * 13 + self.others.capacity() * size_of::<Value>()
    }
}
