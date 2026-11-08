//! The whole inventory as one store made of segments.
//!
//! speasy builds provider trees lazily, one by one, refreshes them independently, and HAPI fills servers and datasets
//! on first access. Each of these is an immutable segment (a Store). The inventory mounts them:
//! - attached under the root, by key: a provider's tree;
//! - filling a childless node of another segment: a lazy node loading (HAPI's load_into replaces the node's content
//!   with metadata + children; such a node has no children before).
//!
//! Node ids are (segment << 32 | row). A filled node keeps its id: its content comes from the filling segment's root.
//! Readers work on snapshots (an Arc). Each change makes a new snapshot and logs which child rows appeared or
//! disappeared under which node, so a Qt model replays the changes with matching begin/end row signals.
use crate::capi::Shared;
use crate::search::{bm25_multi, display_name, node_desc, parse, Place, SearchIndex};
use crate::store::{Meta, Store};
use hashbrown::HashMap;
use rayon::prelude::*;
use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

pub type NodeId = u64;
pub const ROOT: NodeId = 0;
const LEAF_KINDS: [&str; 2] = ["ParameterIndex", "TemplatedParameterIndex"];
const LOG_SIZE: usize = 4096;

pub fn node(seg: u32, row: u32) -> NodeId {
    (seg as u64) << 32 | row as u64
}

pub fn split(id: NodeId) -> (u32, u32) {
    ((id >> 32) as u32, id as u32)
}

pub fn segment_of(shared: &Shared) -> u32 {
    shared.generation as u32 // generations start at 1: segment 0 is the root's
}

pub struct Mount {
    pub seg: u32,
    pub shared: Arc<Shared>,
    pub host: NodeId, // ROOT when attached
    pub key: Box<str>, // the attach key, or the filled node's key
    place: Place,
    search: OnceLock<SearchIndex>,
}

impl Mount {
    pub fn store(&self) -> &Store {
        &self.shared.store
    }

    fn search(&self) -> &SearchIndex {
        self.search.get_or_init(|| SearchIndex::build(self.store(), &LEAF_KINDS, self.place.clone()))
    }
}

#[derive(Clone, Default)]
pub struct Snapshot {
    pub seq: u64,
    top: Vec<u32>,
    mounts: HashMap<u32, Arc<Mount>>,
    fills: HashMap<NodeId, u32>,
}

impl Snapshot {
    /// The segment and row holding `id`'s content: a filled node reads its filling segment's root.
    pub fn resolve(&self, id: NodeId) -> Option<(&Mount, u32)> {
        if id == ROOT {
            return None;
        }
        if let Some(seg) = self.fills.get(&id) {
            return self.mounts.get(seg).map(|m| (m.as_ref(), 0));
        }
        let (seg, row) = split(id);
        self.mounts.get(&seg).filter(|m| (row as usize) < m.store().len()).map(|m| (m.as_ref(), row))
    }

    pub fn at(&self, id: NodeId) -> Option<(&Store, u32)> {
        self.resolve(id).map(|(m, row)| (m.store(), row))
    }

    /// Attached segments under the root, in order: (key, segment).
    pub fn top(&self) -> Vec<(String, u32)> {
        self.top.iter().map(|s| (self.mounts[s].key.to_string(), *s)).collect()
    }

    pub fn child_count(&self, id: NodeId) -> u32 {
        if id == ROOT {
            return self.top.len() as u32;
        }
        self.at(id).map_or(0, |(st, row)| st.children(row).len() as u32)
    }

    pub fn child(&self, id: NodeId, i: u32) -> Option<NodeId> {
        if id == ROOT {
            return self.top.get(i as usize).map(|&seg| node(seg, 0));
        }
        let (m, row) = self.resolve(id)?;
        m.store().children(row).get(i as usize).map(|&c| node(m.seg, c))
    }

    pub fn parent(&self, id: NodeId) -> Option<NodeId> {
        if id == ROOT {
            return None;
        }
        let (seg, row) = split(id);
        let m = self.mounts.get(&seg)?;
        if row == 0 && m.host != ROOT {
            return self.parent(m.host); // a filling segment's root stands for its host
        }
        Some(match m.store().parent(row) {
            None => ROOT,                                   // an attached segment's root
            Some(0) if m.host != ROOT => m.host,            // a child of a filling segment's root: the filled node
            Some(p) => node(seg, p),
        })
    }

    pub fn row_in_parent(&self, id: NodeId) -> Option<u32> {
        let parent = self.parent(id)?;
        let (seg, row) = split(id);
        if parent == ROOT {
            return self.top.iter().position(|&s| s == seg).map(|i| i as u32);
        }
        let (m, prow) = self.resolve(parent)?;
        m.store().children(prow).binary_search(&row).ok().map(|i| i as u32)
    }

