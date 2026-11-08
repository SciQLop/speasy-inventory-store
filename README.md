# speasy-inventory-store

A native store for [speasy](https://github.com/SciQLop/speasy)'s inventory, written in Rust.

- **Compact:** the full inventory (115k products, 1.75M metadata values) takes about 66 MB, against about 270 MB as
  Python objects. Metadata keeps its exact Python types.
- **Searchable:** SciQLop's product search (fuzzy matching and BM25F) runs inside the store, on all cores.
- **Grows lazily:** providers and lazy nodes attach as speasy loads them; every change is logged, so views follow
  it row by row.
- **Readable from C/C++ without the GIL**, through a database-like C interface: one connection number, pinned
  versions, copied results, a status for every call. A wrong id gives an error code, never a crash.

## C/C++ consumers

The header ships in the package:

```python
import speasy_inventory_store
speasy_inventory_store.get_include()  # folder holding inventory_db.h
```

```c
const InventoryDbApi* api = PyCapsule_GetPointer(inventory.__inventory_db_c_api__(), INVENTORY_DB_CAPSULE_NAME);
uint64_t conn; api->connect(api, &conn);
```

See `inventory_db.h` for the full contract, and `qt/DbModel.*` for a Qt item model built on it.

## Design notes

`DESIGN.md` (store layout, benchmarks) and `QT_INTEGRATION.md` (Qt Model/View, lazy loading, the C interface).
