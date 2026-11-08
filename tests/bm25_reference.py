"""SciQLop's BM25F (SciQLop/components/smart_search/bm25_index.py), reduced to what the comparison needs:
the reference the store's BM25F must match, and the baseline its build and query times are compared with."""
import math
import re
from collections import Counter, defaultdict

_TOKEN_RE = re.compile(r'[a-z0-9]+')
_K1, _B, _W_PATH, _W_META = 1.5, 0.75, 6.0, 1.0


def _tokenize(text):
    return _TOKEN_RE.findall(text.lower())


def _invert(field_tokens):
    inverted = defaultdict(dict)
    for doc_id, toks in enumerate(field_tokens):
        for term, count in Counter(toks).items():
            inverted[term][doc_id] = count
    return dict(inverted)


def build(entries):
    """entries: [(key, path_text, meta_text)]"""
    path_tokens = [_tokenize(p) for _, p, _ in entries]
    meta_tokens = [_tokenize(m) for _, _, m in entries]
    path_len, meta_len = [len(t) for t in path_tokens], [len(t) for t in meta_tokens]
    n = len(entries)
    return dict(keys=[k for k, _, _ in entries], path_inverted=_invert(path_tokens), meta_inverted=_invert(meta_tokens),
                path_len=path_len, meta_len=meta_len, path_avgdl=sum(path_len) / n if n else 1.0,
                meta_avgdl=sum(meta_len) / n if n else 1.0, doc_count=n)


def score(index, query):
    scores = defaultdict(float)
    for term in _tokenize(query):
        path_postings = index["path_inverted"].get(term, {})
        meta_postings = index["meta_inverted"].get(term, {})
        n = len(set(path_postings) | set(meta_postings))
        idf = math.log((index["doc_count"] - n + 0.5) / (n + 0.5) + 1)
        for doc_id in set(path_postings) | set(meta_postings):
            tf = _W_PATH * path_postings.get(doc_id, 0) + _W_META * meta_postings.get(doc_id, 0)
            doc_len = _W_PATH * index["path_len"][doc_id] + _W_META * index["meta_len"][doc_id]
            avgdl = _W_PATH * index["path_avgdl"] + _W_META * index["meta_avgdl"]
            scores[doc_id] += idf * (tf * (_K1 + 1)) / (tf + _K1 * (1 - _B + _B * doc_len / avgdl))
    return {index["keys"][doc_id]: s for doc_id, s in scores.items()}