    pub fn find_child(&self, id: NodeId, key: &str) -> Option<NodeId> {
        if id == ROOT {
            return self.top.iter().find(|s| &*self.mounts[*s].key == key).map(|&s| node(s, 0));
        }
        let (m, row) = self.resolve(id)?;
        m.store().find_child(row, key).map(|c| node(m.seg, c))
    }

    /// The attribute key: a segment root's key is its mount key (its store calls it "").
    pub fn key(&self, id: NodeId) -> &str {
        match self.resolve(id) {
            Some((m, 0)) => &m.key,
            Some((m, row)) => m.store().key(row),
            None => "",
        }
    }

    pub fn display(&self, id: NodeId) -> String {
        match self.at(id) {
            Some((st, row)) => match st.get(row, "name") {
                Some(Meta::Str(n)) if n != "AMDA" => n.to_owned(),
                _ if row == 0 => self.key(id).to_owned(),
                _ => display_name(st, row),
            },
            None => String::new(),
        }
    }

    /// Display names and descriptions of `id` and its ancestors below the root, top first.
    fn place_of(&self, id: NodeId) -> Place {
        let mut chain: Vec<NodeId> = std::iter::successors(Some(id), |&n| self.parent(n)).collect();
        chain.pop(); // ROOT
        chain.reverse();
        let host = chain.pop();
        Place {
            above: chain.iter().map(|&n| self.display(n)).collect(),
            above_desc: chain.iter().filter_map(|&n| self.at(n).and_then(|(st, r)| node_desc(st, r))).collect(),
            root_name: host.map(|h| self.display(h)),
        }
    }

    fn mounts(&self) -> Vec<&Mount> {
        let mut m: Vec<&Mount> = self.mounts.values().map(Arc::as_ref).collect();
        m.sort_by_key(|m| m.seg); // stable order: results don't depend on hash order
        m
    }

    /// SciQLop's flat filter pass over every segment: (node, fuzzy score).
    pub fn search(&self, query: &str) -> Vec<(NodeId, i32)> {
        let q = parse(query);
        self.mounts()
            .par_iter()
            .flat_map_iter(|m| {
                let index = m.search();
                index.fuzzy(m.store(), &q).into_iter().map(move |(i, s)| (node(m.seg, index.leaves[i]), s))
            })
            .collect()
    }

    /// SciQLop's BM25F signal over every segment, with corpus-wide statistics: (node, raw score).
    pub fn bm25(&self, text: &str) -> Vec<(NodeId, f64)> {
        let mounts = self.mounts();
        let indexes: Vec<&SearchIndex> = mounts.par_iter().map(|m| m.search()).collect();
        bm25_multi(&indexes, text)
            .into_iter()
            .map(|(x, leaf, s)| (node(mounts[x].seg, indexes[x].leaves[leaf]), s))
            .collect()
    }

    /// Drops `seg` and every segment filling one of its nodes, recursively.
    fn drop_segment(&mut self, seg: u32) {
        let filling: Vec<u32> = self.mounts.values().filter(|m| split(m.host).0 == seg && m.host != ROOT)
            .map(|m| m.seg).collect();
        for s in filling {
            self.drop_segment(s);
        }
        self.fills.retain(|_, s| *s != seg);
        self.mounts.remove(&seg);
    }
}

pub const CHANGE_INSERTED: u32 = 1;
pub const CHANGE_REMOVED: u32 = 2;
pub const CHANGE_DATA: u32 = 3;

/// Child rows [position, position + count) of `host` appeared (INSERTED) or disappeared (REMOVED), or `host`'s own
/// data changed (DATA). `seq` is the snapshot's after the change.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Change {
    pub seq: u64,
    pub kind: u32,
    pub position: u32,
    pub count: u32,
    pub host: NodeId,
}

pub type Notify = extern "C" fn(ctx: *mut c_void);

#[derive(Default)]
pub struct Inventory {
    current: Mutex<Arc<Snapshot>>,
    log: Mutex<VecDeque<(Change, Arc<Snapshot>)>>,
    subscribers: Mutex<Vec<(u64, Notify, usize)>>,
    next_token: AtomicU64,
}

#[derive(Debug)]
pub enum MountError {
    NotAHost,
    NotChildless,
    AlreadyFilled,
    NotFinished,
}

