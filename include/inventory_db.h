/* speasy's inventory as a database (src/capi_db.rs), version 1. C/C++ holds no pointer into it.
 *
 *     const InventoryDbApi* api = PyCapsule_GetPointer(inventory.__inventory_db_c_api__(), INVENTORY_DB_CAPSULE_NAME);
 *     uint64_t conn; api->connect(api, &conn);      // once, while holding the capsule; then only integers
 *     uint64_t v; api->current_version(conn, &v); api->pin(conn, v);
 *     uint32_t n = 0; api->children(conn, v, INVENTORY_DB_ROOT, buffer, capacity, &n);
 *     api->text(conn, v, node, INV_TEXT_DISPLAY, sink, ctx);   // copied through sink, valid during the call
 *     api->unpin(conn, v); api->disconnect(conn);
 *
 * - Node ids, versions and connections are integers. A wrong or stale one gives a status, never a crash.
 * - Every function returns a status (INV_OK = 0) and writes results to out-parameters or sinks.
 * - Strings passed to sinks are valid during the sink call only: copy them (QString::fromUtf8 copies anyway).
 * - The inventory changes (lazy providers, lazy nodes, refreshes): `subscribe`'s notify runs on the changing thread
 *   (post an event, never call back); then `changes(after_version)` lists each change with the version it leads to:
 *   emit the begin signal, pin that version (and unpin the old), emit the end signal.
 * - Any thread may call; no GIL is needed. Strings in are UTF-8 (pointer + length).
 */
#pragma once
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define INVENTORY_DB_ABI_VERSION 1u
#define INVENTORY_DB_CAPSULE_NAME "speasy.inventory_db.v1"
#define INVENTORY_DB_ROOT 0ull
#define INVENTORY_DB_NO_NODE 0xFFFFFFFFFFFFFFFFull
#define INVENTORY_DB_CURRENT 0xFFFFFFFFFFFFFFFFull /* as a version: read the latest, unpinned */

enum InvStatus
{
    INV_OK = 0,
    INV_NOT_FOUND = 1,
    INV_BAD_CONNECTION = 2,
    INV_BAD_VERSION = 3, /* not pinned by this connection, or no longer available to pin */
    INV_BAD_ARGUMENT = 4,
    INV_BUFFER_TOO_SMALL = 5,
    INV_RESET_NEEDED = 6, /* the change log no longer reaches back: re-read from the current version */
    INV_INTERNAL = 7 /* a bug on the inventory's side, caught: the call did nothing */
};

enum InvText
{
    INV_TEXT_KEY = 0,
    INV_TEXT_NAME = 1,
    INV_TEXT_KIND = 2,
    INV_TEXT_PROVIDER = 3,
    INV_TEXT_UID = 4,
    INV_TEXT_DISPLAY = 5, /* SciQLop's rule: the "name" metadata unless it is "AMDA", else the key */
    INV_TEXT_TOOLTIP = 6 /* SciQLop's tooltip HTML */
};

enum InvValueKind
{
    INV_STR = 1,
    INV_INT = 2,
    INV_FLOAT = 3,
    INV_BOOL = 4,
    INV_NULL = 5,
    INV_LIST = 6,
    INV_TUPLE = 7,
    INV_DICT = 8,
    INV_OPAQUE = 9
};

typedef struct
{
    const char* ptr;
    size_t len;
} InvDbStr;

typedef struct
{
    uint32_t kind;
    int64_t int_value;
    double float_value;
    InvDbStr text; /* valid during the sink call */
} InvDbValue;

typedef struct
{
    uint64_t parent; /* INVENTORY_DB_NO_NODE for the root */
    uint32_t row; /* position among its siblings */
    uint32_t child_count;
} InvNodeInfo;

enum InvChangeKind
{
    INV_CHANGE_INSERTED = 1,
    INV_CHANGE_REMOVED = 2,
    INV_CHANGE_DATA = 3
};

typedef struct
{
    uint64_t version; /* the version after this change */
    uint32_t kind;
    uint32_t position;
    uint32_t count;
    uint64_t host;
} InvDbChange;

typedef void (*InvDbNotify)(void* ctx);
typedef void (*InvDbTextSink)(void* ctx, const char* text, size_t len);
typedef void (*InvDbMetaSink)(void* ctx, const char* key, size_t key_len, const InvDbValue* value);
typedef void (*InvDbHitsSink)(void* ctx, const uint64_t* nodes, const double* scores, uint32_t count);
typedef void (*InvDbChangeSink)(void* ctx, const InvDbChange* change);

typedef struct InventoryDbApi
{
    uint32_t abi_version;
    uint32_t struct_size;
    int32_t (*connect)(const struct InventoryDbApi* self, uint64_t* conn);
    int32_t (*disconnect)(uint64_t conn);
    int32_t (*current_version)(uint64_t conn, uint64_t* version);
    int32_t (*pin)(uint64_t conn, uint64_t version);
    int32_t (*unpin)(uint64_t conn, uint64_t version);
    int32_t (*subscribe)(uint64_t conn, InvDbNotify notify, void* ctx, uint64_t* token);
    int32_t (*unsubscribe)(uint64_t conn, uint64_t token);
    int32_t (*changes)(uint64_t conn, uint64_t after_version, InvDbChangeSink sink, void* ctx);
    int32_t (*node)(uint64_t conn, uint64_t version, uint64_t node, InvNodeInfo* info);
    int32_t (*child)(uint64_t conn, uint64_t version, uint64_t node, uint32_t i, uint64_t* child);
    int32_t (*children)(uint64_t conn, uint64_t version, uint64_t node, uint64_t* buffer, uint32_t capacity,
                        uint32_t* count);
    int32_t (*find_child)(uint64_t conn, uint64_t version, uint64_t node, const char* key, size_t len,
                          uint64_t* child);
    int32_t (*text)(uint64_t conn, uint64_t version, uint64_t node, uint32_t which, InvDbTextSink sink, void* ctx);
    int32_t (*meta)(uint64_t conn, uint64_t version, uint64_t node, const char* key, size_t len, InvDbMetaSink sink,
                    void* ctx);
    int32_t (*meta_all)(uint64_t conn, uint64_t version, uint64_t node, InvDbMetaSink sink, void* ctx);
    /* SciQLop's search over the whole version: one sink call with every hit */
    int32_t (*search)(uint64_t conn, uint64_t version, const char* query, size_t len, InvDbHitsSink sink, void* ctx);
    int32_t (*bm25)(uint64_t conn, uint64_t version, const char* text, size_t len, InvDbHitsSink sink, void* ctx);
    const void* reserved; /* used by connect() only */
} InventoryDbApi;

#ifdef __cplusplus
}
#endif
