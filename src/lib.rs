//! Python bindings. Row ids are plain ints; node 0 is the root.
mod interner;
mod json;
mod search;
mod store;

use json::{NodeRef, SPZ_KEYS};
use pyo3::exceptions::{PyIndexError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple};
use store::{Meta, NodeFields, OTHER, Store, Val, NONE};

/// Python values into the store: exact types only, anything else is kept as the object itself (opaque).
struct Ingest<'a> {
    store: &'a mut Store,
    opaque: &'a mut Vec<Py<PyAny>>,
}

impl Ingest<'_> {
    fn val(&mut self, v: &Bound<PyAny>) -> PyResult<Val> {
        Ok(if v.is_exact_instance_of::<PyString>() {
            Val::Str(self.store.s.intern(v.cast::<PyString>()?.to_str()?))
        } else if v.is_none() {
            Val::Null
        } else if v.is_exact_instance_of::<PyBool>() {
            Val::Bool(v.is_truthy()?)
        } else if v.is_exact_instance_of::<PyFloat>() {
            Val::Float(v.extract()?)
        } else if let Some(i) = v.is_exact_instance_of::<PyInt>().then(|| v.extract::<i64>().ok()).flatten() {
            Val::Int(i)
        } else if v.is_exact_instance_of::<PyList>() {
            Val::List(self.items(v)?)
        } else if v.is_exact_instance_of::<PyTuple>() {
            Val::Tuple(self.items(v)?)
        } else if let Some(d) = v.cast::<PyDict>().ok().filter(|d| is_plain_dict(d)) {
            let entries: PyResult<Vec<_>> = d
                .iter()
                .map(|(k, x)| Ok((self.store.s.intern(k.cast::<PyString>()?.to_str()?), self.val(&x)?)))
                .collect();
            Val::Dict(entries?)
        } else {
            self.opaque.push(v.clone().unbind());
            self.store.opaque(&v.str()?.to_string())
        })
    }

    fn items(&mut self, v: &Bound<PyAny>) -> PyResult<Vec<Val>> {
        v.try_iter()?.map(|x| self.val(&x?)).collect()
    }

    fn meta(&mut self, key: &str, v: &Bound<PyAny>) -> PyResult<()> {
        let value = self.val(v)?;
        self.store.push_meta(key, value);
        Ok(())
    }

    fn node(&mut self, parent: u32, key: &str, node: &Bound<PyDict>) -> PyResult<()> {
        let [name, kind, provider, uid] =
            ["__spz_name__", "__spz_type__", "__spz_provider__", "__spz_uid__"].map(|k| str_field(node, k));
        let fields = NodeFields { key, name: &name?, kind: &kind?, provider: &provider?, uid: &uid? };
        let id = self.store.start_node(parent, fields);
        let mut children = Vec::new();
        for (k, v) in node.iter() {
            let k = k.str()?.to_string();
            if SPZ_KEYS.contains(&k.as_str()) {
                continue;
            }
            match v.cast::<PyDict>() {
                Ok(d) if d.contains("__spz_type__")? => children.push((k, d.clone())),
                _ => self.meta(&k, &v)?,
            }
        }
        for (k, child) in children {
            self.node(id, &k, &child)?;
        }
        Ok(())
    }
}

fn is_plain_dict(d: &Bound<PyDict>) -> bool {
    d.is_exact_instance_of::<PyDict>() && d.keys().iter().all(|k| k.is_exact_instance_of::<PyString>())
}

fn str_field(d: &Bound<PyDict>, key: &str) -> PyResult<String> {
    Ok(match d.get_item(key)? {
        Some(v) => v.str()?.to_string(),
        None => String::new(),
    })
}

/// Store values out to Python. With `cache`, one Python string object per distinct string, so the legacy tree and
/// its pickle share them too.
struct Out<'py, 'a> {
    py: Python<'py>,
    store: &'a Store,
    opaque: &'a [Py<PyAny>],
    strings: Vec<Option<Bound<'py, PyString>>>,
}

