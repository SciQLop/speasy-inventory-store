//! The database-like C interface (include/inventory_db.h): C/C++ holds no pointer into the inventory.
//!
//! - One connection per consumer, an integer from a registry; everything else is plain values: node ids, versions.
//! - A version is pinned by the connection (pin/unpin): its snapshot stays alive in the registry, not in the caller.
//! - Results are copied out: into the caller's buffer, or through a callback whose strings are valid during the call.
//! - Every entry point checks its arguments, catches panics, and returns a status; a wrong id is NOT_FOUND, never a
//!   read of freed memory.
use crate::capi::{meta_text, value_of, InvValue};
use crate::inventory::{Change, Inventory, NodeId, Snapshot};
use hashbrown::HashMap;
use std::ffi::{c_char, c_void};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex, OnceLock};

pub const DB_ABI_VERSION: u32 = 1;
pub const DB_CAPSULE_NAME: &std::ffi::CStr = c"speasy.inventory_db.v1";
pub const CURRENT: u64 = u64::MAX;

pub const OK: i32 = 0;
pub const NOT_FOUND: i32 = 1;
pub const BAD_CONNECTION: i32 = 2;
pub const BAD_VERSION: i32 = 3;
pub const BAD_ARGUMENT: i32 = 4;
pub const BUFFER_TOO_SMALL: i32 = 5;
pub const RESET_NEEDED: i32 = 6;
pub const INTERNAL: i32 = 7;

pub const TEXT_KEY: u32 = 0;
pub const TEXT_NAME: u32 = 1;
pub const TEXT_KIND: u32 = 2;
pub const TEXT_PROVIDER: u32 = 3;
pub const TEXT_UID: u32 = 4;
pub const TEXT_DISPLAY: u32 = 5;
pub const TEXT_TOOLTIP: u32 = 6;

pub type TextSink = extern "C" fn(ctx: *mut c_void, text: *const c_char, len: usize);
pub type MetaSink = extern "C" fn(ctx: *mut c_void, key: *const c_char, key_len: usize, value: *const InvValue);
pub type HitsSink = extern "C" fn(ctx: *mut c_void, nodes: *const NodeId, scores: *const f64, count: u32);
pub type ChangeSink = extern "C" fn(ctx: *mut c_void, change: *const Change);
pub type Notify = crate::inventory::Notify;

#[repr(C)]
pub struct NodeInfo {
    pub parent: NodeId,
    pub row: u32,
    pub child_count: u32,
}

type Conn = u64;

#[repr(C)]
pub struct InventoryDbApi {
    pub abi_version: u32,
    pub struct_size: u32,
    pub connect: extern "C" fn(*const InventoryDbApi, *mut Conn) -> i32,
    pub disconnect: extern "C" fn(Conn) -> i32,
    pub current_version: extern "C" fn(Conn, *mut u64) -> i32,
    pub pin: extern "C" fn(Conn, u64) -> i32,
    pub unpin: extern "C" fn(Conn, u64) -> i32,
    pub subscribe: extern "C" fn(Conn, Option<Notify>, *mut c_void, *mut u64) -> i32,
    pub unsubscribe: extern "C" fn(Conn, u64) -> i32,
    pub changes: extern "C" fn(Conn, u64, Option<ChangeSink>, *mut c_void) -> i32,
    pub node: extern "C" fn(Conn, u64, NodeId, *mut NodeInfo) -> i32,
    pub child: extern "C" fn(Conn, u64, NodeId, u32, *mut NodeId) -> i32,
    pub children: extern "C" fn(Conn, u64, NodeId, *mut NodeId, u32, *mut u32) -> i32,
    pub find_child: extern "C" fn(Conn, u64, NodeId, *const c_char, usize, *mut NodeId) -> i32,
    pub text: extern "C" fn(Conn, u64, NodeId, u32, Option<TextSink>, *mut c_void) -> i32,
    pub meta: extern "C" fn(Conn, u64, NodeId, *const c_char, usize, Option<MetaSink>, *mut c_void) -> i32,
    pub meta_all: extern "C" fn(Conn, u64, NodeId, Option<MetaSink>, *mut c_void) -> i32,
    pub search: extern "C" fn(Conn, u64, *const c_char, usize, Option<HitsSink>, *mut c_void) -> i32,
    pub bm25: extern "C" fn(Conn, u64, *const c_char, usize, Option<HitsSink>, *mut c_void) -> i32,
    /// The inventory this table connects to: used by `connect` only, while the capsule is alive.
    pub reserved: *const c_void,
}

