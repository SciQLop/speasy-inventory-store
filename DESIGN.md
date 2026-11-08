# A native store for speasy's inventory — prototype and benchmarks

Status: prototype, 2026-11-08. No change to speasy.

## Answer first

1. **Memory comes from per-node Python objects and duplicated strings, not computation.** The inventory has
   1.5 M metadata strings but only 261 k distinct ones. Each of the 115 k nodes holds about 15 values in its own dict.
2. **Plain Python can't bring process memory down.** Every pure-Python layout shrinks the live objects (207 → 65 MB), but
   process RSS stays at 217–267 MB.
   - Whatever survives the build sits scattered among the parse's garbage, so Python can't return the freed memory.
   - That holds for interned strings, for one big string with offsets, and for interning the existing tree.
   - With the real value types (180 k lists, tuples, dicts, numbers), even the single-string layout keeps small objects alive.
3. **The Rust store does bring it down:** RSS of 66 MB against 271 MB today (4× less). On top of that:
   - builds are 2.6× faster and JSON export 3× faster;
   - filter queries are 50× faster;
   - lookups and walks are as fast as today;
   - its legacy pickle is the smallest (33 MB, against 64 MB today).
4. **Every value comes back with its exact type,** in Python and in the legacy pickle. Numbers that C++ can read are stored natively.
   Anything else (NumPy scalar, `datetime`, `set`, an int above 2⁶³) is kept as the Python object itself.

**Recommendation:**
1. Build the store in Rust, exposed to C++ through a PyCapsule holding a plain C function table.
2. Do the `__dict__` migration in the order below.
3. Interning strings in speasy now (step 0) halves the proxy's legacy pickles, but doesn't help worker memory.

## What speasy puts in metadata

Surveyed on each provider's tree as speasy builds it locally, without the proxy:

| provider | non-string values |
|---|---|
| amda | `bool` (`is_public`, `user_product`: 28 k), lists of tuples (`choices` of template arguments) |
| cda | `int` and `tuple` (`spz_shape`: 68 k), lists of floats or strings (`FILLVAL`, `LABL_PTR_*`: 89 k) |
| csa | `None` (121 k), `int`, `float`, `bool` |
| ssc | `None` |
| uiowaephtool | `float` |

There are no NumPy scalars and no dates. The archive provider was disabled here, so it isn't covered.

What clients receive from the proxy:
- **Version 1 JSON (the default)** turns every value into a string: `spz_shape` becomes `'(7,)'` and `is_public` becomes `'True'`.
- **Version 2 as a pickled dict**, which speasy clients ask for, keeps the types. Its JSON form turns tuples into lists.
- **It also contains bare `NaN`/`Infinity` tokens** in CDF fill values. Python's `json` writes them, but they aren't valid JSON,
  so non-Python clients can't parse that JSON.

## Benchmarks

Data:
- the proxy's `get_inventory?provider=all&version=2` (2026-10-27 build);
- plus HAPI exported from speasy with every server's catalogue loaded (11,180 datasets, 8,381 parameters; some servers list
  parameters only per dataset).

That's 115,575 nodes, 1.75 M metadata entries and 71 MB of JSON.

Method:
- Python 3.13, Linux x86-64.
- Each implementation runs in its own process, 3 times. Times are the best run, sizes the median.
- `bench/run.py data/inventory_v2.json 3` reproduces it.
- "RSS" is measured in a fresh process that only reads the file and builds. Subtracting RSS readings within one long run undercounts,
  because freed memory gets reused.

| | Python tree (today) | Python columnar, interned str | Python columnar, one str blob | Rust store |
|---|---|---|---|---|
| build from parsed JSON dict | 0.89 s | 0.68 s | 0.81 s | 0.34 s |
| build from JSON text | 1.38 s | 1.22 s | 1.36 s | 0.85 s |
| build through add() per node | — | 0.71 s | 0.81 s | 0.50 s |
| memory: Python objects + native heap estimate | 207 MB | 65 MB | 70 MB | 78 MB¹ |
| **memory: RSS of a process that only loads it** | **271 MB** | **223 MB** | **217 MB** | **66 MB** |
| JSON export | 0.92 s | 0.62 s | 0.89 s | 0.28 s |
| legacy tree rebuild | — | 0.27 s | 0.63 s | 0.26 s |
| legacy pickle (rebuild + protocols 3, 4, 5) | 1.43 s | 1.08 s | 1.47 s | 0.96 s |
| legacy tree, transient memory | — | 79 MB | 102 MB | 116 MB |
| pickle size (protocol 3) | 63.8 MB | 37.8 MB | 37.9 MB | 33.4 MB |
| uid lookup | 0.95 µs | 0.78 µs | 6.33 µs² | 0.76 µs |
| path lookup | 1.36 µs | 11.20 µs | 32.75 µs | 0.74 µs |
| full walk, children + meta dict per node | 0.14 s | 0.12 s | 0.54 s | 0.14 s |
| query: cda parameters in nT (3,435 hits) | 0.105 s | 0.030 s | 0.115 s | 0.002 s |

