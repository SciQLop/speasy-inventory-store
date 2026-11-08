"""A speasy session as a user lives it, with a Qt model attached from the start (run in the speasy worktree's venv):
providers start on first use, a HAPI server and dataset load on first access, a provider is refreshed. At each step:
the model's rows, QAbstractItemModelTester's verdict (it checks every insert/remove signal), pinned indexes, search.

usage: SPEASY_STORE_VIEWS=1 python lazy_session.py
"""
import os
import sys
import time

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "qt", "build"))
os.environ["SPEASY_STORE_VIEWS"] = "1"

import store_qt  # noqa: E402
import speasy as spz  # noqa: E402
from speasy.core.inventory.store_views import INVENTORY  # noqa: E402


def step(label, action=None):
    t = time.perf_counter()
    result = action() if action else None
    elapsed = time.perf_counter() - t
    events = store_qt.process_events()
    print(f"{label:46s} {elapsed:6.2f} s | top rows {events['top_rows']} | +{events['inserted']} -{events['removed']} "
          f"data {events['data_changed']} resets {events['resets']} | tester warnings {events['warnings']}")
    return result


def unloaded_dataset(server):
    """A dataset of a loaded server whose parameters aren't loaded yet, if the server lists them per dataset."""
    def walk(node):
        for v in node.__dict__.values():
            if hasattr(v, "spz_type") and v.spz_type() == "HapiDatasetIndex" and not v.__dict__.get("hapi_loaded"):
                return v
            if hasattr(v, "spz_type") and v.spz_type() == "SpeasyIndex":
                found = walk(v)
                if found is not None:
                    return found
        return None
    return walk(server)


store_qt.attach(INVENTORY)
step("model attached, nothing loaded")
step("spz.inventories.tree.cda", lambda: spz.inventories.tree.cda)
cda_pin = store_qt.pin(["cda", "ACE", "MAG", "AC_H0_MFI", "BGSEc"])
print("  walk:", store_qt.walk(), "| pinned cda BGSEc:", store_qt.pinned(cda_pin))
step("spz.inventories.tree.amda", lambda: spz.inventories.tree.amda)
amda_pin = store_qt.pin(["amda", "Parameters"])
print("  pinned cda BGSEc still:", store_qt.pinned(cda_pin), "| pinned amda:", store_qt.pinned(amda_pin))
step("spz.inventories.tree.hapi (servers, unloaded)", lambda: spz.inventories.tree.hapi)
for name in ("intermagnet", "lisird", "das2"):
    pin = store_qt.pin(["hapi", name])
    print(f"  {name} before load:", store_qt.pinned(pin))
    server = step(f"dir(tree.hapi.{name}) -> loads it", lambda: (dir(getattr(spz.inventories.tree.hapi, name)),
                                                                 getattr(spz.inventories.tree.hapi, name))[1])
    print(f"  {name} after load:", store_qt.pinned(pin), "| cda pin:", store_qt.pinned(cda_pin))
    dataset = unloaded_dataset(server)
    if dataset is not None:
        print(f"  a dataset to load lazily: {dataset.spz_uid()}")
        ds_keys = ["hapi", name] + dataset.spz_uid().split("/", 1)[1].split("/")
        ds_pin = store_qt.pin([k for k in ds_keys if k])
        step(f"list({dataset.spz_uid()}) -> loads it", lambda: list(dataset))
        print("  dataset after load:", store_qt.pinned(ds_pin) if ds_pin >= 0 else "not pinned (key path differs)",
              "| parameters via speasy:", [p.spz_name() for p in dataset][:3])
print("  search 'intermagnet':", store_qt.search("intermagnet"))
print("  search 'mms1 fgm bgse':", store_qt.search("mms1 fgm bgse"))
step("spz.cda.update_inventory() (refresh)", lambda: spz.cda.update_inventory())
print("  pinned cda BGSEc after refresh:", store_qt.pinned(cda_pin), "| amda:", store_qt.pinned(amda_pin))
print("  final walk:", store_qt.walk())
print("  control: tester warnings on a stock QStringListModel:", store_qt.control_warnings())
