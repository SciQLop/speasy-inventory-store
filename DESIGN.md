# A native store for speasy's inventory — prototype and benchmarks

Status: prototype, 2026-11-08. No change to speasy.

## Answer first

1. **Memory comes from per-node Python objects and duplicated strings, not computation.** The inventory has
   1.73 M metadata strings but only 261 k distinct ones, and each of the 115 k nodes holds about 15 of them in its own dict.
2. **Plain Python can get the memory back or keep access fast, not both.**
   - A columnar store with interned strings is small in live objects (37 MB), but its process RSS stays at 212 MB.
     Its strings are scattered among the parse garbage, so Python can't return the memory (226 arenas held, 92 MB in use).
   - Packing every string into one big `str` brings RSS down to 70 MB, but walks are 4× slower and queries no faster
     than today.
3. **The Rust store gets both.** RSS is 51 MB (5× less than today). Builds are 2× faster, JSON export 3× faster and
   queries 50× faster, while lookups and walks stay as fast as today.
4. **A cheap step works today without any of this.** Interning strings when speasy builds the tree cuts objects from
   192 to 103 MB, RSS from 254 to 197 MB and the legacy pickle from 64 to 31 MB. It needs no API change.

**Recommendation:**
1. Intern strings in speasy now (step 0 below).
2. Build the store in Rust, exposed to C++ through a PyCapsule holding a plain C function table.
3. Do the `__dict__` migration in the order sketched below.

## Benchmarks

Real data:
- `get_inventory?provider=all` from the proxy (2026-10-27 build);
- plus HAPI exported from speasy with every server's catalogue loaded (11,180 datasets, 8,381 parameters; some servers list
  parameters only per dataset).

That's 115,575 nodes, 1.75 M metadata entries and 71 MB of JSON.

Method:
- Python 3.13, Linux x86-64.
- Each implementation runs in its own process, 3 times. Times are the best run, sizes the median.
- `bench/run.py data/inventory.json 3` reproduces it.
- All four give back speasy's exact JSON and an equal legacy tree (`tests/`).

| | Python tree (today) | Python columnar, interned str | Python columnar, one str blob | Rust store |
|---|---|---|---|---|
| build from parsed JSON dict | 0.66 s | 0.73 s | 0.84 s | 0.34 s |
| build from JSON text | 1.02 s | 1.08 s | 1.18 s | 0.77 s |
| build through add() per node | — | 0.73 s | 0.87 s | 0.51 s |
| memory: Python objects + native heap | 192 MB | 37 MB | 42 MB | 48 MB |
| **memory: RSS of a process that only loads it** | **254 MB** | **212 MB** | **70 MB** | **51 MB** |
| JSON export | 0.76 s | 0.53 s | 0.82 s | 0.23 s |
| legacy tree rebuild | — | 0.23 s | 0.55 s | 0.19 s |
| legacy pickle (rebuild + protocols 3, 4, 5) | 1.64 s | 0.91 s | 1.20 s | 0.94 s |
| legacy tree, transient memory | — | 79 MB | 103 MB | 106 MB |
| pickle size (protocol 3) | 63.8 MB | 31.7 MB | 31.8 MB | 32.3 MB |
| uid lookup | 1.03 µs | 0.83 µs | 6.54 µs¹ | 0.83 µs |
| path lookup | 1.38 µs | 12.49 µs | 35.17 µs | 0.84 µs |
| full walk, children + meta dict per node | 0.15 s | 0.12 s | 0.56 s | 0.13 s |
| query: cda parameters in nT (3,435 hits) | 0.105 s | 0.030 s | 0.122 s | 0.002 s |

¹ includes building its uid index on first use, spread over the 10k lookups.

Today's tree with interned strings (step 0): 103 MB of objects, 197 MB RSS, 31.2 MB pickle.

Reading the numbers:
- **The proxy's "hundreds of MB per worker"** fits: the tree alone is 254 MB of RSS, before the cached JSON and pickle variants.
- **The legacy pickle path costs about 1 s and about 100 MB per refresh.** It's a temporary peak, so run it in one place (the refresher)
  and share the bytes, not once per worker.
- **The pickles shrink by half** because shared string objects are written once (pickle memo). Old clients load them
  unchanged; the tests check equality with speasy's own tree.

## Store layout (src/store.rs, ~200 lines)

- **Strings:** an interner that stores each distinct string once; a hash table of ids, so no string is stored twice.
- **Nodes:** parallel `u32` columns: key (the Python attribute, which differs from the name for 13,596 nodes), name, kind,
  provider, uid, parent.
- **Metadata:** one flat run per node of (key id, value). A value is a string id, or flagged as an index into a side list of
  JSON values (lists, dicts, bools, numbers: 1% of the values).
- **Children:** CSR offsets, built once in `finish()`. Lookup by uid uses a hash table on (provider, uid).
- **Python API:** `from_json`, `from_dict`, `add(...)` + `finish()`, `children`, `meta`, `get`, `find_uid`, `find_path`,
  `filter`, `to_json`, `to_legacy(types, default)`.

Open design point: **lazily loaded subtrees.**
- HAPI fills servers and datasets on first access, and a refresh replaces a whole provider. A single CSR array can't be appended to.
- Proposal: immutable segments, one per provider (and one per HAPI server or loaded dataset), each a store of its own, linked from a parent row.
  A refresh swaps a segment, and readers keep the old one until they drop it.

## Migration path

