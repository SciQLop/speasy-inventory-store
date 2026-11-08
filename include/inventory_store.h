/* The C interface of speasy's inventory store (src/capi.rs), version 1.
 *
 * Getting it: call `store.__inventory_store_c_api__()` on the Python store object; it returns a PyCapsule named
 * INVENTORY_STORE_CAPSULE_NAME whose pointer is a `const InventoryStoreApi*`:
 *
 *     const InventoryStoreApi* api = PyCapsule_GetPointer(capsule, INVENTORY_STORE_CAPSULE_NAME);
 *     InventoryStoreApi mine = *api;          // copy the table
 *     mine.retain(mine.handle);               // keep the store past the capsule's life
 *     ...                                      // any thread, no GIL needed: the store is immutable
 *     mine.release(mine.handle);
 *
 * Check `abi_version == INVENTORY_STORE_ABI_VERSION` and `struct_size >= sizeof(InventoryStoreApi)`: later versions
 * only append members. Rows are uint32 ids, 0 is the root; INVENTORY_STORE_NONE means "no such row".
 * Strings are borrowed UTF-8 (not NUL-terminated), valid while you hold a reference on the store.
 */
#pragma once
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define INVENTORY_STORE_ABI_VERSION 1u
#define INVENTORY_STORE_CAPSULE_NAME "speasy.inventory_store.v1"
#define INVENTORY_STORE_NONE 0xFFFFFFFFu

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

typedef void (*InvTextSink)(void* ctx, InvStr text);
typedef void (*InvHitSink)(void* ctx, uint32_t row, double score);
typedef const void* InvHandle;

typedef struct
{
    uint32_t abi_version;
    uint32_t struct_size;
    InvHandle handle;
    void (*retain)(InvHandle);
    void (*release)(InvHandle);
    uint64_t (*generation)(InvHandle); /* identifies this snapshot: a refreshed inventory is a new store */
    uint32_t (*node_count)(InvHandle);
    uint32_t (*child_count)(InvHandle, uint32_t row);
    uint32_t (*child)(InvHandle, uint32_t row, uint32_t i);
    uint32_t (*parent)(InvHandle, uint32_t row);
    uint32_t (*row_in_parent)(InvHandle, uint32_t row);
    uint32_t (*find_child)(InvHandle, uint32_t row, InvStr key);
    InvStr (*field)(InvHandle, uint32_t row, uint32_t which);
    uint32_t (*meta_count)(InvHandle, uint32_t row);
    InvStr (*meta_key)(InvHandle, uint32_t row, uint32_t i);
    void (*meta_value)(InvHandle, uint32_t row, uint32_t i, InvValue* out);
    uint32_t (*meta_find)(InvHandle, uint32_t row, InvStr key); /* entry index or INVENTORY_STORE_NONE */
    void (*meta_text)(InvHandle, uint32_t row, uint32_t i, InvTextSink sink, void* ctx);
    /* SciQLop's product search: query syntax + filters + fuzzy score, and BM25F. One sink call per hit;
       returns the hit count. The index is built on first use (~0.5 s for 85k leaves). */
    uint32_t (*search)(InvHandle, InvStr query, InvHitSink sink, void* ctx);
    uint32_t (*bm25)(InvHandle, InvStr text, InvHitSink sink, void* ctx);
} InventoryStoreApi;

#ifdef __cplusplus
}
#endif
