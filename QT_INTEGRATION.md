# Plugging the inventory store into Qt Model/View (SciQLop / SciQLopPlots)

Status: research and prototype, 2026-11-08. Code: `src/inventory.rs`, `src/capi.rs`, `include/inventory_store.h`,
`qt/`, `tests/test_inventory.py`, `tests/test_capi.py`, `bench/lazy_session.py`.

## Answer first

1. **SciQLopPlots can use the very inventory speasy holds: no copy, no second instance.**
   - speasy's `Inventory` object hands out a versioned C function table in a PyCapsule.
   - SciQLopPlots keeps its own reference and reads from C++ without the GIL.
   - It links nothing from speasy.
2. **The inventory grows while it's used, and the model follows it change by change.** speasy starts providers
   lazily, HAPI fills servers and datasets on first access, and refreshes replace a provider.
   - The inventory is one store made of immutable segments.
   - Every change is logged with the snapshot it leads to.
   - The Qt model replays each one with the right begin/end row signals, so untouched parts keep their indexes,
     expansion and selection.
3. **The payoff is in SciQLop:** its main process holds about 1.9 GB today for the inventory, the product tree copy, the
   filter models and the BM25 index. The store would hold about 140 MB (section 0).
4. **Checked in a live speasy session** (`bench/lazy_session.py`) with `QAbstractItemModelTester` watching every signal:
   - CDA, AMDA and HAPI start on first use;
   - 3 HAPI servers and 3 datasets load lazily;
   - CDA is refreshed;
   - no tester failure throughout, pinned indexes outside the changed parts stay valid, and search finds the new
     products.
5. **A database-like C interface is the better boundary** (section 3b). C++ holds only a connection number and version
   numbers, results are copied out, and hostile input gets a status instead of a crash. The cost doesn't show in a view.
6. **The performance holds:** the segmented inventory searches exactly like one store (same scores, same speed). The Qt
   walk covers all 115,574 nodes in 19 ms.

## 0. What SciQLop costs today (`bench/sciqlop_today.py`)

This replays SciQLop's startup path with Qt offscreen, using SciQLop's own environment: speasy 1.8.5, which has no HAPI
provider, and PySide6/Qt 6.11.
- the speasy plugin's `init_providers()`, then `build_product_tree` and `ProductsModel.add_node`;
- a search through the product view's flat and tree filter models;
- the smart search's corpus snapshot and BM25F build.

It runs in one process, and RSS growth is measured per step. 77,001 leaves.

| step | time | RSS | today, where it lives | with the inventory store |
|---|---|---|---|---|
| speasy inventories (import + init) | 3.3 s | +446 MB | main process | 66 MB store; tree views on top |
| product tree copy (77k QObjects + QVariantMap + tooltip + search text) | 3.3 s | +928 MB | main process | no copy: the model reads the store (walk of 115k nodes in 19 ms) |
| flat filter model, one query until settled | 1.1 s | +148 MB | per filter model (SciQLop has 5) | the store's search: 140 ms, results only |
| tree filter model, same query | 0.6 s | +72 MB | product view | the store's search plus the hits' ancestors |
| smart search corpus snapshot | 0.6 s | +156 MB | main process, at every reindex | not needed: the index reads the store |
| BM25F build | 4.0 s | +866 MB peak | subprocess, at every model change | 0.28 s, about 75 MB, in-process, once per segment |
| BM25F index kept (after the pickle transfer: 43 MB pickle, 1.3 s) | — | +345 MB | main process | included above |
| BM25F query | 24 ms | — | | 1.5–2.6 ms |
| embeddings, if enabled | — | 75 / 150 MB | main process | unchanged (stays in SciQLop) |

**Main process in steady state: about 1.9 GB** (speasy 446 + tree copy 928 + two filter models 220 + BM25 345),
before embeddings. With the store it's about 140 MB for the same functions. Startup also drops from about 6.6 s
(speasy + tree copy) to speasy's own load, and the 4 s BM25 rebuild disappears.

**Caveats:**
- These are RSS deltas within one process: a step may reuse memory an earlier one freed, so they're approximations.
- speasy 1.9 loads providers lazily and adds HAPI. In the views session above, its inventories took 257 MB.
- The two memory figures don't count exactly the same things: the store side leaves out the Python views of a running speasy.



