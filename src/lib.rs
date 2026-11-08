//! Python bindings. Row ids are plain ints; node 0 is the root.
mod interner;
mod json;
mod store;

use json::{NodeRef, SPZ_KEYS};
use pyo3::exceptions::{PyIndexError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple};
use serde_json::{Map, Number, Value};
use store::{Meta, NodeFields, OTHER, Store, NONE};

fn to_json(v: &Bound<PyAny>) -> PyResult<Value> {
    Ok(if v.is_none() {
        Value::Null
    } else if let Ok(b) = v.cast::<PyBool>() {
        Value::Bool(b.is_true())
    } else if v.is_instance_of::<PyInt>() {
        Value::Number(v.extract::<i64>()?.into())
    } else if v.is_instance_of::<PyFloat>() {
        Number::from_f64(v.extract::<f64>()?).map_or(Value::Null, Value::Number)
    } else if let Ok(s) = v.cast::<PyString>() {
        Value::String(s.to_str()?.to_owned())
    } else if v.is_instance_of::<PyList>() || v.is_instance_of::<PyTuple>() {
        Value::Array(v.try_iter()?.map(|x| to_json(&x?)).collect::<PyResult<_>>()?)
    } else if let Ok(d) = v.cast::<PyDict>() {
        let map: PyResult<Map<String, Value>> = d.iter().map(|(k, v)| Ok((k.str()?.to_string(), to_json(&v)?))).collect();
        Value::Object(map?)
    } else {
        Value::String(v.str()?.to_string())
    })
}

fn to_py<'py>(py: Python<'py>, v: &Value) -> PyResult<Bound<'py, PyAny>> {
    Ok(match v {
        Value::Null => py.None().into_bound(py),
        Value::Bool(b) => PyBool::new(py, *b).to_owned().into_any(),
        Value::Number(n) => match n.as_i64() {
            Some(i) => i.into_pyobject(py)?.into_any(),
            None => n.as_f64().unwrap_or(f64::NAN).into_pyobject(py)?.into_any(),
        },
        Value::String(s) => PyString::new(py, s).into_any(),
        Value::Array(a) => PyList::new(py, a.iter().map(|x| to_py(py, x)).collect::<PyResult<Vec<_>>>()?)?.into_any(),
        Value::Object(o) => {
            let d = PyDict::new(py);
            for (k, v) in o {
                d.set_item(k, to_py(py, v)?)?;
            }
            d.into_any()
        }
    })
}

fn meta_to_py<'py>(py: Python<'py>, m: Meta) -> PyResult<Bound<'py, PyAny>> {
    match m {
        Meta::Str(s) => Ok(PyString::new(py, s).into_any()),
        Meta::Other(v) => to_py(py, v),
    }
}

fn str_field(d: &Bound<PyDict>, key: &str) -> PyResult<String> {
    Ok(match d.get_item(key)? {
        Some(v) => v.str()?.to_string(),
        None => String::new(),
    })
}

fn push_meta(store: &mut Store, key: &str, v: &Bound<PyAny>) -> PyResult<()> {
    match v.cast::<PyString>() {
        Ok(s) => store.push_str(key, s.to_str()?),
        Err(_) => store.push_other(key, to_json(v)?),
    }
    Ok(())
}

fn load_dict(store: &mut Store, parent: u32, key: &str, node: &Bound<PyDict>) -> PyResult<()> {
    let [name, kind, provider, uid] =
        ["__spz_name__", "__spz_type__", "__spz_provider__", "__spz_uid__"].map(|k| str_field(node, k));
    let fields = NodeFields { key, name: &name?, kind: &kind?, provider: &provider?, uid: &uid? };
    let id = store.start_node(parent, fields);
    let mut children = Vec::new();
    for (k, v) in node.iter() {
        let k = k.str()?.to_string();
        if SPZ_KEYS.contains(&k.as_str()) {
            continue;
        }
        match v.cast::<PyDict>() {
            Ok(d) if d.contains("__spz_type__")? => children.push((k, d.clone())),
            _ => push_meta(store, &k, &v)?,
        }
    }
    for (k, child) in children {
        load_dict(store, id, &k, &child)?;
    }
    Ok(())
}

/// One Python string object per distinct string, so the legacy tree and its pickle share them too.
struct PyStrings<'py> {
    py: Python<'py>,
    cache: Vec<Option<Bound<'py, PyString>>>,
}

impl<'py> PyStrings<'py> {
    fn get(&mut self, store: &Store, id: u32) -> Bound<'py, PyString> {
        self.cache[id as usize].get_or_insert_with(|| PyString::new(self.py, store.s.str(id))).clone()
    }
}