/// Owns the capsule's reference on the inventory.
pub struct DbCapsule(pub InventoryDbApi);
unsafe impl Send for DbCapsule {}

impl Drop for DbCapsule {
    fn drop(&mut self) {
        unsafe { Arc::decrement_strong_count(self.0.reserved as *const Inventory) }
    }
}

pub fn api(inv: &Arc<Inventory>) -> InventoryDbApi {
    InventoryDbApi {
        abi_version: DB_ABI_VERSION,
        struct_size: size_of::<InventoryDbApi>() as u32,
        connect,
        disconnect,
        current_version,
        pin,
        unpin,
        subscribe,
        unsubscribe,
        changes,
        node,
        child,
        children,
        find_child,
        text,
        meta,
        meta_all,
        search,
        bm25,
        reserved: Arc::into_raw(inv.clone()) as *const c_void,
    }
}

struct Connection {
    inventory: Arc<Inventory>,
    pins: HashMap<u64, (Arc<Snapshot>, u32)>,
    subscriptions: Vec<u64>,
}

#[derive(Default)]
struct Registry {
    next: Conn,
    connections: HashMap<Conn, Connection>,
}

fn registry() -> std::sync::MutexGuard<'static, Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// Runs an entry point: its error becomes a status, a panic becomes INTERNAL instead of aborting the host.
fn guarded(f: impl FnOnce() -> Result<(), i32>) -> i32 {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(())) => OK,
        Ok(Err(status)) => status,
        Err(_) => INTERNAL,
    }
}

fn out<T>(ptr: *mut T, value: T) -> Result<(), i32> {
    if ptr.is_null() {
        return Err(BAD_ARGUMENT);
    }
    unsafe { ptr.write(value) };
    Ok(())
}

fn input<'a>(ptr: *const c_char, len: usize) -> Result<&'a str, i32> {
    if ptr.is_null() {
        return if len == 0 { Ok("") } else { Err(BAD_ARGUMENT) };
    }
    std::str::from_utf8(unsafe { std::slice::from_raw_parts(ptr as *const u8, len) }).map_err(|_| BAD_ARGUMENT)
}

fn inventory_of(conn: Conn) -> Result<Arc<Inventory>, i32> {
    registry().connections.get(&conn).map(|c| c.inventory.clone()).ok_or(BAD_CONNECTION)
}

/// The snapshot a query reads: a version the connection pinned, or CURRENT.
fn snapshot(conn: Conn, version: u64) -> Result<Arc<Snapshot>, i32> {
    let reg = registry();
    let c = reg.connections.get(&conn).ok_or(BAD_CONNECTION)?;
    if version == CURRENT {
        return Ok(c.inventory.snapshot());
    }
    c.pins.get(&version).map(|(s, _)| s.clone()).ok_or(BAD_VERSION)
}

fn send_text(sink: TextSink, ctx: *mut c_void, text: &str) {
    sink(ctx, text.as_ptr() as *const c_char, text.len());
}

extern "C" fn connect(api: *const InventoryDbApi, conn: *mut Conn) -> i32 {
    guarded(|| {
        if api.is_null() || unsafe { (*api).reserved.is_null() } {
            return Err(BAD_ARGUMENT);
        }
        let inventory = unsafe {
            let raw = (*api).reserved as *const Inventory;
            Arc::increment_strong_count(raw);
            Arc::from_raw(raw)
        };
        let mut reg = registry();
        reg.next += 1;
        let id = reg.next;
        reg.connections.insert(id, Connection { inventory, pins: HashMap::new(), subscriptions: Vec::new() });
        out(conn, id)
    })
}

extern "C" fn disconnect(conn: Conn) -> i32 {
    guarded(|| {
        let c = registry().connections.remove(&conn).ok_or(BAD_CONNECTION)?;
        for token in &c.subscriptions {
            c.inventory.unsubscribe(*token); // waits for a notification in progress
        }
        Ok(())
    })
}

extern "C" fn current_version(conn: Conn, version: *mut u64) -> i32 {
    guarded(|| out(version, inventory_of(conn)?.snapshot().seq))
}