¹ an upper estimate from container capacities; the process RSS (66 MB) is lower.
² includes building its uid index on first use, spread over the 10k lookups.

Step 0, today's tree with interned strings: 118 MB of objects, 267 MB RSS, 33.3 MB pickle.

Reading the numbers:
- **The proxy's "hundreds of MB per worker"** fits: the tree alone is 271 MB of RSS, before the cached JSON and pickle variants.
- **The legacy pickle path costs about 1 s and about 120 MB per refresh.** It's a temporary peak, so run it once (in the refresher)
  and share the bytes, not once per worker.
- **The pickles shrink by half** because shared objects are written once (pickle memo). Old clients load them
  unchanged.

## Store layout (src/store.rs)

- **Strings:** an interner storing each distinct string once; its hash table holds ids, so no string is stored twice.
- **Nodes:** parallel `u32` columns: key (the Python attribute, which differs from the name for 13,596 nodes), name, kind,
  provider, uid, parent.
- **Metadata:** one flat run per node of (key id, value). A value is a string id (87% of them), or flagged as an index into a
  side list of typed values:
  - `Int`, `Float`, `Bool`, `Null`;
  - `List`, `Tuple` and `Dict`, kept apart, with nested strings interned too;
  - `Opaque`: the Python object itself, kept by the binding, with its text kept by the store for JSON and C readers.

  The Python binding accepts exact types only (`type(x) is int` and so on). So a NumPy scalar or an `str` subclass is opaque
  and comes back untouched.