impl<'py, 'a> Out<'py, 'a> {
    fn new(py: Python<'py>, store: &'a Store, opaque: &'a [Py<PyAny>], cache: bool) -> Self {
        Out { py, store, opaque, strings: if cache { vec![None; store.s.len()] } else { Vec::new() } }
    }

    fn s(&mut self, id: u32) -> Bound<'py, PyString> {
        let (py, store) = (self.py, self.store);
        match self.strings.get_mut(id as usize) {
            Some(slot) => slot.get_or_insert_with(|| PyString::new(py, store.s.str(id))).clone(),
            None => PyString::new(py, store.s.str(id)),
        }
    }

    fn val(&mut self, v: &Val) -> PyResult<Bound<'py, PyAny>> {
        let py = self.py;
        Ok(match v {
            Val::Str(id) => self.s(*id).into_any(),
            Val::Int(i) => i.into_pyobject(py)?.into_any(),
            Val::Float(f) => f.into_pyobject(py)?.into_any(),
            Val::Bool(b) => PyBool::new(py, *b).to_owned().into_any(),
            Val::Null => py.None().into_bound(py),
            Val::List(items) => PyList::new(py, self.items(items)?)?.into_any(),
            Val::Tuple(items) => PyTuple::new(py, self.items(items)?)?.into_any(),
            Val::Dict(entries) => {
                let d = PyDict::new(py);
                for (k, x) in entries {
                    d.set_item(self.s(*k), self.val(x)?)?;
                }
                d.into_any()
            }
            Val::Opaque(i) => self.opaque[*i as usize].bind(py).clone(),
        })
    }

    fn items(&mut self, items: &[Val]) -> PyResult<Vec<Bound<'py, PyAny>>> {
        items.iter().map(|x| self.val(x)).collect()
    }

    fn meta(&mut self, m: Meta) -> PyResult<Bound<'py, PyAny>> {
        match m {
            Meta::Str(s) => Ok(PyString::new(self.py, s).into_any()),
            Meta::Other(v) => self.val(v),
        }
    }

    fn legacy(&mut self, i: u32, types: &Bound<'py, PyDict>, default: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
        let st = self.store;
        let [_, name, kind, provider, uid] = st.raw(i);
        let cls = types.get_item(self.s(kind))?.unwrap_or_else(|| default.clone());
        let obj = cls.call_method1("__new__", (&cls,))?;
        let dict = obj.getattr("__dict__")?.cast_into::<PyDict>()?;
        for (k, v) in st.raw_meta(i) {
            let value = if v & OTHER != 0 { self.val(st.other(v))? } else { self.s(v).into_any() };
            dict.set_item(self.s(k), value)?;
        }
        for &c in st.children(i) {
            dict.set_item(self.s(st.raw(c)[0]), self.legacy(c, types, default)?)?;
        }
        for (k, v) in SPZ_KEYS.iter().zip([kind, name, provider, uid]) {
            dict.set_item(PyString::intern(self.py, k), self.s(v))?;
        }
        Ok(obj)
    }
}

#[pyclass(name = "NativeStore", module = "inventory_store")]
struct PyStore {
    inner: Store,
    opaque: Vec<Py<PyAny>>,
    search: Option<search::SearchIndex>,
}

impl PyStore {
    fn check(&self, i: u32) -> PyResult<u32> {
        if (i as usize) < self.inner.len() { Ok(i) } else { Err(PyIndexError::new_err(i)) }
    }

    fn index(&self) -> PyResult<&search::SearchIndex> {
        self.search.as_ref().ok_or_else(|| PyValueError::new_err("call build_search() first"))
    }

    fn ingest(&mut self) -> Ingest<'_> {
        Ingest { store: &mut self.inner, opaque: &mut self.opaque }
    }
}

#[pymethods]
impl PyStore {
    #[new]
    fn new() -> Self {
        PyStore { inner: Store::default(), opaque: Vec::new(), search: None }
    }