/// Keeps version `version` readable by this connection until unpinned (pins count).
extern "C" fn pin(conn: Conn, version: u64) -> i32 {
    guarded(|| {
        let inventory = inventory_of(conn)?;
        let snap = inventory.snapshot_at(version).ok_or(BAD_VERSION)?;
        let mut reg = registry();
        let c = reg.connections.get_mut(&conn).ok_or(BAD_CONNECTION)?;
        c.pins.entry(version).or_insert((snap, 0)).1 += 1;
        Ok(())
    })
}

extern "C" fn unpin(conn: Conn, version: u64) -> i32 {
    guarded(|| {
        let mut reg = registry();
        let c = reg.connections.get_mut(&conn).ok_or(BAD_CONNECTION)?;
        let count = &mut c.pins.get_mut(&version).ok_or(BAD_VERSION)?.1;
        *count -= 1;
        if *count == 0 {
            c.pins.remove(&version);
        }
        Ok(())
    })
}

extern "C" fn subscribe(conn: Conn, notify: Option<Notify>, ctx: *mut c_void, token: *mut u64) -> i32 {
    guarded(|| {
        let notify = notify.ok_or(BAD_ARGUMENT)?;
        let t = inventory_of(conn)?.subscribe(notify, ctx);
        registry().connections.get_mut(&conn).ok_or(BAD_CONNECTION)?.subscriptions.push(t);
        out(token, t)
    })
}

extern "C" fn unsubscribe(conn: Conn, token: u64) -> i32 {
    guarded(|| {
        let inventory = inventory_of(conn)?;
        {
            let mut reg = registry();
            let c = reg.connections.get_mut(&conn).ok_or(BAD_CONNECTION)?;
            let i = c.subscriptions.iter().position(|t| *t == token).ok_or(NOT_FOUND)?;
            c.subscriptions.remove(i);
        }
        inventory.unsubscribe(token);
        Ok(())
    })
}

/// Changes after `after_version`, in order (each carries the version it leads to: pin it to read that state).
extern "C" fn changes(conn: Conn, after_version: u64, sink: Option<ChangeSink>, ctx: *mut c_void) -> i32 {
    guarded(|| {
        let sink = sink.ok_or(BAD_ARGUMENT)?;
        let list = inventory_of(conn)?.changes_since(after_version).map_err(|_| RESET_NEEDED)?;
        for (change, _) in &list {
            sink(ctx, change);
        }
        Ok(())
    })
}

extern "C" fn node(conn: Conn, version: u64, id: NodeId, info: *mut NodeInfo) -> i32 {
    guarded(|| {
        let s = snapshot(conn, version)?;
        if id != crate::inventory::ROOT && s.resolve(id).is_none() {
            return Err(NOT_FOUND);
        }
        let parent = s.parent(id).unwrap_or(u64::MAX);
        let row = s.row_in_parent(id).unwrap_or(0);
        out(info, NodeInfo { parent, row, child_count: s.child_count(id) })
    })
}

extern "C" fn child(conn: Conn, version: u64, id: NodeId, i: u32, result: *mut NodeId) -> i32 {
    guarded(|| out(result, snapshot(conn, version)?.child(id, i).ok_or(NOT_FOUND)?))
}

/// Writes up to `capacity` children into `buffer` and their total count into `count` (BUFFER_TOO_SMALL if it's more).
extern "C" fn children(conn: Conn, version: u64, id: NodeId, buffer: *mut NodeId, capacity: u32, count: *mut u32)
    -> i32 {
    guarded(|| {
        let s = snapshot(conn, version)?;
        if id != crate::inventory::ROOT && s.resolve(id).is_none() {
            return Err(NOT_FOUND);
        }
        let n = s.child_count(id);
        out(count, n)?;
        if capacity > 0 && buffer.is_null() {
            return Err(BAD_ARGUMENT); // a null buffer only asks for the count (capacity 0)
        }
        for i in 0..n.min(capacity) {
            unsafe { buffer.add(i as usize).write(s.child(id, i).ok_or(INTERNAL)?) };
        }
        if n > capacity { Err(BUFFER_TOO_SMALL) } else { Ok(()) }
    })
}

