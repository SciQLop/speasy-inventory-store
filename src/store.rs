//! The inventory as columns: one row per node, metadata as (key, value) runs, children as CSR.
use crate::interner::Interner;
use hashbrown::HashMap;

pub const NONE: u32 = u32::MAX;
/// A metadata value with this bit set indexes `others`, else it is a string id: 87% of the values are strings.
pub const OTHER: u32 = 1 << 31;

/// Every type speasy's providers put in metadata, kept apart so each comes back as it went in.
/// Strings anywhere, nested ones included, are interned ids.
pub enum Val {
    Str(u32),
    Int(i64),
    Float(f64),
    Bool(bool),
    Null,
    List(Vec<Val>),
    Tuple(Vec<Val>),
    Dict(Vec<(u32, Val)>),
    /// Anything else (numpy scalar, datetime, int beyond i64...): the binding keeps the object, the store its text.
    Opaque(u32),
}

pub enum Meta<'a> {
    Str(&'a str),
    Other(&'a Val),
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
    others: Vec<Val>,
    opaque_text: Vec<Box<str>>,
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

    pub fn push_meta(&mut self, key: &str, value: Val) {
        let k = self.s.intern(key);
        self.meta_key.push(k);
        let v = match value {
            Val::Str(id) => id,
            other => {
                self.others.push(other);
                OTHER | (self.others.len() - 1) as u32
            }
        };
        self.meta_val.push(v);
    }

    /// Registers an opaque value by its text (for JSON export and C readers); returns its index.
    pub fn opaque(&mut self, text: &str) -> Val {
        self.opaque_text.push(text.into());
        Val::Opaque((self.opaque_text.len() - 1) as u32)
    }

    pub fn opaque_text(&self, i: u32) -> &str {
        &self.opaque_text[i as usize]
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

    fn meta_range(&self, i: u32) -> std::ops::Range<usize> {
        self.meta_start[i as usize] as usize..self.meta_start[i as usize + 1] as usize
    }

    fn value(&self, v: u32) -> Meta<'_> {
        if v & OTHER != 0 { Meta::Other(&self.others[(v & !OTHER) as usize]) } else { Meta::Str(self.s.str(v)) }
    }

    pub fn meta(&self, i: u32) -> impl Iterator<Item = (&str, Meta<'_>)> {
        self.meta_range(i).map(|m| (self.s.str(self.meta_key[m]), self.value(self.meta_val[m])))
    }

    /// (key id, value) pairs, for callers that cache one object per string id: a value with OTHER set goes to
    /// `other`, else it is a string id.
    pub fn raw_meta(&self, i: u32) -> impl Iterator<Item = (u32, u32)> + '_ {
        self.meta_range(i).map(|m| (self.meta_key[m], self.meta_val[m]))
    }

    pub fn other(&self, v: u32) -> &Val {
        &self.others[(v & !OTHER) as usize]
    }

    /// key, name, kind, provider, uid string ids.
    pub fn raw(&self, i: u32) -> [u32; 5] {
        let i = i as usize;
        [self.key[i], self.name[i], self.kind[i], self.provider[i], self.uid[i]]
    }

    pub fn get(&self, i: u32, key: &str) -> Option<Meta<'_>> {
        let k = self.s.get(key)?;
        self.meta_range(i).find(|&m| self.meta_key[m] == k).map(|m| self.value(self.meta_val[m]))
    }

    pub fn find_child(&self, i: u32, key: &str) -> Option<u32> {
        let k = self.s.get(key)?;
        self.children(i).iter().copied().find(|&c| self.key[c as usize] == k)
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
        fn val_heap(v: &Val) -> usize {
            match v {
                Val::List(x) | Val::Tuple(x) => x.capacity() * size_of::<Val>() + x.iter().map(val_heap).sum::<usize>(),
                Val::Dict(x) => {
                    x.capacity() * size_of::<(u32, Val)>() + x.iter().map(|(_, v)| val_heap(v)).sum::<usize>()
                }
                _ => 0,
            }
        }
        let u32s = [&self.key, &self.name, &self.kind, &self.provider, &self.uid, &self.parent, &self.meta_start,
            &self.meta_key, &self.meta_val, &self.child_start, &self.child]
            .iter()
            .map(|v| v.capacity() * 4)
            .sum::<usize>();
        u32s + self.s.heap_bytes()
            + self.by_uid.capacity() * 13
            + self.others.capacity() * size_of::<Val>()
            + self.others.iter().map(val_heap).sum::<usize>()
            + self.opaque_text.iter().map(|t| t.len() + 16).sum::<usize>()
    }
}
