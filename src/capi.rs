//! The C interface: a table of functions over one shared, immutable store, handed to C/C++ in a PyCapsule.
//!
//! Lifetime: the table's `handle` is one reference on the store (an `Arc`). A consumer that keeps the store calls
//! `retain(handle)` and later `release(handle)`; the capsule releases its own reference when Python frees it.
//! The store is immutable once shared, so every function may run on any thread without the GIL.
//! Strings are borrowed UTF-8 (pointer + length), valid while the consumer holds a reference.
//! See include/inventory_store.h.
use crate::json::value_json;
use crate::search::{parse, SearchIndex};
use crate::store::{Store, Val, NONE, OTHER};
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

pub const ABI_VERSION: u32 = 1;
pub const CAPSULE_NAME: &std::ffi::CStr = c"speasy.inventory_store.v1";
const LEAF_KINDS: [&str; 2] = ["ParameterIndex", "TemplatedParameterIndex"];

/// The store plus what is derived from it lazily; shared by the Python object and every C consumer.
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

    pub fn search_index(&self) -> &SearchIndex {
        self.search.get_or_init(|| SearchIndex::build(&self.store, &LEAF_KINDS))
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

/// A metadata value: scalars typed, composites only tagged (use meta_text for a rendering), opaque as its str().
#[repr(C)]
pub struct InvValue {
    pub kind: u32,
    pub int_value: i64,
    pub float_value: f64,
    pub text: InvStr,
}

pub type TextSink = extern "C" fn(ctx: *mut c_void, text: InvStr);
pub type HitSink = extern "C" fn(ctx: *mut c_void, row: u32, score: f64);

pub const FIELD_KEY: u32 = 0;
pub const FIELD_NAME: u32 = 1;
pub const FIELD_KIND: u32 = 2;
pub const FIELD_PROVIDER: u32 = 3;
pub const FIELD_UID: u32 = 4;

type H = *const c_void;

#[repr(C)]
pub struct InventoryStoreApi {
    pub abi_version: u32,
    pub struct_size: u32,
    pub handle: H,
    pub retain: extern "C" fn(H),
    pub release: extern "C" fn(H),
    pub generation: extern "C" fn(H) -> u64,
    pub node_count: extern "C" fn(H) -> u32,
    pub child_count: extern "C" fn(H, u32) -> u32,
    pub child: extern "C" fn(H, u32, u32) -> u32,
    pub parent: extern "C" fn(H, u32) -> u32,
    pub row_in_parent: extern "C" fn(H, u32) -> u32,
    pub find_child: extern "C" fn(H, u32, InvStr) -> u32,
    pub field: extern "C" fn(H, u32, u32) -> InvStr,
    pub meta_count: extern "C" fn(H, u32) -> u32,
    pub meta_key: extern "C" fn(H, u32, u32) -> InvStr,
    pub meta_value: extern "C" fn(H, u32, u32, *mut InvValue),
    pub meta_find: extern "C" fn(H, u32, InvStr) -> u32,
    pub meta_text: extern "C" fn(H, u32, u32, TextSink, *mut c_void),
    pub search: extern "C" fn(H, InvStr, HitSink, *mut c_void) -> u32,
    pub bm25: extern "C" fn(H, InvStr, HitSink, *mut c_void) -> u32,
}

/// Owns the capsule's reference; dropped (released) when Python frees the capsule.
pub struct Capsule(pub InventoryStoreApi);
// The handle is an Arc<Shared>, which is Send + Sync.
unsafe impl Send for Capsule {}

impl Drop for Capsule {
    fn drop(&mut self) {
        release(self.0.handle);
    }
}

fn shared<'a>(h: H) -> &'a Shared {
    // the caller holds a reference (the capsule's or its own retain), so the Arc is alive
    unsafe { &*(h as *const Shared) }
}

fn valid(st: &Store, row: u32) -> bool {
    (row as usize) < st.len()
}

pub fn api(shared: &Arc<Shared>) -> InventoryStoreApi {
    InventoryStoreApi {
        abi_version: ABI_VERSION,
        struct_size: size_of::<InventoryStoreApi>() as u32,
        handle: Arc::into_raw(shared.clone()) as H,
        retain,
        release,
        generation,
        node_count,
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
        meta_text,
        search,
        bm25,
    }
}

extern "C" fn retain(h: H) {
    unsafe { Arc::increment_strong_count(h as *const Shared) }
}

extern "C" fn release(h: H) {
    unsafe { Arc::decrement_strong_count(h as *const Shared) }
}

extern "C" fn generation(h: H) -> u64 {
    shared(h).generation
}

extern "C" fn node_count(h: H) -> u32 {
    shared(h).store.len() as u32
}

extern "C" fn child_count(h: H, row: u32) -> u32 {
    let st = &shared(h).store;
    if valid(st, row) { st.children(row).len() as u32 } else { 0 }
}

extern "C" fn child(h: H, row: u32, i: u32) -> u32 {
    let st = &shared(h).store;
    if !valid(st, row) {
        return NONE;
    }
    st.children(row).get(i as usize).copied().unwrap_or(NONE)
}

extern "C" fn parent(h: H, row: u32) -> u32 {
    let st = &shared(h).store;
    if valid(st, row) { st.parent(row).unwrap_or(NONE) } else { NONE }
}

/// Position among its siblings (QModelIndex::row()); children are stored sorted by row id.
extern "C" fn row_in_parent(h: H, row: u32) -> u32 {
    let st = &shared(h).store;
    match valid(st, row).then(|| st.parent(row)).flatten() {
        Some(p) => st.children(p).binary_search(&row).map_or(NONE, |i| i as u32),
        None => 0,
    }
}

extern "C" fn find_child(h: H, row: u32, key: InvStr) -> u32 {
    let st = &shared(h).store;
    match (valid(st, row), unsafe { key.as_str() }) {
        (true, Some(k)) => st.find_child(row, k).unwrap_or(NONE),
        _ => NONE,
    }
}

extern "C" fn field(h: H, row: u32, which: u32) -> InvStr {
    let st = &shared(h).store;
    if !valid(st, row) {
        return InvStr::EMPTY;
    }
    match which {
        FIELD_KEY => InvStr::of(st.key(row)),
        FIELD_NAME => InvStr::of(st.name(row)),
        FIELD_KIND => InvStr::of(st.kind(row)),
        FIELD_PROVIDER => InvStr::of(st.provider(row)),
        FIELD_UID => InvStr::of(st.uid(row)),
        _ => InvStr::EMPTY,
    }
}

extern "C" fn meta_count(h: H, row: u32) -> u32 {
    let st = &shared(h).store;
    if valid(st, row) { st.raw_meta(row).count() as u32 } else { 0 }
}

fn entry(st: &Store, row: u32, i: u32) -> Option<(u32, u32)> {
    valid(st, row).then(|| st.raw_meta(row).nth(i as usize)).flatten()
}

extern "C" fn meta_key(h: H, row: u32, i: u32) -> InvStr {
    let st = &shared(h).store;
    entry(st, row, i).map_or(InvStr::EMPTY, |(k, _)| InvStr::of(st.s.str(k)))
}

fn to_value(st: &Store, v: u32) -> InvValue {
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

extern "C" fn meta_value(h: H, row: u32, i: u32, out: *mut InvValue) {
    let st = &shared(h).store;
    if out.is_null() {
        return;
    }
    let value = entry(st, row, i).map_or(
        InvValue { kind: INV_NULL, int_value: 0, float_value: 0.0, text: InvStr::EMPTY },
        |(_, v)| to_value(st, v),
    );
    unsafe { out.write(value) }
}

/// Index of the metadata entry named `key`, or NONE.
extern "C" fn meta_find(h: H, row: u32, key: InvStr) -> u32 {
    let st = &shared(h).store;
    let (Some(k), true) = (unsafe { key.as_str() }.and_then(|k| st.s.get(k)), valid(st, row)) else { return NONE };
    st.raw_meta(row).position(|(kk, _)| kk == k).map_or(NONE, |i| i as u32)
}

/// Any value as text: strings as is, scalars printed, composites as JSON. The text lives during the call only.
extern "C" fn meta_text(h: H, row: u32, i: u32, sink: TextSink, ctx: *mut c_void) {
    let st = &shared(h).store;
    let Some((_, v)) = entry(st, row, i) else { return };
    let text = if v & OTHER == 0 {
        st.s.str(v).to_owned()
    } else {
        match st.other(v) {
            Val::Str(id) => st.s.str(*id).to_owned(),
            Val::Opaque(i) => st.opaque_text(*i).to_owned(),
            other => value_json(st, other),
        }
    };
    sink(ctx, InvStr::of(&text));
}

/// SciQLop's flat filter pass (query syntax, filters, fuzzy score); one sink call per hit. Returns the hit count.
extern "C" fn search(h: H, query: InvStr, sink: HitSink, ctx: *mut c_void) -> u32 {
    let sh = shared(h);
    let Some(q) = (unsafe { query.as_str() }) else { return 0 };
    let index = sh.search_index();
    let hits = index.fuzzy(&sh.store, &parse(q));
    for &(i, s) in &hits {
        sink(ctx, index.leaves[i], s as f64);
    }
    hits.len() as u32
}

/// SciQLop's BM25F signal; one sink call per leaf matching a term. Returns the hit count.
extern "C" fn bm25(h: H, text: InvStr, sink: HitSink, ctx: *mut c_void) -> u32 {
    let sh = shared(h);
    let Some(t) = (unsafe { text.as_str() }) else { return 0 };
    let index = sh.search_index();
    let hits = index.bm25(t);
    for &(i, s) in &hits {
        sink(ctx, index.leaves[i], s);
    }
    hits.len() as u32
}