speasy's inventory never exists all at once. From the code:
- **Providers start on first access.** `speasy/inventories/__init__.py` routes `tree.<provider>` and
  `flat_inventories.<provider>` to `_ensure_provider`, and so does `spz.<provider>`.
- **Each provider refreshes on its own** (`update_inventory`).
- **HAPI's servers and datasets are lazy nodes** (`_LazyIndex`): childless until first access. `load_into` then
  replaces the node's content (metadata, the loaded flag, children).

So the inventory (`src/inventory.rs`) is one logical store whose pieces are immutable segments (each one a `Store`):
- **attached under the root, by key:** a provider's tree, when it starts. Attaching the same key again replaces it;
- **filling a childless node:** a lazy node loading. The node keeps its id, and its content comes from the filling
  segment's root.

Node ids are `segment << 32 | row`, and the root is 0.
- **Readers take a snapshot:** an `Arc`, immutable, read with no lock and no GIL.
- **A change builds a new snapshot** (the segment table is small and cloned; segments are shared) and logs it:
  `INSERTED`/`REMOVED` child rows `[position, position + count)` under a host, or `DATA` (the host's own data). A replaced
  provider logs `REMOVED` then `INSERTED` at the same place, each step with its own snapshot.
- **Search runs over every segment.** Each segment's index is built on first use, with its place in the tree, so the
  path texts include the ancestors above it.
  - Fuzzy results are a plain union.
  - BM25F sums document counts, lengths and term frequencies across segments, so its scores equal one index built over
    everything, which is what SciQLop does today. Tested equal to a single store, `tests/test_inventory.py`.

Tested (`tests/test_inventory.py`, `tests/test_capi.py`):
- the same tree as one store;
- parents and rows consistent;
- fuzzy and BM25F equal to one store;
- lazy nodes fill in place, a fill's text inherits its ancestors;
- refusing to fill what isn't a childless lazy node;
- replacing a provider drops its fills and keeps its place;
- the change log, step by step;
- old snapshots never change;
- one notification per change;
- handles outlive the Python objects.

## 2. Sharing speasy's instance with C++

```c
const InventoryApi* api = PyCapsule_GetPointer(inventory.__inventory_store_c_api__(), "speasy.inventory_store.v2");
InventoryApi mine = *api;  mine.retain(mine.handle);       // keep it past the capsule
InvSnapshot s = mine.snapshot(mine.handle);                // read s from any thread, no GIL
token = mine.subscribe(mine.handle, notify, ctx);          // notify() runs on the changing thread: post an event
mine.changes_since(mine.handle, last_seq, sink, ctx);      // on your thread: replay each change with its snapshot
```

- **Lifetime is plain reference counting:** the inventory and the snapshots are `Arc`s.
- **`unsubscribe` waits for a notification in progress,** so a consumer can be destroyed right after it.
- **Strings are borrowed UTF-8** and live as long as the snapshot.
- **Versioning:** the capsule name carries the major version; `abi_version` and `struct_size` let later versions only append.
- **The table is one plain C header** (`include/inventory_store.h`). SciQLopPlots can vendor it: the ABI is versioned and only
  grows.

Why a PyCapsule: it's the pattern NumPy (`_ARRAY_API`), CPython (`PyDateTime_CAPI`) and Arrow (`__arrow_c_stream__`)
use to share native objects between extensions.
- **There's no link-time dependency,** so there are no two copies of a library and no DLL search issues on Windows or macOS.
- **Neither side's toolchain leaks** into the other's build.

The alternatives:
- linking speasy's library from SciQLopPlots;
- the Arrow C Data Interface (navigation, typed heterogeneous metadata and search don't map onto it);
- calling Python from each `data()` (the GIL on the GUI thread);
- copying, which is what happens today.

All four are worse.

From Python to C++: SciQLopPlots' Shiboken bindings already take raw Python objects through custom primitive types
(`SciQLopPyBuffer`, `GetDataPyCallable` in `bindings.xml`). Something like
`ProductsModel.instance().mount_inventory(["speasy"], spz.inventories.store())` would follow that pattern.

## 3. The Qt model (prototype: `qt/StoreModel.{hpp,cpp}`)

| QAbstractItemModel | from the inventory snapshot |
|---|---|
| `index(r, c, parent)` | `child(parent_node, r)` → `createIndex(r, c, quintptr(node))` |
| `parent(index)` | `parent(node)`, plus `row_in_parent(parent)` (O(log n): siblings are sorted) |
| `rowCount` / `hasChildren` | `child_count(node)`, O(1) |
| `data(DisplayRole)` | SciQLop's rule: `name` metadata unless it's "AMDA", else the key |
| `data(ToolTipRole)` | built on demand (1,000 in 2 ms), where SciQLop precomputes 77k today |
| `flags` | drag-enabled for parameter kinds; leaves are recognized by kind, since a lazy node has no children until it loads |
| changes | `subscribe` → a queued `sync()` → `changes_since` → `beginInsertRows`/`beginRemoveRows` on the old snapshot, switch, `end…` |

**Threading:**
- the notification only queues a sync on the model's thread;
- reads need no lock;
- if the model falls behind the log (4096 changes), it resets.

### The live session (`bench/lazy_session.py`)

Run in the speasy worktree with `SPEASY_STORE_VIEWS=1`. The model is attached before anything is loaded, and
`QAbstractItemModelTester` watches it.

| step | time | model |
|---|---|---|
| attach, nothing loaded | — | 0 top rows |
| `spz.inventories.tree.cda` | 2.0 s | 1 row inserted; walk of 73,585 nodes in 10 ms |
| `spz.inventories.tree.amda` | 0.8 s | 1 row inserted; the CDA pin stays valid |
| `spz.inventories.tree.hapi` | 0.0 s | 1 row inserted (servers, unloaded) |
| `dir(tree.hapi.intermagnet)` loads it | 0.6 s | 154 children inserted under the server; DATA on the server |
| `list(<intermagnet dataset>)` loads it | 0.03 s | 3 parameters inserted under the dataset |
| lisird, then das2, and one dataset each | 0.5–1.9 s | the same: rows inserted in place |
| `spz.cda.update_inventory()` | 2.0 s | CDA removed, then inserted at the same row; the AMDA pin stays valid |

Throughout, the tester reported only its warning about type id 4097. It comes from running without Qt GUI: a stock
`QStringListModel` shows it too in a fresh process. The final walk covers 92,559 nodes in 13 ms.

**What a refresh does to the view:** the refreshed provider's subtree is replaced, so what was expanded *inside* it
collapses. Other providers keep everything. Today SciQLop never rebuilds its tree, so a refresh never reaches it at
all. Two ways to keep expansion across a refresh:
- SciQLop re-expands by key path after the change, since key paths are stable;
- the inventory diffs the new tree against the old one by key path and logs finer changes. That's more work, and not
  done here.

### Performance on the full inventory (`bench/inventory_bench.py`, 16 threads)

| | one store | segmented inventory |
|---|---|---|
| fuzzy `mms1 fgm bgse` | 171 ms | 140 ms |
| fuzzy `provider:cda after:2020 b_gse` | 40 ms | 37 ms |
| fuzzy `magnetic field vector` | 291 ms | 223 ms |
| BM25F queries | 2.1–2.4 ms | 1.5–2.6 ms |
| first search (builds the indexes) | 0.07 s* | 0.28 s |
| RSS, store + search index | 148 MB | 157 MB |
| Qt walk, 115,574 nodes | — | 19 ms |

\* the single store's index was built before this measurement, by `build_search()`.

**Memory: the search index used to cost more than it showed.** It held 77 MB but added 151 MB to the process: the
build's garbage (every leaf's term strings, postings grown by doubling) stayed scattered after being freed. It now
keeps terms as 64-bit hashes and builds one exactly-sized flat postings array in bounded chunks. It holds 52 MB, adds
about 72–80 MB, and builds in 0.28 s on 16 threads, against 0.5 s before. The store itself is unchanged at 66 MB.

## 3b. A database-like C interface instead (`src/capi_db.rs`, `include/inventory_db.h`, `qt/DbModel.*`)

**The question:** could SciQLop talk to the inventory like a database, so C++ holds no object of it?

**The prototype:**
- **One connection per consumer, as an integer in a registry on the Rust side.** Node ids and versions are integers too.
- **A connection pins the versions it reads.** Snapshots stay alive in the registry, not in the caller.
- **Every result is copied:** into the caller's buffer, or through a callback whose strings are valid during the call.
- **Every entry point checks its arguments, catches panics** (they become `INV_INTERNAL` instead of aborting SciQLop), and
  returns a status.
- **Display names and tooltips are computed on the store's side,** one call each.

It runs side by side with the handle-based interface, on the same data.

| | handles (StoreModel) | database (DbModel) |
|---|---|---|
| what C++ holds | inventory and snapshot pointers to retain/release; borrowed strings tied to a snapshot | one connection number, one pinned version number |
| a wrong or stale id, use after release/disconnect, invalid UTF-8, null pointer or callback | undefined behaviour (likely a crash) | a status code: tested on every entry point with random ids, unpinned versions, invalid UTF-8, null out-pointers and callbacks, too-small buffers, use after disconnect (`tests/test_capi_db.py`) |
| a panic in the store | aborts SciQLop | `INV_INTERNAL`, the call does nothing |
| Qt walk, 115,574 nodes | 13 ms | 34 ms |
| search through the model | 125 ms | 126 ms |
| 1,000 tooltips | 2.1 ms | 1.5 ms (one call each) |
| live lazy session (`bench/lazy_session.py db`) | passes the model tester | passes the model tester, identical signals, 0 errors |
| C ABI on the Rust side | 277 lines, 14 `unsafe` | 333 lines, 7 `unsafe` |
| the Qt model | 191 lines | 178 lines |

**The cost** is about 45 ns per call: a registry lock, a lookup and a reference count. A full walk of everything is 2.6× slower,
but a view only asks for its visible rows (tens at a time), so that's microseconds per screen. If it ever matters, a
per-thread cache of the pinned snapshot removes it.

**Verdict: worth it.**
- **It turns every misuse from the C++ side into an error code,** and keeps a bug in the store from taking SciQLop down.
- **Its cost isn't visible to a user.**
- **The code on both sides is no larger.**
- **There's nothing left to wrap** in a C++ RAII layer: there's one connection to close.
- **It's request/response,** so it would also work across a process boundary (speasy in a Jupyter kernel) if that's ever wanted.

## 4. Fitting it into SciQLopPlots' ProductsModel

ProductsModel is generic: plugins add `ProductsModelNode`s. Everything assumes `internalPointer()` is one: the model,
both filter models (they hash by node pointer), `ProductsModel.node(path)` (Python reads `metadata()` from it), and the
search snapshot.

**Option B, first: lazy store-backed nodes.** A `StoreNode : ProductsModelNode` holds `(inventory, node id)`.
- **Its children are created when Qt asks for them.** Its accessors (`metadata`, `tooltip`, `raw_text`, `display_name`) read
  the snapshot.
- **Existing code keeps getting `ProductsModelNode*`.** Objects exist only for what is expanded.
- **The inventory's change log drives the store-backed subtree:** insert or remove the StoreNodes the model has created.
- **The full-corpus walks have to ask the inventory instead,** otherwise they would create every node:
  - `collect_all_leaves`, in both filter models;
  - `corpus_snapshot`;
  - the autocompletion walk.

**Option A, later if needed: tagged indexes, no node objects.** `internalId` carries a node pointer (low bit 0) or a
tagged inventory node id.
- **Every model member branches on the tag.**
- **The filter models key their per-item state** by a small `ProductRef`.

This removes the last objects, but touches every cast of `internalPointer()`.

**Recommendation:** B, then A if profiling asks for it. speasy's side doesn't change either way.

**Search:**
- **The inventory returns `(node, raw score)`** for the fuzzy and BM25F signals. SciQLop keeps fusing them with its
  embeddings; the stable keys are `speasy_id`, or the key path.
- **Plugin and virtual-product nodes** aren't in speasy's inventory. SciQLop keeps a small corpus for them, or they get
  mounted as segments too.

## What this doesn't cover yet

- **The Shiboken binding and SciQLopPlots' real ProductsModel:** the prototype model is standalone.
- **A refresh that keeps expansion inside the refreshed provider** (re-expand by key path, or diffed changes).
- **Building the search indexes ahead of time:** the first search after new segments pays for their build, about 0.3 s
  for everything.
- **Builds on Windows and macOS.**