extern "C" fn find_child(conn: Conn, version: u64, id: NodeId, key: *const c_char, len: usize, result: *mut NodeId)
    -> i32 {
    guarded(|| out(result, snapshot(conn, version)?.find_child(id, input(key, len)?).ok_or(NOT_FOUND)?))
}

/// One text of a node, copied through `sink`: its key, name, kind, provider, uid, display name (SciQLop's rule) or
/// tooltip (SciQLop's HTML).
extern "C" fn text(conn: Conn, version: u64, id: NodeId, which: u32, sink: Option<TextSink>, ctx: *mut c_void) -> i32 {
    guarded(|| {
        let sink = sink.ok_or(BAD_ARGUMENT)?;
        let s = snapshot(conn, version)?;
        if which == TEXT_KEY {
            send_text(sink, ctx, s.key(id));
            return Ok(());
        }
        if which == TEXT_DISPLAY {
            send_text(sink, ctx, &s.display(id));
            return Ok(());
        }
        let (st, row) = s.at(id).ok_or(NOT_FOUND)?;
        match which {
            TEXT_NAME => send_text(sink, ctx, st.name(row)),
            TEXT_KIND => send_text(sink, ctx, st.kind(row)),
            TEXT_PROVIDER => send_text(sink, ctx, st.provider(row)),
            TEXT_UID => send_text(sink, ctx, st.uid(row)),
            TEXT_TOOLTIP => {
                let mut html = format!("<h3>{}</h3>", s.display(id));
                for (k, v) in st.raw_meta(row) {
                    html += &format!("<br/><b>{}:</b> {}", st.s.str(k), meta_text(st, v));
                }
                send_text(sink, ctx, &html);
            }
            _ => return Err(BAD_ARGUMENT),
        }
        Ok(())
    })
}

extern "C" fn meta(conn: Conn, version: u64, id: NodeId, key: *const c_char, len: usize, sink: Option<MetaSink>,
                   ctx: *mut c_void) -> i32 {
    guarded(|| {
        let sink = sink.ok_or(BAD_ARGUMENT)?;
        let s = snapshot(conn, version)?;
        let (st, row) = s.at(id).ok_or(NOT_FOUND)?;
        let k = st.s.get(input(key, len)?).ok_or(NOT_FOUND)?;
        let (_, v) = st.raw_meta(row).find(|(kk, _)| *kk == k).ok_or(NOT_FOUND)?;
        let value = value_of(st, v);
        sink(ctx, key, len, &value);
        Ok(())
    })
}

extern "C" fn meta_all(conn: Conn, version: u64, id: NodeId, sink: Option<MetaSink>, ctx: *mut c_void) -> i32 {
    guarded(|| {
        let sink = sink.ok_or(BAD_ARGUMENT)?;
        let s = snapshot(conn, version)?;
        let (st, row) = s.at(id).ok_or(NOT_FOUND)?;
        for (k, v) in st.raw_meta(row) {
            let key = st.s.str(k);
            sink(ctx, key.as_ptr() as *const c_char, key.len(), &value_of(st, v));
        }
        Ok(())
    })
}

fn send_hits<S: Copy + Into<f64>>(hits: &[(NodeId, S)], sink: HitsSink, ctx: *mut c_void) {
    let nodes: Vec<NodeId> = hits.iter().map(|h| h.0).collect();
    let scores: Vec<f64> = hits.iter().map(|h| h.1.into()).collect();
    sink(ctx, nodes.as_ptr(), scores.as_ptr(), hits.len() as u32);
}

extern "C" fn search(conn: Conn, version: u64, query: *const c_char, len: usize, sink: Option<HitsSink>, ctx: *mut c_void)
    -> i32 {
    guarded(|| {
        let sink = sink.ok_or(BAD_ARGUMENT)?;
        let s = snapshot(conn, version)?;
        send_hits(&s.search(input(query, len)?), sink, ctx);
        Ok(())
    })
}

extern "C" fn bm25(conn: Conn, version: u64, text: *const c_char, len: usize, sink: Option<HitsSink>, ctx: *mut c_void) -> i32 {
    guarded(|| {
        let sink = sink.ok_or(BAD_ARGUMENT)?;
        let s = snapshot(conn, version)?;
        send_hits(&s.bm25(input(text, len)?), sink, ctx);
        Ok(())
    })
}
