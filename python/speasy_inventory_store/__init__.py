import os

from ._native import Inventory, NativeStore

__all__ = ["Inventory", "NativeStore", "get_include"]


def get_include() -> str:
    """The folder holding inventory_db.h, the C interface C/C++ consumers (SciQLopPlots) compile against."""
    return os.path.join(os.path.dirname(__file__), "include")