    #[staticmethod]
    fn from_json(text: &str) -> PyResult<Self> {
        json::from_json(text)
            .map(|inner| PyStore { inner, opaque: Vec::new(), search: None })
            .map_err(|e| PyValueError::new_err(e.to_string()))
    }

    #[staticmethod]
    fn from_dict(root: &Bound<PyDict>) -> PyResult<Self> {
        let mut store = PyStore::new();
        store.ingest().node(NONE, "", root)?;
        store.inner.finish();
        Ok(store)
    }

    /// Builder API: one call per node, parents before children; call finish() at the end.
    #[pyo3(signature = (parent, key, name, kind, provider, uid, meta=None))]
    fn add(
        &mut self, parent: Option<u32>, key: &str, name: &str, kind: &str, provider: &str, uid: &str,
        meta: Option<&Bound<PyDict>>,
    ) -> PyResult<u32> {
        let id = self.inner.start_node(parent.unwrap_or(NONE), NodeFields { key, name, kind, provider, uid });
        let mut ingest = self.ingest();
        for (k, v) in meta.into_iter().flat_map(|m| m.iter()) {
            ingest.meta(&k.str()?.to_string(), &v)?;
        }
        Ok(id)
    }

    fn finish(&mut self) {
        self.inner.finish();
    }

    fn __len__(&self) -> usize {
        self.inner.len()
    }

    fn key(&self, i: u32) -> PyResult<&str> {
        Ok(self.inner.key(self.check(i)?))
    }
    fn name(&self, i: u32) -> PyResult<&str> {
        Ok(self.inner.name(self.check(i)?))
    }
    fn kind(&self, i: u32) -> PyResult<&str> {
        Ok(self.inner.kind(self.check(i)?))
    }
    fn provider(&self, i: u32) -> PyResult<&str> {
        Ok(self.inner.provider(self.check(i)?))
    }
    fn uid(&self, i: u32) -> PyResult<&str> {
        Ok(self.inner.uid(self.check(i)?))
    }
    fn parent_of(&self, i: u32) -> PyResult<Option<u32>> {
        Ok(self.inner.parent(self.check(i)?))
    }
    fn children(&self, i: u32) -> PyResult<Vec<u32>> {
        Ok(self.inner.children(self.check(i)?).to_vec())
    }

