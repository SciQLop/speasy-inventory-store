# Plugging the inventory store into Qt Model/View (SciQLop / SciQLopPlots)

Status: research and prototype, 2026-11-08. Code: `src/capi.rs`, `include/inventory_store.h`, `qt/`, `tests/test_capi.py`.

## Answer first

1. **Yes, SciQLopPlots can use the very store speasy holds: no copy, no second instance.**
   - speasy's Python store object hands out a C function table in a PyCapsule.
   - SciQLopPlots takes it once, keeps its own reference, and from then on reads the store from C++ without the GIL.
   - It links nothing from speasy: no shared library, no ABI coupling, no Rust in SciQLopPlots' build.
2. **A Qt model reads the store directly.** Each `QModelIndex` carries the store row id in `internalId()`, so there are no
   node objects.
   - The prototype `StoreModel` passes `QAbstractItemModelTester`.
   - It walks all 115,574 nodes in 12 ms with no wrong parent.
   - SciQLop's search runs through it in 174 ms.
   - Tooltips are built on demand, 1,000 in 2 ms.
3. **For SciQLopPlots' generic ProductsModel, mount the store as a subtree.** Start with lazy nodes, created only for the rows a view
   visits, and send the filter models and search to the store. Tagged indexes without node objects can come later if needed.

## 1. Sharing speasy's instance with C++

### The protocol (prototype: `src/capi.rs`, `include/inventory_store.h`)

```c
// Python: capsule = store.__inventory_store_c_api__()
const InventoryStoreApi* api = PyCapsule_GetPointer(capsule, "speasy.inventory_store.v1");
// check api->abi_version == 1 and api->struct_size >= sizeof(InventoryStoreApi)
InventoryStoreApi mine = *api;   // copy the table
mine.retain(mine.handle);        // our reference: the capsule, and even the Python object, may now go away
// ... any thread, no GIL: child_count / child / parent / row_in_parent / find_child / field /
//     meta_count / meta_key / meta_value (typed) / meta_find / meta_text / search / bm25 ...
mine.release(mine.handle);
```

**Lifetime is a plain reference count** on the shared store (an `Arc`):
- The capsule owns one reference and drops it when Python frees it.
- A consumer takes its own with `retain`.

Tested: a retained store keeps working after the Python object is deleted.

**Immutability is what makes GIL-free reads safe.**
- Once a store has been shared, the Python side refuses to modify it (tested).
- A refreshed inventory is a new store, with a new `generation()`.
- Opaque metadata values (Python objects with no C form) only reach C as text.

**Strings are borrowed UTF-8** (pointer + length), valid while the reference is held. `QString::fromUtf8` converts them only when
a view asks.

**Versioning:**
- the capsule name carries the major version;
- the table starts with `abi_version` and `struct_size`;
- later versions only append members. An old SciQLopPlots then still works with a newer speasy, and the reverse is detected.

### Why this, and not the alternatives

| option | verdict |
|---|---|
| PyCapsule with a versioned C table (NumPy's `_ARRAY_API`, CPython's `PyDateTime_CAPI`, Arrow's `__arrow_c_stream__` all do this) | **chosen**: no link-time dependency, one instance, works on Linux, macOS and Windows, language-neutral |
| SciQLopPlots links speasy's native library | two loaded copies or DLL search problems on Windows and macOS, version skew between the two packages, and a Rust/C++ build dependency in SciQLopPlots |
| Arrow C Data Interface for the columns | standard and zero-copy for flat columns, but navigation, typed heterogeneous metadata and search don't map onto it |
| C++ calls back into Python for every `data()` | the GIL on the GUI thread for every paint, and a risk of stalls with the Jupyter kernel's threads |
| Copy into SciQLopPlots (today) | the inventory exists twice; this is what we're removing |

### Distributing the header

`inventory_store.h` is one self-contained C header (about 100 lines). Two options:
- Ship it in speasy's wheel with a `get_include()` function, the NumPy pattern. SciQLopPlots' meson build would then ask
  Python for the path.
- Or vendor it into SciQLopPlots.

Since the ABI is versioned and only grows, vendoring is fine and keeps SciQLopPlots' build independent of speasy's.

### Getting the store from Python to C++

SciQLopPlots' Shiboken bindings already accept raw Python objects through custom primitive types (`SciQLopPyBuffer`,
`GetDataPyCallable` in `bindings.xml`). The same pattern fits:

```python
# SciQLop's speasy plugin
ProductsModel.instance().mount_store(["speasy", "cda"], spz.inventories.store("cda"))
```

The C++ side calls `__inventory_store_c_api__()` once under the GIL and retains the store. After that it never touches Python.

## 2. The Qt model (prototype: `qt/StoreModel.{hpp,cpp}`)

| QAbstractItemModel member | from the store |
|---|---|
| `index(r, c, parent)` | `child(parent_row, r)` → `createIndex(r, c, quintptr(row))` (verified: `createIndex(int, int, quintptr)`) |
| `parent(index)` | `parent(row)`, plus `row_in_parent(parent)` for the parent's `QModelIndex::row()`. That's O(log n): siblings are stored sorted |
| `rowCount` / `hasChildren` | `child_count(row)`: O(1), so no `canFetchMore`/`fetchMore` is needed for in-memory stores |
| `flags` | leaves: `ItemIsDragEnabled | ItemNeverHasChildren` |
| `data(DisplayRole)` | SciQLop's rule: `name` metadata unless it's "AMDA", else the attribute key |
| `data(ToolTipRole)` | built on demand from `meta_key` / `meta_text`, the same HTML SciQLop precomputes today for all 77k nodes |
| `data(UserRole)` | the key, SciQLop's identity role for path lookups |
| search | `search(query)` / `bm25(text)` → `(row, score)`; `index_of(row)` turns a hit into an index |

