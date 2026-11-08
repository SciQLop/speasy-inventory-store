"""RSS of one freshly built inventory, nothing else allocated before it. usage: clean_rss.py {tree,columnar,native} file"""
import ctypes, gc, json, sys
from bench import IMPLS, rss_mb

def main(name, path):
    text = open(path).read()
    gc.collect(); ctypes.CDLL("libc.so.6").malloc_trim(0)
    before = rss_mb()
    impl = IMPLS[name].from_json(text)
    gc.collect(); ctypes.CDLL("libc.so.6").malloc_trim(0)
    print(json.dumps({"rss_clean_mb": rss_mb() - before}))

main(*sys.argv[1:])