    fn meta<'py>(&self, py: Python<'py>, i: u32) -> PyResult<Bound<'py, PyDict>> {
        let mut out = Out::new(py, &self.inner, &self.opaque, false);
        let d = PyDict::new(py);
        for (k, v) in self.inner.meta(self.check(i)?) {
            d.set_item(k, out.meta(v)?)?;
        }
        Ok(d)
    }

    fn get<'py>(&self, py: Python<'py>, i: u32, key: &str) -> PyResult<Option<Bound<'py, PyAny>>> {
        let mut out = Out::new(py, &self.inner, &self.opaque, false);
        self.inner.get(self.check(i)?, key).map(|m| out.meta(m)).transpose()
    }

    fn find_uid(&self, provider: &str, uid: &str) -> Option<u32> {
        self.inner.find_uid(provider, uid)
    }

    /// One attribute of a node, for the views: (True, child row) or (False, metadata value); None if absent.
    fn lookup<'py>(&self, py: Python<'py>, i: u32, key: &str) -> PyResult<Option<(bool, Bound<'py, PyAny>)>> {
        let i = self.check(i)?;
        if let Some(c) = self.inner.find_child(i, key) {
            return Ok(Some((true, c.into_pyobject(py)?.into_any())));
        }
        let mut out = Out::new(py, &self.inner, &self.opaque, false);
        self.inner.get(i, key).map(|m| Ok((false, out.meta(m)?))).transpose()
    }

    /// Children keys of a node, in order.
    fn child_keys(&self, i: u32) -> PyResult<Vec<&str>> {
        Ok(self.inner.children(self.check(i)?).iter().map(|&c| self.inner.key(c)).collect())
    }

    fn find_path(&self, path: &str) -> Option<u32> {
        self.inner.find_path(path)
    }

    #[pyo3(signature = (provider=None, kind=None, contains=None))]
    fn filter(&self, provider: Option<&str>, kind: Option<&str>, contains: Option<(String, String)>) -> Vec<u32> {
        self.inner.filter(provider, kind, contains.as_ref().map(|(k, t)| (k.as_str(), t.as_str())))
    }

    fn to_json(&self) -> PyResult<String> {
        serde_json::to_string(&NodeRef { store: &self.inner, id: 0 })
            .map(json::unquote_nonfinite)
            .map_err(|e| PyValueError::new_err(e.to_string()))
    }

    /// Rebuilds a tree of the old index classes ({type name: class}) from `row` down, for the legacy pickle.
    #[pyo3(signature = (types, default, row=0))]
    fn to_legacy<'py>(&self, py: Python<'py>, types: &Bound<'py, PyDict>, default: &Bound<'py, PyAny>, row: u32)
        -> PyResult<Bound<'py, PyAny>> {
        let row = self.check(row)?;
        Out::new(py, &self.inner, &self.opaque, true).legacy(row, types, default)
    }

    fn heap_bytes(&self) -> usize {
        self.inner.heap_bytes()
    }

    /// Builds SciQLop's search corpus (one path and one meta text per leaf) and its BM25F index.
    #[pyo3(signature = (leaf_kinds=vec!["ParameterIndex".to_owned(), "TemplatedParameterIndex".to_owned()]))]
    fn build_search(&mut self, py: Python, leaf_kinds: Vec<String>) -> usize {
        let kinds: Vec<&str> = leaf_kinds.iter().map(String::as_str).collect();
        let inner = &self.inner;
        let index = py.detach(|| search::SearchIndex::build(inner, &kinds));
        let n = index.leaves.len();
        self.search = Some(index);
        n
    }

    fn search_heap_bytes(&self) -> usize {
        self.search.as_ref().map_or(0, search::SearchIndex::heap_bytes)
    }

    /// (path text, meta text) of every leaf, by row: SciQLop's corpus_snapshot.
    fn corpus(&self) -> PyResult<Vec<(u32, String, String)>> {
        let inner = &self.inner;
        Ok(self.index()?.leaves.iter().map(|&r| { let (p, m) = search::leaf_texts(inner, r); (r, p, m) }).collect())
    }

    /// The flat filter model's pass: query syntax, filters, fuzzy score. [(row, score)] for non-zero scores.
    fn search(&self, py: Python, query: &str) -> PyResult<Vec<(u32, i32)>> {
        let (index, inner) = (self.index()?, &self.inner);
        let q = search::parse(query);
        let hits = py.detach(|| index.fuzzy(inner, &q));
        Ok(hits.into_iter().map(|(i, s)| (index.leaves[i], s)).collect())
    }

    /// SciQLop's BM25F signal: [(row, raw score)] for leaves matching at least one term.
    fn bm25(&self, py: Python, text: &str) -> PyResult<Vec<(u32, f64)>> {
        let index = self.index()?;
        let hits = py.detach(|| index.bm25(text));
        Ok(hits.into_iter().map(|(i, s)| (index.leaves[i], s)).collect())
    }
}

#[pyfunction]
fn subsequence_score(query: &str, candidate: &str) -> i32 {
    let q: Vec<char> = query.chars().map(|c| c.to_lowercase().next().unwrap_or(c)).collect();
    search::subsequence_score(&q, candidate)
}

/// (free text tokens, [(field, value)]), as SciQLopPlots' QueryParser gives them.
#[pyfunction]
fn parse_query(text: &str) -> (Vec<String>, Vec<(String, String)>) {
    let q = search::parse(text);
    (q.free_text, q.filters.into_iter().map(|f| (f.field, f.value)).collect())
}

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyStore>()?;
    m.add_function(wrap_pyfunction!(subsequence_score, m)?)?;
    m.add_function(wrap_pyfunction!(parse_query, m)?)
}
