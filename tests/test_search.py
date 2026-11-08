"""The store's search must give SciQLop's results: fuzzy scores and query parsing as SciQLopPlots computes them
(tests/parity_sciqlopplots.json, generated with SciQLopPlots on real leaf texts), BM25F as SciQLop's Python."""
import json
import math
import os

import pytest

import bm25_reference
from inventory_store import NativeStore
from inventory_store._native import parse_query, subsequence_score

HERE = os.path.dirname(__file__)
DATA = os.path.join(HERE, "..", "data", "inventory_v2.json")


@pytest.fixture(scope="module")
def reference():
    with open(os.path.join(HERE, "parity_sciqlopplots.json")) as f:
        return json.load(f)


@pytest.fixture(scope="module")
def store():
    s = NativeStore.from_json(open(DATA).read())
    s.build_search()
    return s


def test_fuzzy_scores_equal_sciqlopplots(reference):
    rows = reference["pairs"] + reference["full"]
    wrong = [(q, p, sp, subsequence_score(q, p)) for q, p, m, sp, sm in rows if subsequence_score(q, p) != sp]
    wrong += [(q, m[:60], sm, subsequence_score(q, m)) for q, p, m, sp, sm in rows if subsequence_score(q, m) != sm]
    assert not wrong, wrong[:5]


def test_query_parsing_equals_sciqlopplots(reference):
    for text, free, filters in reference["parse"]:
        assert parse_query(text) == (free, [tuple(f) for f in filters]), text


def test_free_text_needs_every_token_and_adds_scores(store):
    by_row = {row: (p, m) for row, p, m in store.corpus()}
    hits = dict(store.search("mms1 fgm bgse"))
    assert hits
    for row, score in list(hits.items())[:200]:
        p, m = by_row[row]
        assert score == sum(max(subsequence_score(t, p), subsequence_score(t, m)) for t in ("mms1", "fgm", "bgse"))


def test_filters(store):
    by_row = {row: (p, m) for row, p, m in store.corpus()}
    cda = store.search("provider:CDA")
    assert cda and all(store.provider(r) == "cda" for r, _ in cda)
    assert len(store.search("provider:cda")) == len(cda)
    late = store.search("provider:cda after:2024-06")
    assert late and all(store.get(r, "stop_date") >= "2024-06" for r, _ in late)
    early = store.search("provider:cda before:1990")
    assert early and all(store.get(r, "start_date") < "1991" for r, _ in early)
    units = store.search("UNITS:nt")
    assert units and all(str(store.get(r, "UNITS")).lower() == "nt" for r, _ in units)
    assert store.search("provider:cda §§§") == []  # a subsequence match finds most words in long metadata texts
    assert len(store.search("")) == len(by_row)


def test_bm25_equals_sciqlop(store):
    corpus = store.corpus()
    index = bm25_reference.build(corpus)
    for query in ("mms fgm b gse", "solar wind proton density", "ace magnetic field", "electron pitch angle", "zzz"):
        expected = bm25_reference.score(index, query)
        got = dict(store.bm25(query))
        assert got.keys() == expected.keys(), query
        assert all(math.isclose(got[k], v, rel_tol=1e-9) for k, v in expected.items()), query