- **Children:** CSR offsets, built once in `finish()`. A hash table on (provider, uid) serves lookups by uid.
- **JSON:** in and out in speasy's version 2 form. It accepts and writes Python's bare `NaN`/`Infinity` tokens, and parses floats exactly
  (serde_json's `float_roundtrip`; its default parser gets the last digit wrong on some CDF fill values).
- **Python API:** `from_json`, `from_dict`, `add(...)` + `finish()`, `children`, `meta`, `get`, `find_uid`, `find_path`,
  `filter`, `to_json`, `to_legacy(types, default)`.

Tests (`tests/test_equivalence.py`, 22 of them):
- each store gives back speasy's exact JSON, and a legacy tree equal to speasy's with exact types and NaN matching NaN;
- the same holds for the trees speasy builds locally for AMDA, CDA, CSA, SSC, 3DView and UiowaEphTool;
- every value type, opaque ones included, keeps its type.

Open design point: **lazily loaded subtrees.**
- HAPI fills servers and datasets on first access, and a refresh replaces a whole provider. A single CSR array can't be appended to.
- Proposal: immutable segments, one per provider (and one per HAPI server or loaded dataset), each a store of its own, linked from a parent row.
  A refresh swaps a segment, and readers keep the old one until they drop it.

## Migration path

0. **Now, no API change:** `sys.intern` keys and string values in `from_dict` and `make_inventory_node`. This halves the
   proxy's legacy pickles (64 → 33 MB), but doesn't lower worker RSS.
1. **Accessor API** (`children(node)`, `meta(node)`); move speasy's ~110 internal `__dict__` call sites onto it.
2. **Node classes become thin slotted views** (store ref + row id), materialized on first access and memoized. `isinstance`,
   attribute access and `__dir__` keep working.
3. **Compat shim:** `__dict__` as a property returning a read-only mapping built from the store. Reads keep working;
   writes (`node.__dict__[k] = ...`) are tree builders (providers, the proxy's `hapi_dumps`, SciQLop's patched `_register_nodes`),
   which move to the builder API.
4. **Native store behind the views.** `export_legacy_pickle(store)` stays outside the core and is deleted when old clients are gone.

## Views experiment: speasy's suite on store-backed trees

Branch `exp/store-views` in a speasy worktree (`~/Documents/prog/speasy-store-views`, local only).
- With `SPEASY_STORE_VIEWS=1`, `update_inventory` copies each provider's finished tree into a `NativeStore`, and puts the
  root's view in `spz.inventories.tree`.
- Views (`speasy/core/inventory/store_views.py`):
  - subclass the real index classes, with the same names;
  - one view per row, created once and memoized;
  - `__dict__` is a read-only mapping over the store;
  - attribute access and `dir()` read the store;
  - writes raise `TypeError`;
  - pickling gives the real class (via `copyreg`), so the unpickling side needs neither the store nor this module.

Results:
- **speasy's full suite: 1941 passed with views, 1941 without.**
- Before the fixes below, 28 tests failed with views, from two causes:
  1. **Exact-type checks on index objects:** `impex/__init__.py` (`to_index`, `find_parent_dataset`) and
     `hapi/inventory.py` (`_folder`). They now compare `spz_type()`. In the real migration, where the index classes
     themselves become the views, these checks would keep working unchanged.
  2. **A refresh that finds the inventory unchanged kept the old tree, and wrote the private AMDA part into it again.**
     A frozen tree is now swapped instead, and code holding old nodes keeps a consistent snapshot.
- **Smoke test** (navigation, `isinstance`, `vars`, `dir`, pickle, flat inventories, `get_data(view)`, a SciQLop-plugin-style
  walk): everything works.
  - The walk over CDA's 68k leaves takes 0.43 s, against 0.18 s with today's tree, because each attribute read goes through the store.
- **A real session loading every provider's inventory:** 174 MB RSS with views, 257 MB without. Startup is 2.3 s instead of 1.5 s.
  - The tree is still built in Python first (from the proxy's pickle) and copied into the store afterwards. The freed memory stays scattered, so
    the gain is a third, not the 4× of a store loaded directly.
  - Getting the rest means building the store straight from the proxy's version 2 JSON (`NativeStore.from_json`, 66 MB for everything
    including HAPI), with providers writing through `add()`.

Not covered:
- **HAPI.** It has its own `update_inventory` and fills servers and datasets on first access, so it isn't frozen. This
  needs the per-segment store.
- **SciQLop's plugin**, which wasn't run itself (it needs the Qt stack). Its access patterns were replayed instead.
- **The plugin's `_register_nodes` patch.** It wraps `ProviderInventory._register_nodes` and reads, but doesn't write.

## A. SciQLop's search, in the store (src/search.rs)

The store now runs SciQLop's product search over its leaves (85,637 parameters):
- **Query syntax:** a port of SciQLopPlots' `QueryParser`. Free-text tokens, `field:value`, quoted tokens, partial
  dates for `after:`/`before:`.
- **Filters:** a port of `ProductsFlatFilterModel::filters_match`:
  - `after:`/`before:` against `stop_date`/`start_date`;
  - `provider:`;
  - any other key as a case-insensitive exact match on the metadata value.
- **Fuzzy signal:** a port of `SubsequenceMatcher` and `free_text_score`. Every token must match the path or meta text,
  and the scores add up. It runs in parallel with rayon.
- **BM25F signal:** SciQLop's `bm25_index.py` (same tokenizer, k1, b and field weights), as an inverted index built in parallel.
- **Leaf texts:** they follow the speasy plugin (display names for the path; name + sorted scalar metadata + uid, provider,
  speasy_id, stable_id and instrument_description for meta). They aren't kept, which would cost 149 MB; each search regenerates them,
  at about 5 µs per leaf.

Parity (`tests/test_search.py`):
- 24,000 fuzzy scores on real leaf texts are equal to SciQLopPlots' (fixture generated with its Python binding);
- query parsing is equal on tricky inputs;
- BM25F scores are equal to SciQLop's Python on 5 queries (rel. 1e-9);
- the C++ finds exactly the same hits over the whole corpus.

| on 85,637 leaves | SciQLop today | store, 1 core | store, 16 threads |
|---|---|---|---|
| fuzzy, `mag` | 0.60 s (C++ via binding, 1 core) | 0.56 s | 0.07 s |
| fuzzy, `mms1 fgm bgse` | 1.88 s | 1.08 s | 0.14 s |
| fuzzy, `magnetic field vector` | 2.80 s | 1.73 s | 0.23 s |
| fuzzy, `provider:cda after:2020 b_gse` | — | 0.26 s | 0.04 s |
| BM25F index build | 3.3 s (Python, in a subprocess, on every model change) | 1.6 s | 0.5 s |
| BM25F index memory | 286 MB (Python dicts) | 77 MB | 77 MB |
| BM25F query | 13–17 ms | 1–2 ms | 1–2 ms |

Notes:
- The C++ times include converting Python strings to `QString` on each call. Inside SciQLopPlots, the texts are already
  `QString`s, and the pass runs in parallel batches of 2000.
- tantivy isn't needed: at this size a hand-written inverted index is small, fast, and gives SciQLop's exact scores.

**Still in SciQLop:**
- embeddings (model2vec: model, download, cache, cosine);
- fusing the signals (`score_query` bands, `merge_scores` strategies);
- the UI's progressive streaming;
- non-speasy rows (virtual products, scenes, plugin nodes). These need an ingest path into the store, or a second small corpus on the SciQLop side.

The store returns `(row, raw score)` per signal, and the existing merging takes it from there.

**Not ported:**
- `type:`, which needs SciQLop's ParameterType (`data_serie_type`). It should become a column computed at ingest.
- Qt's UTC-offset handling in date filters.

Possible next steps: delta-packing the postings (77 MB → ~25 MB), and an ASCII fast path in the matcher.

## B. SciQLop's product tree

Today SciQLop copies the speasy tree once, eagerly:
- one `ProductsModelNode` QObject per node, about 77k leaves;
- each with a QVariantMap of every scalar attribute, plus a precomputed tooltip and search text.

So the inventory exists twice. The copy's size wasn't measured.

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
- node fields: `name`, `kind`, `provider`;
- metadata: `meta_keys`, and `meta(row, key)` returning a typed value (string, int, float, bool, null, list, dict, or opaque text);
- search: `filter`, `search`, `distinct_values`;
- change tracking: `generation()` plus a change callback (to drive begin/end reset).

Row ids must be stable for a session, and keyed by `speasy_id` across sessions for the embedding cache.

## Rust or C++

**The consumer doesn't decide it.** The store lives in a Python extension that speasy loads. SciQLopPlots gets it through a
PyCapsule holding a versioned table of C function pointers, declared in one plain C header. SciQLopPlots then links nothing and
never sees the implementation language.

**For C++:**
- one native toolchain with CDFpp and pysciqlop_cache;
- the existing meson and wheel pipelines.

**For Rust:**
- serde, hashbrown, rayon and maturin abi3 wheels with little effort;
- the cocat rewrite went well.

The search turned out to need no library: tantivy isn't needed, so it no longer argues for Rust. The core is about 1,000 lines
either way, and C++ has `cpp_utils::threading` for the parallel pass.

**Recommendation:** it comes down to toolchains. Rust keeps this prototype as is. C++ keeps one native toolchain with CDFpp,
pysciqlop_cache and SciQLopPlots.

## Cost not paid yet: wheels

- **manylinux, macOS, Windows:** maturin abi3, a single wheel per platform for CPython 3.10 and later.
- **Free-threaded CPython:** abi3 doesn't cover it, so 3.13t and 3.14t need their own wheels. pyo3 supports them; opaque values
  then need care, since they're Python objects held by a native structure.
- **Pyodide:** pyo3 builds for the emscripten target, but it needs pinned toolchains. tantivy's in-RAM directory there wasn't
  checked.
- **The PyCapsule C header:** needs a version field and a rule that new functions are only ever appended.

## Files

- `src/`: Rust store (`interner.rs`, `store.rs`, `json.rs`, Python bindings in `lib.rs`).
- `python/inventory_store/`: the two pure-Python stores (`columnar.py`, `blob.py`).
- `tests/test_equivalence.py`: each store against speasy's trees.
- `tests/test_search.py`, `tests/parity_sciqlopplots.json`, `tests/bm25_reference.py`: the search against SciQLopPlots and SciQLop.
- `bench/search_bench.py`: search timings (`RAYON_NUM_THREADS=1` for one core).
- `bench/`: `bench.py` (one implementation per process), `clean_rss.py`, `run.py` (table).
- Data, not committed:
  - `data/inventory_v2.json`: the proxy's version 2 `all` inventory plus the HAPI export;
  - `data/live_trees.pkl`: each provider's tree built locally by speasy.