0. **Now, no API change:** `sys.intern` keys and string values in `from_dict` and `make_inventory_node`.
1. **Accessor API** (`children(node)`, `meta(node)`); move speasy's ~110 internal `__dict__` call sites onto it.
2. **Node classes become thin slotted views** (store ref + row id), materialized on first access and memoized. `isinstance`,
   attribute access and `__dir__` keep working.
3. **Compat shim:** `__dict__` as a property returning a read-only mapping built from the store. Reads keep working;
   writes (`node.__dict__[k] = ...`) are tree builders (providers, the proxy's `hapi_dumps`), which move to the builder API.
4. **Native store behind the views.** `export_legacy_pickle(store)` stays outside the core and is deleted when old clients are gone.

## A. Queries and search

Today SciQLop's smart search doesn't read speasy at all:
- It snapshots the ProductsModel leaves into one text blob each (path + every metadata `k: v`).
- It rebuilds a hand-rolled BM25F index (path weight 6, meta 1) in a subprocess on every model change, which takes about 2 s for 77k leaves.
- An optional model2vec embedding fallback runs on top of it.
- Field filters (`provider:`, `type:`, `after:`/`before:`, any `key:value`) are linear C++ scans in the filter models.

**Can move to the store:**
- **Predicate filters:** provider, kind, parameter type, time coverage overlap, mission or instrument as an ancestor test, and
  units once promoted to a column. A native scan answers in 2 ms where walking the tree takes 100 ms. With columns,
  these become bitmaps.
- **Full-text BM25:** tantivy with two fields (path, meta) and the same boosts. It replaces the Python inverted lists and the
  per-change rebuild, and can be updated incrementally.
- **Distinct values** for autocompletion.

**Must stay in SciQLop:**
- embeddings (model choice, download, the cache keyed by text, cosine similarity);
- how the search text is composed;
- score fusion (the 100/50 bands, Override merge);
- fuzzy subsequence scoring;
- non-speasy rows (virtual products, scenes, plugin nodes). These need either an ingest API for foreign rows, or SciQLop keeps
  searching the union.

The interface to keep stable is SciQLop's `path_key → score` dict.

Not prototyped: tantivy. The `filter` above is a plain scan.

## B. SciQLop's product tree

Today SciQLop copies the speasy tree once, eagerly:
- one `ProductsModelNode` QObject per node, about 77k leaves;
- each with a QVariantMap of every scalar attribute, plus a precomputed tooltip and search text.

So the inventory exists twice. The copy's size wasn't measured here, but it's likely larger than the speasy tree.

A store-backed subtree inside the generic ProductsModel can work, but four things assume real node objects today:
1. `internalPointer` is a `ProductsModelNode*`. The model needs a tagged index (pointer or store row), or small proxy nodes
   made on demand.
2. Both filter models and the search snapshot walk every leaf through `children_nodes()`. They have to query the store
   instead (`filter`, `search`, `leaves`).
3. `ProductsModel.node(path)` returns a QObject that Python reads `metadata(...)` from. It needs a materialized, cached node
   on demand.
4. Drag and drop already carries only paths, so it keeps working.

C functions the store must expose:
- navigation: `child_count`, `child`, `parent`, `row_in_parent`, `find_child`;
- node fields: `name`, `kind`, `provider`, `meta(row, key)`, `meta_keys`;
- search: `filter`, `search`, `distinct_values`;
- change tracking: `generation()` plus a change callback (to drive begin/end reset).

Row ids must be stable for a session, and keyed by `speasy_id` across sessions for the embedding cache.

## Rust or C++

**The consumer doesn't decide it.** The store lives in a Python extension that speasy loads. SciQLopPlots gets it through a
PyCapsule holding a versioned table of C function pointers, declared in one plain C header. SciQLopPlots then links nothing and
never sees the implementation language. This works the same whether the store is written in Rust or C++.

**For C++:**
- one native toolchain with CDFpp and pysciqlop_cache;
- the existing meson and wheel pipelines;
- no second set of build skills.

**For Rust:**
- tantivy for full-text search; C++ has no equivalent as easy to use (Xapian is GPL, Lucene++ is stale);
- serde, hashbrown and maturin abi3 wheels with little effort;
- the cocat rewrite went well;
- the core is about 450 lines either way.

**Recommendation:** Rust if search moves into the store (question A), which is where the biggest user-visible win is.
C++ if the store stays a plain container.

## Cost not paid yet: wheels

- **manylinux, macOS, Windows:** maturin abi3, a single wheel per platform for CPython 3.10 and later.
- **Free-threaded CPython:** abi3 doesn't cover it, so 3.13t and 3.14t need their own wheels. pyo3 supports them.
- **Pyodide:** pyo3 builds for the emscripten target, but it needs pinned toolchains. tantivy would need its in-RAM
  directory there, which wasn't checked.
- **The PyCapsule C header:** needs a version field and a rule that new functions are only ever appended.

## Files

- `src/`: Rust store (`interner.rs`, `store.rs`, `json.rs`, Python bindings in `lib.rs`).
- `python/inventory_store/`: the two pure-Python stores (`columnar.py`, `blob.py`).
- `tests/test_equivalence.py`: each store against speasy's tree on the real inventory.
- `bench/`: `bench.py` (one implementation per process), `clean_rss.py`, `run.py` (table).
- Data: `data/inventory.json` (not committed): the proxy's `all` inventory plus the HAPI export described above.