Measured from a C++ extension in the same interpreter as speasy, with the GIL released (`qt/module.cpp`):
- `QAbstractItemModelTester` passes in 25 ms. Its single warning (type id 4097, a Qt GUI type) also shows up on a stock
  `QStringListModel`, so it comes from running the tester without Qt GUI, not from the model.
- A full walk takes 12 ms for 115,574 indexes, `data(DisplayRole)` included, with `parent(index(r, 0, p)) == p` everywhere.
- Search: 174 ms for `mms1 fgm bgse` on 16 threads, 78,915 hits. A hit's `index_of(row)` points back at the same row.
- 1,000 tooltips built on demand take 2 ms.

**Threading:** a Qt model belongs to one thread (the GUI thread), and model signals must be emitted there. Reads need no lock,
because the store is immutable. Swapping in a refreshed store (`set_store`) must run on the model thread: speasy notifies from
wherever it refreshes, and the plugin queues the swap with `QMetaObject::invokeMethod(..., Qt::QueuedConnection)`.

## 3. Fitting it into SciQLopPlots' ProductsModel

ProductsModel is generic: plugins add their own `ProductsModelNode`s, and that must keep working. Today
`internalPointer()` is always a `ProductsModelNode*`. That's assumed by the model, by both filter models (which hash by node
pointer), by `ProductsModel.node(path)` (which returns the QObject Python reads `metadata()` from), and by the search snapshot.

### Option B, first: lazy store-backed nodes

A `StoreNode : ProductsModelNode` holds `(store, row)`.
- **Its children** are created when Qt first asks for them (`rowCount`/`index` on that node), and kept.
- **Its accessors read the store:** `metadata()`, `tooltip()`, `raw_text()` and `display_name()`. They become virtual, or branch on
  a store pointer.
- **Every existing code path keeps getting `ProductsModelNode*`:** `node(path)`, drag and drop, Python's `metadata("speasy_id")`, the plugins.
- **Objects exist only for what a user expands:** a few hundred, instead of 77k QObjects with QVariantMaps, tooltips and search texts built at startup.

What has to change at the same time are the **full-corpus walks**, which would otherwise create every node:
- `collect_all_leaves`, in both filter models;
- `corpus_snapshot`, used by SciQLop's smart search;
- the autocompletion walk in `ProductsView`.

For store-backed subtrees they ask the store instead:
- **the flat model** lists `search()` hits;
- **the tree filter model** accepts a row if it, or a descendant, is a hit. That's the hits plus their ancestors, O(hits × depth)
  to compute;
- **completion** asks for distinct values, a store query to add.

### Option A, later if needed: tagged indexes, no node objects

`internalId()` would carry either a node pointer (aligned, so the low bit is 0) or `(mount << 33) | (row << 1) | 1`.
- **Every model member branches on the tag.**
- **The filter models key their per-item state** by a small `ProductRef {node*, mount, row}` instead of `ProductsModelNode*`.
- **`node(path)`** returns a cached node created on demand.

This removes the last objects, but touches every place that casts `internalPointer()`. It also needs a 64-bit `quintptr`;
SciQLop's desktop targets have one.

**Recommendation:** B. It changes less, it removes the memory and startup cost that matter, and A remains possible
afterwards without changing speasy's side.

### Refresh and lazy segments

- **One store per provider,** each mounted as a subtree. A provider refresh in speasy gives a new store. Speasy has to expose a hook
  (for example `spz.inventories.on_update(callback)`); the plugin then swaps that mount: `beginResetModel` for the subtree, or
  remove rows + insert rows under the mount.
  - **To keep the expanded and selected state across a swap,** re-resolve it by key path. The paths are stable, and node pointers
    weren't preserved before either.
- **HAPI servers and datasets load on first access.** That maps onto Qt's own lazy loading:
  - the mount reports `canFetchMore` until it's loaded;
  - `fetchMore` asks speasy to load the server, off the GUI thread;
  - the new segment arrives with `beginInsertRows`/`endInsertRows`.

  This is the per-segment store from DESIGN.md.

### Search, scores and SciQLop's smart search

The store returns `(row, raw score)` for the fuzzy and BM25F signals. SciQLop keeps fusing them with the embeddings, which it
keys by `path_key` today; `speasy_id` or `(mount, row)` would be stable keys.
- **The BM25 build moves out of the subprocess:** the store builds it in 0.5 s, on first use.
- **Plugin and virtual-product nodes** aren't in speasy's store. SciQLop keeps its own small corpus for them, or a later version adds an
  ingest path.

## What the prototype doesn't cover

- **A Shiboken binding and SciQLopPlots' real ProductsModel:** the prototype model is standalone. That's the next step if this direction is kept.
- **Change notification from speasy** (the update hook) and multi-segment mounting.
- **A view on screen:** the tests use `QCoreApplication`, a model tester and a programmatic walk. Painting adds nothing on the store's side;
  `data()` is the same call.
- **Windows and macOS builds** of the capsule path (nothing platform-specific is involved, but it wasn't built there).