#[pyclass(name = "NativeStore", module = "inventory_store")]
struct PyStore {
    inner: Store,
}

impl PyStore {
    fn check(&self, i: u32) -> PyResult<u32> {
        if (i as usize) < self.inner.len() { Ok(i) } else { Err(PyIndexError::new_err(i)) }
    }

    fn legacy<'py>(
        &self, i: u32, types: &Bound<'py, PyDict>, default: &Bound<'py, PyAny>, strings: &mut PyStrings<'py>,
    ) -> PyResult<Bound<'py, PyAny>> {
        let st = &self.inner;
        let [_, name, kind, provider, uid] = st.raw(i);
        let cls = types.get_item(strings.get(st, kind))?.unwrap_or_else(|| default.clone());
        let obj = cls.call_method1("__new__", (&cls,))?;
        let dict = obj.getattr("__dict__")?.cast_into::<PyDict>()?;
        for (k, v) in st.raw_meta(i) {
            let value = if v & OTHER != 0 { to_py(strings.py, st.other(v))? } else { strings.get(st, v).into_any() };
            dict.set_item(strings.get(st, k), value)?;
        }
        for &c in st.children(i) {
            dict.set_item(strings.get(st, st.raw(c)[0]), self.legacy(c, types, default, strings)?)?;
        }
        for (k, v) in SPZ_KEYS.iter().zip([kind, name, provider, uid]) {
            dict.set_item(PyString::intern(strings.py, k), strings.get(st, v))?;
        }
        Ok(obj)
    }
}

#[pymethods]
impl PyStore {
    #[new]
    fn new() -> Self {
        PyStore { inner: Store::default() }
    }

    #[staticmethod]
    fn from_json(text: &str) -> PyResult<Self> {
        json::from_json(text).map(|inner| PyStore { inner }).map_err(|e| PyValueError::new_err(e.to_string()))
    }

    #[staticmethod]
    fn from_dict(root: &Bound<PyDict>) -> PyResult<Self> {
        let mut inner = Store::default();
        load_dict(&mut inner, NONE, "", root)?;
        inner.finish();
        Ok(PyStore { inner })
    }

    /// Builder API: one call per node, parents before children; call finish() at the end.
    #[pyo3(signature = (parent, key, name, kind, provider, uid, meta=None))]
    fn add(
        &mut self, parent: Option<u32>, key: &str, name: &str, kind: &str, provider: &str, uid: &str,
        meta: Option<&Bound<PyDict>>,
    ) -> PyResult<u32> {
        let id = self.inner.start_node(parent.unwrap_or(NONE), NodeFields { key, name, kind, provider, uid });
        for (k, v) in meta.into_iter().flat_map(|m| m.iter()) {
            push_meta(&mut self.inner, &k.str()?.to_string(), &v)?;
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
        let d = PyDict::new(py);
        for (k, v) in self.inner.meta(self.check(i)?) {
            d.set_item(k, meta_to_py(py, v)?)?;
        }
        Ok(d)
    }

    fn get<'py>(&self, py: Python<'py>, i: u32, key: &str) -> PyResult<Option<Bound<'py, PyAny>>> {
        self.inner.get(self.check(i)?, key).map(|m| meta_to_py(py, m)).transpose()
    }

    fn find_uid(&self, provider: &str, uid: &str) -> Option<u32> {
        self.inner.find_uid(provider, uid)
    }

    fn find_path(&self, path: &str) -> Option<u32> {
        self.inner.find_path(path)
    }

    #[pyo3(signature = (provider=None, kind=None, contains=None))]
    fn filter(&self, provider: Option<&str>, kind: Option<&str>, contains: Option<(String, String)>) -> Vec<u32> {
        self.inner.filter(provider, kind, contains.as_ref().map(|(k, t)| (k.as_str(), t.as_str())))
    }

    fn to_json(&self) -> PyResult<String> {
        serde_json::to_string(&NodeRef { store: &self.inner, id: 0 }).map_err(|e| PyValueError::new_err(e.to_string()))
    }

    /// Rebuilds a tree of the old index classes ({type name: class}), for the legacy pickle.
    fn to_legacy<'py>(&self, py: Python<'py>, types: &Bound<'py, PyDict>, default: &Bound<'py, PyAny>)
        -> PyResult<Bound<'py, PyAny>> {
        let mut strings = PyStrings { py, cache: vec![None; self.inner.s.len()] };
        self.legacy(0, types, default, &mut strings)
    }

    fn heap_bytes(&self) -> usize {
        self.inner.heap_bytes()
    }
}

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyStore>()
}
