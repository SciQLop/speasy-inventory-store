"""Test data, committed compressed: the real inventory (the speasy proxy's version 2 `all` inventory plus the HAPI
export, 2026-11-08) and fuzzy scores computed by SciQLopPlots on its leaf texts."""
import json
import lzma
import os

DATA = os.path.join(os.path.dirname(__file__), "data")


def inventory_text() -> str:
    with lzma.open(os.path.join(DATA, "inventory_v2.json.xz"), "rt", encoding="utf-8") as f:
        return f.read()


def inventory() -> dict:
    return json.loads(inventory_text())


def sciqlopplots_scores() -> list:
    with lzma.open(os.path.join(DATA, "parity_sciqlopplots.json.xz"), "rt", encoding="utf-8") as f:
        return json.load(f)
