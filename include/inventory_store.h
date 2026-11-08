/* The C interface of speasy's inventory (src/capi.rs), version 2.
 *
 * The inventory grows while it is used: speasy starts providers lazily, lazy nodes (HAPI servers, datasets) fill in
 * on first access, providers get refreshed. It is one store made of immutable segments; readers use snapshots.
 *
 * Getting it: `inventory.__inventory_store_c_api__()` returns a PyCapsule named INVENTORY_STORE_CAPSULE_NAME whose
 * pointer is a `const InventoryApi*`:
 *
 *     const InventoryApi* api = PyCapsule_GetPointer(capsule, INVENTORY_STORE_CAPSULE_NAME);
 *     InventoryApi mine = *api;               // copy the table
 *     mine.retain(mine.handle);               // keep the inventory past the capsule's life
 *     InvSnapshot s = mine.snapshot(mine.handle);   // any thread, no GIL: a snapshot never changes
 *     ... mine.child_count(s, INVENTORY_ROOT) ...
 *     mine.snapshot_release(s);
 *     mine.release(mine.handle);
 *
 * Following changes (e.g. a Qt model): `subscribe` registers a callback called after each change, on the thread that
 * made it (only post an event there). Then, on your own thread, `changes_since(last_seq)` replays each change in
 * order with the snapshot it leads to: emit the begin signal, switch to that snapshot, emit the end signal.
 * A change is: child rows [position, position + count) of `host` appeared (INSERTED) or disappeared (REMOVED), or
 * `host`'s own data changed (DATA). A lazy node keeps its id when it fills in.
 *
 * Check `abi_version == INVENTORY_STORE_ABI_VERSION` and `struct_size >= sizeof(InventoryApi)`: later versions only
 * append members. Node ids are uint64 (segment << 32 | row), INVENTORY_ROOT is 0, INVENTORY_NO_NODE means none.
 * Strings are borrowed UTF-8 (not NUL-terminated), valid while you hold the snapshot they come from.
 */
#pragma once
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define INVENTORY_STORE_ABI_VERSION 2u
#define INVENTORY_STORE_CAPSULE_NAME "speasy.inventory_store.v2"
#define INVENTORY_ROOT 0ull
#define INVENTORY_NO_NODE 0xFFFFFFFFFFFFFFFFull
#define INVENTORY_NONE 0xFFFFFFFFu

typedef uint64_t InvNode;
typedef const void* InvHandle;
typedef const void* InvSnapshot;

typedef struct
{
    const char* ptr;
    size_t len;
} InvStr;

enum InvField
{
    INV_FIELD_KEY = 0, /* the Python attribute under the parent, e.g. "AC_H0_MFI" */
    INV_FIELD_NAME = 1,
    INV_FIELD_KIND = 2, /* the speasy index class name, e.g. "ParameterIndex" */
    INV_FIELD_PROVIDER = 3,
    INV_FIELD_UID = 4
};

enum InvValueKind
{
    INV_STR = 1,
    INV_INT = 2,
    INV_FLOAT = 3,
    INV_BOOL = 4,
    INV_NULL = 5,
    INV_LIST = 6, /* composites: use meta_text for a JSON rendering */
    INV_TUPLE = 7,
    INV_DICT = 8,
    INV_OPAQUE = 9 /* a Python object with no C form: text is its str() */
};

typedef struct
{
    uint32_t kind;
    int64_t int_value; /* INV_INT, INV_BOOL */
    double float_value; /* INV_FLOAT */
    InvStr text; /* INV_STR, INV_OPAQUE */
} InvValue;

enum InvChangeKind
{
    INV_CHANGE_INSERTED = 1,
    INV_CHANGE_REMOVED = 2,
    INV_CHANGE_DATA = 3
};

typedef struct
{
    uint64_t seq; /* the snapshot's after this change */
    uint32_t kind;
    uint32_t position;
    uint32_t count;
    InvNode host;
} InvChange;

typedef void (*InvNotify)(void* ctx);
typedef void (*InvTextSink)(void* ctx, InvStr text);
typedef void (*InvHitSink)(void* ctx, InvNode node, double score);
/* `after` is borrowed for the call: snapshot_retain it to keep it */
typedef void (*InvChangeSink)(void* ctx, const InvChange* change, InvSnapshot after);

typedef struct
{
    uint32_t abi_version;
    uint32_t struct_size;
    InvHandle handle;
    void (*retain)(InvHandle);
    void (*release)(InvHandle);
    InvSnapshot (*snapshot)(InvHandle); /* the current one, retained for you */
    void (*snapshot_retain)(InvSnapshot);
    void (*snapshot_release)(InvSnapshot);
    uint64_t (*snapshot_seq)(InvSnapshot);
    /* `notify` runs on the thread that changed the inventory: only post an event there, never call back into the
       inventory. unsubscribe() waits for a notification in progress, so ctx may be freed right after it. */
    uint64_t (*subscribe)(InvHandle, InvNotify notify, void* ctx); /* returns a token */
    void (*unsubscribe)(InvHandle, uint64_t token);
    /* replays changes after `after_seq` in order; returns how many, or INVENTORY_NONE if the log no longer reaches
       back there: reset from snapshot() */
    uint32_t (*changes_since)(InvHandle, uint64_t after_seq, InvChangeSink sink, void* ctx);
    uint32_t (*child_count)(InvSnapshot, InvNode node);
    InvNode (*child)(InvSnapshot, InvNode node, uint32_t i);
    InvNode (*parent)(InvSnapshot, InvNode node);
    uint32_t (*row_in_parent)(InvSnapshot, InvNode node);
    InvNode (*find_child)(InvSnapshot, InvNode node, InvStr key);
    InvStr (*field)(InvSnapshot, InvNode node, uint32_t which);
    uint32_t (*meta_count)(InvSnapshot, InvNode node);
    InvStr (*meta_key)(InvSnapshot, InvNode node, uint32_t i);
    void (*meta_value)(InvSnapshot, InvNode node, uint32_t i, InvValue* out);
    uint32_t (*meta_find)(InvSnapshot, InvNode node, InvStr key); /* entry index or INVENTORY_NONE */
    void (*meta_text)(InvSnapshot, InvNode node, uint32_t i, InvTextSink sink, void* ctx);
    /* SciQLop's product search over the whole snapshot: query syntax + filters + fuzzy score, and BM25F with
       corpus-wide statistics. One sink call per hit; returns the hit count. Indexes build per segment on first use. */
    uint32_t (*search)(InvSnapshot, InvStr query, InvHitSink sink, void* ctx);
    uint32_t (*bm25)(InvSnapshot, InvStr text, InvHitSink sink, void* ctx);
} InventoryApi;

#ifdef __cplusplus
}
#endif
