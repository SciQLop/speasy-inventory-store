//! The C interface of the inventory (version 2): a table of functions handed to C/C++ in a PyCapsule.
//!
//! - The inventory handle (`H`) is one reference on the inventory (an `Arc`): `retain`/`release` it.
//! - Reads go through a snapshot (`S`, also an `Arc`): immutable, so any thread may read it without the GIL.
//!   `snapshot(H)` gives the current one, released with `snapshot_release`.
//! - The inventory grows and changes (lazy providers, lazy nodes, refreshes): `subscribe` gets a call after each
//!   change (from the changing thread: only post an event there), and `changes_since` replays the changes in order,
//!   each with the snapshot it leads to, so a Qt model emits begin/end row signals around the right states.
//! Strings are borrowed UTF-8 (pointer + length), valid while the snapshot they come from is held.
//! See include/inventory_store.h.
use crate::inventory::{Change, Inventory, NodeId, Notify, Snapshot, ROOT};
use crate::json::value_json;
use crate::search::SearchIndex;
use crate::store::{Store, Val, NONE, OTHER};
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

pub const ABI_VERSION: u32 = 2;
pub const CAPSULE_NAME: &std::ffi::CStr = c"speasy.inventory_store.v2";
const NO_NODE: NodeId = u64::MAX;

/// One segment: a store plus what is derived from it lazily; shared by its Python object and the inventory.
pub struct Shared {
    pub store: Store,
    pub search: OnceLock<SearchIndex>,
    pub generation: u64,
}