impl Inventory {
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.current.lock().unwrap().clone()
    }

    /// Attaches a provider's tree under the root as `key`, replacing the one there (removed, then inserted in place).
    pub fn attach(&self, key: &str, shared: Arc<Shared>) -> u32 {
        let seg = segment_of(&shared);
        self.change(|snap, log| {
            let position = match snap.top.iter().position(|s| &*snap.mounts[s].key == key) {
                Some(p) => {
                    let old = snap.top.remove(p);
                    snap.drop_segment(old);
                    log(snap, CHANGE_REMOVED, ROOT, p as u32, 1);
                    p
                }
                None => snap.top.len(),
            };
            let root_name = match shared.store.get(0, "name") {
                Some(Meta::Str(n)) if n != "AMDA" => n.to_owned(),
                _ => key.to_owned(),
            };
            let place = Place { root_name: Some(root_name), ..Place::default() };
            snap.mounts.insert(seg, mount(seg, shared, ROOT, key, place));
            snap.top.insert(position, seg);
            log(snap, CHANGE_INSERTED, ROOT, position as u32, 1);
        });
        seg
    }

    pub fn detach(&self, key: &str) -> bool {
        let mut found = false;
        self.change(|snap, log| {
            if let Some(p) = snap.top.iter().position(|s| &*snap.mounts[s].key == key) {
                let old = snap.top.remove(p);
                snap.drop_segment(old);
                log(snap, CHANGE_REMOVED, ROOT, p as u32, 1);
                found = true;
            }
        });
        found
    }

    /// Fills `host`, a childless node, with `shared`'s root: its metadata and children.
    pub fn fill(&self, host: NodeId, shared: Arc<Shared>) -> Result<u32, MountError> {
        let seg = segment_of(&shared);
        if shared.store.len() == 0 {
            return Err(MountError::NotFinished);
        }
        let mut result = Ok(seg);
        self.change(|snap, log| {
            result = check_fillable(snap, host);
            if result.is_err() {
                return;
            }
            let (key, place) = (snap.key(host).to_owned(), snap.place_of(host));
            let children = shared.store.children(0).len() as u32;
            snap.mounts.insert(seg, mount(seg, shared.clone(), host, &key, place));
            snap.fills.insert(host, seg);
            if children > 0 {
                log(snap, CHANGE_INSERTED, host, 0, children);
            }
            log(snap, CHANGE_DATA, host, 0, 0);
        });
        result.map(|_| seg)
    }

    /// Changes after `after_seq`, each with the snapshot it leads to. Err when the log no longer reaches back there:
    /// the consumer resets from snapshot().
    pub fn changes_since(&self, after_seq: u64) -> Result<Vec<(Change, Arc<Snapshot>)>, ()> {
        let log = self.log.lock().unwrap();
        match log.front() {
            Some((first, _)) if first.seq > after_seq + 1 => Err(()),
            _ => Ok(log.iter().filter(|(c, _)| c.seq > after_seq).cloned().collect()),
        }
    }

    pub fn subscribe(&self, notify: Notify, ctx: *mut c_void) -> u64 {
        let token = self.next_token.fetch_add(1, Ordering::Relaxed) + 1;
        self.subscribers.lock().unwrap().push((token, notify, ctx as usize));
        token
    }

    pub fn unsubscribe(&self, token: u64) {
        self.subscribers.lock().unwrap().retain(|(t, _, _)| *t != token);
    }

    /// Writers are serialized by `current`'s lock. Each logged step publishes its own snapshot, so a consumer replays
    /// the exact intermediate states (e.g. a replaced provider: removed, then inserted). Subscribers are notified
    /// once the snapshot and log locks are released.
    fn change(&self, f: impl FnOnce(&mut Snapshot, &mut dyn FnMut(&mut Snapshot, u32, NodeId, u32, u32))) {
        {
            let mut current = self.current.lock().unwrap();
            let mut snap: Snapshot = (**current).clone();
            let mut steps: Vec<(Change, Arc<Snapshot>)> = Vec::new();
            f(&mut snap, &mut |s: &mut Snapshot, kind, host, position, count| {
                s.seq += 1;
                steps.push((Change { seq: s.seq, kind, position, count, host }, Arc::new(s.clone())));
            });
            if steps.is_empty() {
                return;
            }
            *current = steps.last().unwrap().1.clone();
            let mut log = self.log.lock().unwrap();
            log.extend(steps);
            while log.len() > LOG_SIZE {
                log.pop_front();
            }
        }
        // Notified under the lock: unsubscribe() then waits for a call in progress, so a consumer that unsubscribed
        // can be destroyed safely. Callbacks must only post an event (calling back into the inventory would deadlock).
        let subscribers = self.subscribers.lock().unwrap();
        for (_, notify, ctx) in subscribers.iter() {
            notify(*ctx as *mut c_void);
        }
    }
}

fn mount(seg: u32, shared: Arc<Shared>, host: NodeId, key: &str, place: Place) -> Arc<Mount> {
    Arc::new(Mount { seg, shared, host, key: key.into(), place, search: OnceLock::new() })
}

fn check_fillable(snap: &Snapshot, host: NodeId) -> Result<u32, MountError> {
    if host == ROOT || snap.resolve(host).is_none() {
        return Err(MountError::NotAHost);
    }
    if snap.fills.contains_key(&host) {
        return Err(MountError::AlreadyFilled);
    }
    if snap.child_count(host) > 0 {
        return Err(MountError::NotChildless);
    }
    Ok(0)
}