impl Shared {
    pub fn new(store: Store) -> Shared {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Shared { store, search: OnceLock::new(), generation: NEXT.fetch_add(1, Ordering::Relaxed) }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct InvStr {
    pub ptr: *const u8,
    pub len: usize,
}

impl InvStr {
    fn of(s: &str) -> InvStr {
        InvStr { ptr: s.as_ptr(), len: s.len() }
    }
    const EMPTY: InvStr = InvStr { ptr: std::ptr::null(), len: 0 };
    unsafe fn as_str<'a>(self) -> Option<&'a str> {
        if self.ptr.is_null() {
            return Some("");
        }
        std::str::from_utf8(unsafe { std::slice::from_raw_parts(self.ptr, self.len) }).ok()
    }
}

pub const INV_STR: u32 = 1;
pub const INV_INT: u32 = 2;
pub const INV_FLOAT: u32 = 3;
pub const INV_BOOL: u32 = 4;
pub const INV_NULL: u32 = 5;
pub const INV_LIST: u32 = 6;
pub const INV_TUPLE: u32 = 7;
pub const INV_DICT: u32 = 8;
pub const INV_OPAQUE: u32 = 9;

#[repr(C)]
pub struct InvValue {
    pub kind: u32,
    pub int_value: i64,
    pub float_value: f64,
    pub text: InvStr,
}

pub type TextSink = extern "C" fn(ctx: *mut c_void, text: InvStr);
pub type HitSink = extern "C" fn(ctx: *mut c_void, node: NodeId, score: f64);
pub type ChangeSink = extern "C" fn(ctx: *mut c_void, change: *const Change, after: S);

pub const FIELD_KEY: u32 = 0;
pub const FIELD_NAME: u32 = 1;
pub const FIELD_KIND: u32 = 2;
pub const FIELD_PROVIDER: u32 = 3;
pub const FIELD_UID: u32 = 4;

type H = *const c_void;
type S = *const c_void;

#[repr(C)]
pub struct InventoryApi {
    pub abi_version: u32,
    pub struct_size: u32,
    pub handle: H,
    pub retain: extern "C" fn(H),
    pub release: extern "C" fn(H),
    pub snapshot: extern "C" fn(H) -> S,
    pub snapshot_retain: extern "C" fn(S),
    pub snapshot_release: extern "C" fn(S),
    pub snapshot_seq: extern "C" fn(S) -> u64,
    pub subscribe: extern "C" fn(H, Notify, *mut c_void) -> u64,
    pub unsubscribe: extern "C" fn(H, u64),
    pub changes_since: extern "C" fn(H, u64, ChangeSink, *mut c_void) -> u32,
    pub child_count: extern "C" fn(S, NodeId) -> u32,
    pub child: extern "C" fn(S, NodeId, u32) -> NodeId,
    pub parent: extern "C" fn(S, NodeId) -> NodeId,
    pub row_in_parent: extern "C" fn(S, NodeId) -> u32,
    pub find_child: extern "C" fn(S, NodeId, InvStr) -> NodeId,
    pub field: extern "C" fn(S, NodeId, u32) -> InvStr,
    pub meta_count: extern "C" fn(S, NodeId) -> u32,
    pub meta_key: extern "C" fn(S, NodeId, u32) -> InvStr,
    pub meta_value: extern "C" fn(S, NodeId, u32, *mut InvValue),
    pub meta_find: extern "C" fn(S, NodeId, InvStr) -> u32,
    pub meta_text: extern "C" fn(S, NodeId, u32, TextSink, *mut c_void),
    pub search: extern "C" fn(S, InvStr, HitSink, *mut c_void) -> u32,
    pub bm25: extern "C" fn(S, InvStr, HitSink, *mut c_void) -> u32,
}

/// Owns the capsule's reference; released when Python frees the capsule.
pub struct Capsule(pub InventoryApi);
// The handle is an Arc<Inventory>, which is Send + Sync.
unsafe impl Send for Capsule {}

impl Drop for Capsule {
    fn drop(&mut self) {
        release(self.0.handle);
    }
}

fn inventory<'a>(h: H) -> &'a Inventory {
    unsafe { &*(h as *const Inventory) }
}

fn snap<'a>(s: S) -> &'a Snapshot {
    unsafe { &*(s as *const Snapshot) }
}

pub fn api(inv: &Arc<Inventory>) -> InventoryApi {
    InventoryApi {
        abi_version: ABI_VERSION,
        struct_size: size_of::<InventoryApi>() as u32,
        handle: Arc::into_raw(inv.clone()) as H,
        retain,
        release,
        snapshot,
        snapshot_retain,
        snapshot_release,
        snapshot_seq,
        subscribe,
        unsubscribe,
        changes_since,
        child_count,
        child,
        parent,
        row_in_parent,
        find_child,
        field,
        meta_count,
        meta_key,
        meta_value,
        meta_find,
        meta_text: meta_text_entry,
        search,
        bm25,
    }
}

extern "C" fn retain(h: H) {
    unsafe { Arc::increment_strong_count(h as *const Inventory) }
}

extern "C" fn release(h: H) {
    unsafe { Arc::decrement_strong_count(h as *const Inventory) }
}

extern "C" fn snapshot(h: H) -> S {
    Arc::into_raw(inventory(h).snapshot()) as S
}

extern "C" fn snapshot_retain(s: S) {
    unsafe { Arc::increment_strong_count(s as *const Snapshot) }
}

extern "C" fn snapshot_release(s: S) {
    unsafe { Arc::decrement_strong_count(s as *const Snapshot) }
}

extern "C" fn snapshot_seq(s: S) -> u64 {
    snap(s).seq
}

extern "C" fn subscribe(h: H, notify: Notify, ctx: *mut c_void) -> u64 {
    inventory(h).subscribe(notify, ctx)
}

extern "C" fn unsubscribe(h: H, token: u64) {
    inventory(h).unsubscribe(token)
}

/// Replays the changes after `after_seq` in order; the snapshot passed with each is borrowed for the call (retain it
/// to keep it). Returns how many were replayed, or 0xFFFFFFFF when the log no longer reaches `after_seq`: reset.
extern "C" fn changes_since(h: H, after_seq: u64, sink: ChangeSink, ctx: *mut c_void) -> u32 {
    match inventory(h).changes_since(after_seq) {
        Ok(changes) => {
            for (change, after) in &changes {
                sink(ctx, change, Arc::as_ptr(after) as S);
            }
            changes.len() as u32
        }
        Err(()) => NONE,
    }
}

extern "C" fn child_count(s: S, id: NodeId) -> u32 {
    snap(s).child_count(id)
}

extern "C" fn child(s: S, id: NodeId, i: u32) -> NodeId {
    snap(s).child(id, i).unwrap_or(NO_NODE)
}

extern "C" fn parent(s: S, id: NodeId) -> NodeId {
    snap(s).parent(id).unwrap_or(NO_NODE)
}

extern "C" fn row_in_parent(s: S, id: NodeId) -> u32 {
    snap(s).row_in_parent(id).unwrap_or(if id == ROOT { 0 } else { NONE })
}

extern "C" fn find_child(s: S, id: NodeId, key: InvStr) -> NodeId {
    match unsafe { key.as_str() } {
        Some(k) => snap(s).find_child(id, k).unwrap_or(NO_NODE),
        None => NO_NODE,
    }
}

extern "C" fn field(s: S, id: NodeId, which: u32) -> InvStr {
    let sn = snap(s);
    if which == FIELD_KEY {
        return InvStr::of(sn.key(id));
    }
    let Some((st, row)) = sn.at(id) else { return InvStr::EMPTY };
    match which {
        FIELD_NAME => InvStr::of(st.name(row)),
        FIELD_KIND => InvStr::of(st.kind(row)),
        FIELD_PROVIDER => InvStr::of(st.provider(row)),
        FIELD_UID => InvStr::of(st.uid(row)),
        _ => InvStr::EMPTY,
    }
}

extern "C" fn meta_count(s: S, id: NodeId) -> u32 {
    snap(s).at(id).map_or(0, |(st, row)| st.raw_meta(row).count() as u32)
}

fn entry(sn: &Snapshot, id: NodeId, i: u32) -> Option<(&Store, u32, u32)> {
    let (st, row) = sn.at(id)?;
    st.raw_meta(row).nth(i as usize).map(|(k, v)| (st, k, v))
}

extern "C" fn meta_key(s: S, id: NodeId, i: u32) -> InvStr {
    entry(snap(s), id, i).map_or(InvStr::EMPTY, |(st, k, _)| InvStr::of(st.s.str(k)))
}

pub fn value_of(st: &Store, v: u32) -> InvValue {
    let mut out = InvValue { kind: INV_NULL, int_value: 0, float_value: 0.0, text: InvStr::EMPTY };
    if v & OTHER == 0 {
        out.kind = INV_STR;
        out.text = InvStr::of(st.s.str(v));
        return out;
    }
    match st.other(v) {
        Val::Str(id) => (out.kind, out.text) = (INV_STR, InvStr::of(st.s.str(*id))),
        Val::Int(i) => (out.kind, out.int_value) = (INV_INT, *i),
        Val::Float(f) => (out.kind, out.float_value) = (INV_FLOAT, *f),
        Val::Bool(b) => (out.kind, out.int_value) = (INV_BOOL, *b as i64),
        Val::Null => out.kind = INV_NULL,
        Val::List(_) => out.kind = INV_LIST,
        Val::Tuple(_) => out.kind = INV_TUPLE,
        Val::Dict(_) => out.kind = INV_DICT,
        Val::Opaque(i) => (out.kind, out.text) = (INV_OPAQUE, InvStr::of(st.opaque_text(*i))),
    }
    out
}

extern "C" fn meta_value(s: S, id: NodeId, i: u32, out: *mut InvValue) {
    if out.is_null() {
        return;
    }
    let value = entry(snap(s), id, i).map_or(
        InvValue { kind: INV_NULL, int_value: 0, float_value: 0.0, text: InvStr::EMPTY },
        |(st, _, v)| value_of(st, v),
    );
    unsafe { out.write(value) }
}

extern "C" fn meta_find(s: S, id: NodeId, key: InvStr) -> u32 {
    let Some((st, row)) = snap(s).at(id) else { return NONE };
    let Some(k) = (unsafe { key.as_str() }).and_then(|k| st.s.get(k)) else { return NONE };
    st.raw_meta(row).position(|(kk, _)| kk == k).map_or(NONE, |i| i as u32)
}

/// Any value as text: strings as is, scalars printed, composites as JSON.
pub fn meta_text(st: &Store, v: u32) -> String {
    if v & OTHER == 0 {
        return st.s.str(v).to_owned();
    }
    match st.other(v) {
        Val::Str(id) => st.s.str(*id).to_owned(),
        Val::Opaque(i) => st.opaque_text(*i).to_owned(),
        other => value_json(st, other),
    }
}

/// The text lives during the call only.
extern "C" fn meta_text_entry(s: S, id: NodeId, i: u32, sink: TextSink, ctx: *mut c_void) {
    let Some((st, _, v)) = entry(snap(s), id, i) else { return };
    sink(ctx, InvStr::of(&meta_text(st, v)));
}

extern "C" fn search(s: S, query: InvStr, sink: HitSink, ctx: *mut c_void) -> u32 {
    let Some(q) = (unsafe { query.as_str() }) else { return 0 };
    let hits = snap(s).search(q);
    for &(id, score) in &hits {
        sink(ctx, id, score as f64);
    }
    hits.len() as u32
}

extern "C" fn bm25(s: S, text: InvStr, sink: HitSink, ctx: *mut c_void) -> u32 {
    let Some(t) = (unsafe { text.as_str() }) else { return 0 };
    let hits = snap(s).bm25(t);
    for &(id, score) in &hits {
        sink(ctx, id, score);
    }
    hits.len() as u32
}
