//! SciQLop's product search, over the store's leaves:
//! - query syntax: SciQLopPlots QueryParser.cpp (free text, `field:value`, quoted tokens);
//! - filters: ProductsFlatFilterModel::filters_match (after/before on stop/start_date, provider, any metadata key);
//! - fuzzy signal: SubsequenceMatcher.hpp, same scores;
//! - BM25F signal: SciQLop components/smart_search/bm25_index.py, same tokens and constants.
//!
//! The text of a leaf follows SciQLop's speasy plugin: path = display names from the provider down, joined by ' ';
//! meta = name, then " key: value" for each scalar metadata, sorted by key (a QMap), then the plugin's extra keys.
use crate::store::{Meta, Store, Val};
use hashbrown::HashMap;
use rayon::prelude::*;
use std::cell::RefCell;

/// Leaf texts are not kept (they would be ~150 MB): each search regenerates them, which costs less than scoring them.
pub struct SearchIndex {
    pub leaves: Vec<u32>,
    terms: HashMap<Box<str>, u32>,
    postings: Vec<Vec<(u32, u16, u16)>>, // term -> (leaf, tf in path, tf in meta)
    path_len: Vec<u32>,
    meta_len: Vec<u32>,
    path_avgdl: f64,
    meta_avgdl: f64,
}

// ---------- leaf texts ----------

fn display_name(st: &Store, row: u32) -> String {
    match st.get(row, "name") {
        Some(Meta::Str(n)) if n != "AMDA" => n.to_owned(),
        _ => st.key(row).to_owned(),
    }
}

/// QVariant::toString of what get_node_meta keeps: scalars only (lists render as "" in a QVariantList).
fn scalar_text(st: &Store, m: &Meta) -> Option<String> {
    match m {
        Meta::Str(s) => Some((*s).to_owned()),
        Meta::Other(Val::Int(i)) => Some(i.to_string()),
        Meta::Other(Val::Float(f)) => Some(f.to_string()),
        Meta::Other(Val::Bool(b)) => Some((if *b { "true" } else { "false" }).to_owned()),
        Meta::Other(Val::Str(id)) => Some(st.s.str(*id).to_owned()),
        _ => None,
    }
}

fn ancestors(st: &Store, row: u32) -> Vec<u32> {
    let mut chain: Vec<u32> = std::iter::successors(st.parent(row), |&p| st.parent(p)).collect();
    chain.pop(); // the store root ("all") is not shown in SciQLop
    chain.reverse();
    chain
}

pub fn leaf_texts(st: &Store, row: u32) -> (String, String) {
    let chain = ancestors(st, row);
    let name = display_name(st, row);
    let path = chain.iter().map(|&a| display_name(st, a)).chain([name.clone()]).collect::<Vec<_>>().join(" ");
    let description = chain
        .iter()
        .filter_map(|&a| match st.get(a, "desc").or_else(|| st.get(a, "description")) {
            Some(Meta::Str(d)) if !d.is_empty() => Some(d.to_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ");
    let speasy_id = format!("{}/{}", st.provider(row), st.uid(row));
    let mut fields: Vec<(String, String)> = st
        .meta(row)
        .filter(|(k, _)| !k.starts_with('_'))
        .filter_map(|(k, v)| scalar_text(st, &v).map(|t| (k.to_owned(), t)))
        .collect();
    fields.extend([
        ("uid".into(), st.uid(row).into()),
        ("provider".into(), st.provider(row).into()),
        ("speasy_id".into(), speasy_id.clone()),
        ("stable_id".into(), speasy_id),
    ]);
    if !description.is_empty() {
        fields.push(("instrument_description".into(), description));
    }
    fields.sort_by(|a, b| a.0.cmp(&b.0));
    fields.dedup_by(|a, b| a.0 == b.0);
    let meta = fields.iter().fold(name, |acc, (k, v)| acc + " " + k + ": " + v);
    (path, meta)
}

// ---------- BM25F ----------

const K1: f64 = 1.5;
const B: f64 = 0.75;
const W_PATH: f64 = 6.0;
const W_META: f64 = 1.0;

/// Python's `re.findall(r'[a-z0-9]+', text.lower())`.
fn tokens(text: &str) -> impl Iterator<Item = String> + '_ {
    let lowered = text.chars().map(|c| c.to_lowercase().next().unwrap_or(c));
    let mut current = String::new();
    let mut out = Vec::new();
    for c in lowered.chain([' ']) {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            current.push(c);
        } else if !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
    }
    out.into_iter()
}

impl SearchIndex {
    pub fn build(st: &Store, leaf_kinds: &[&str]) -> SearchIndex {
        let leaves: Vec<u32> = (0..st.len() as u32).filter(|&r| leaf_kinds.contains(&st.kind(r))).collect();
        // per leaf: its (term, tf path, tf meta) list, in parallel; then one sequential pass assigns term ids
        let docs: Vec<Vec<(String, u16, u16)>> = leaves
            .par_iter()
            .map(|&row| {
                let (path, meta) = leaf_texts(st, row);
                let mut tf: HashMap<String, (u16, u16)> = HashMap::new();
                for t in tokens(&path) {
                    tf.entry(t).or_default().0 += 1;
                }
                for t in tokens(&meta) {
                    let e = tf.entry(t).or_default();
                    e.1 = e.1.saturating_add(1);
                }
                tf.into_iter().map(|(t, (p, m))| (t, p, m)).collect()
            })
            .collect();
        let mut terms: HashMap<Box<str>, u32> = HashMap::new();
        let mut postings: Vec<Vec<(u32, u16, u16)>> = Vec::new();
        let (mut path_len, mut meta_len) = (Vec::with_capacity(leaves.len()), Vec::with_capacity(leaves.len()));
        for (doc, entries) in docs.into_iter().enumerate() {
            let (np, nm) = entries.iter().fold((0u32, 0u32), |(a, b), (_, p, m)| (a + *p as u32, b + *m as u32));
            for (t, p, m) in entries {
                let next = terms.len() as u32;
                let id = *terms.entry(t.into_boxed_str()).or_insert(next);
                if id as usize == postings.len() {
                    postings.push(Vec::new());
                }
                postings[id as usize].push((doc as u32, p, m));
            }
            path_len.push(np);
            meta_len.push(nm);
        }
        let avg = |v: &[u32]| if v.is_empty() { 1.0 } else { v.iter().map(|&x| x as f64).sum::<f64>() / v.len() as f64 };
        let (path_avgdl, meta_avgdl) = (avg(&path_len), avg(&meta_len));
        SearchIndex { leaves, terms, postings, path_len, meta_len, path_avgdl, meta_avgdl }
    }

    /// (leaf index, raw BM25F score) for every leaf matching at least one term.
    pub fn bm25(&self, query: &str) -> Vec<(usize, f64)> {
        let n_docs = self.leaves.len() as f64;
        let avgdl = W_PATH * self.path_avgdl + W_META * self.meta_avgdl;
        let mut scores = vec![0.0f64; self.leaves.len()];
        let mut hit = vec![false; self.leaves.len()];
        for term in tokens(query) {
            let Some(&id) = self.terms.get(term.as_str()) else { continue };
            let posting = &self.postings[id as usize];
            let n = posting.len() as f64;
            let idf = ((n_docs - n + 0.5) / (n + 0.5) + 1.0).ln();
            for &(doc, p, m) in posting {
                let d = doc as usize;
                let tf = W_PATH * p as f64 + W_META * m as f64;
                let doc_len = W_PATH * self.path_len[d] as f64 + W_META * self.meta_len[d] as f64;
                scores[d] += idf * (tf * (K1 + 1.0)) / (tf + K1 * (1.0 - B + B * doc_len / avgdl));
                hit[d] = true;
            }
        }
        (0..scores.len()).filter(|&d| hit[d]).map(|d| (d, scores[d])).collect()
    }

    /// (leaf index, fuzzy score) of the leaves passing the filters with a non-zero score, like the flat filter model.
    pub fn fuzzy(&self, st: &Store, query: &Query) -> Vec<(usize, i32)> {
        let lowered: Vec<Vec<char>> = query.free_text.iter().map(|t| lower(t)).collect();
        self.leaves
            .par_iter()
            .enumerate()
            .filter(|(_, row)| filters_match(st, **row, &query.filters))
            .map(|(i, &row)| {
                if lowered.is_empty() {
                    return (i, 1);
                }
                let (path, meta) = leaf_texts(st, row);
                (i, free_text_score(&lowered, &path, &meta))
            })
            .filter(|&(_, s)| s > 0)
            .collect()
    }

    pub fn heap_bytes(&self) -> usize {
        self.leaves.capacity() * 4
            + self.terms.keys().map(|k| k.len() + 24).sum::<usize>()
            + self.postings.iter().map(|p| p.capacity() * 8 + 24).sum::<usize>()
            + self.path_len.capacity() * 8
    }
}

// ---------- fuzzy subsequence matcher (SubsequenceMatcher.hpp) ----------

const NEG_INF: i32 = i32::MIN / 2;

fn lower(s: &str) -> Vec<char> {
    s.chars().map(|c| c.to_lowercase().next().unwrap_or(c)).collect()
}

fn word_start_bonus(prev: Option<char>, c: char) -> i32 {
    match prev {
        None | Some('_' | ' ' | '/') => 3,
        Some(p) if c.is_uppercase() && p.is_lowercase() => 2,
        _ => 0,
    }
}

/// A candidate lowercased with its word-start bonuses: computed once per text, shared by every query token.
#[derive(Default)]
struct Prepared {
    lower: Vec<char>,
    bonus: Vec<i32>,
}

impl Prepared {
    fn fill(&mut self, candidate: &str) {
        self.lower.clear();
        self.bonus.clear();
        let mut prev_char = None;
        for c in candidate.chars() {
            self.lower.push(c.to_lowercase().next().unwrap_or(c));
            self.bonus.push(word_start_bonus(prev_char, c));
            prev_char = Some(c);
        }
    }
}

#[derive(Default)]
struct Scratch {
    path: Prepared,
    meta: Prepared,
    prev: Vec<i32>,
    cur: Vec<i32>,
}

thread_local! { static SCRATCH: RefCell<Scratch> = RefCell::new(Scratch::default()); }

/// Best alignment of `query` (already lowercased) as a subsequence of the candidate; -1 when there is none.
fn best_alignment(query: &[char], c: &Prepared, prev: &mut Vec<i32>, cur: &mut Vec<i32>) -> i32 {
    let (n, m) = (c.lower.len(), query.len());
    if m > n {
        return -1;
    }
    prev.clear();
    prev.extend((0..n).map(|i| if c.lower[i] == query[0] { 1 + c.bonus[i] } else { NEG_INF }));
    for &qj in &query[1..] {
        cur.clear();
        cur.resize(n, NEG_INF);
        let mut prefix_max = NEG_INF;
        for i in 0..n {
            let adjacent = if i >= 1 && prev[i - 1] != NEG_INF { prev[i - 1] + 4 } else { NEG_INF };
            let best_prev = prefix_max.max(adjacent);
            if best_prev != NEG_INF && qj == c.lower[i] {
                cur[i] = best_prev + 1 + c.bonus[i];
            }
            if i >= 1 && prev[i - 1] > prefix_max {
                prefix_max = prev[i - 1];
            }
        }
        std::mem::swap(prev, cur);
    }
    let best = prev.iter().copied().max().unwrap_or(NEG_INF);
    if best == NEG_INF { -1 } else { best }
}

fn score_prepared(query: &[char], c: &Prepared, prev: &mut Vec<i32>, cur: &mut Vec<i32>) -> i32 {
    if query.is_empty() {
        return 1;
    }
    if c.lower.is_empty() {
        return 0;
    }
    let raw = best_alignment(query, c, prev, cur);
    if raw < 0 {
        return 0;
    }
    let length_penalty = c.lower.len() as i32 - query.len() as i32;
    1.max(raw * 100 / (100 + length_penalty))
}

pub fn subsequence_score(query: &[char], candidate: &str) -> i32 {
    SCRATCH.with(|s| {
        let s = &mut *s.borrow_mut();
        s.path.fill(candidate);
        score_prepared(query, &s.path, &mut s.prev, &mut s.cur)
    })
}

/// ProductsFlatFilterModel::free_text_score: every token must match path or meta; scores add up.
fn free_text_score(tokens: &[Vec<char>], path: &str, meta: &str) -> i32 {
    if tokens.is_empty() {
        return 1;
    }
    SCRATCH.with(|s| {
        let s = &mut *s.borrow_mut();
        s.path.fill(path);
        s.meta.fill(meta);
        let mut total = 0;
        for t in tokens {
            let score = score_prepared(t, &s.path, &mut s.prev, &mut s.cur)
                .max(score_prepared(t, &s.meta, &mut s.prev, &mut s.cur));
            if score == 0 {
                return 0;
            }
            total += score;
        }
        total
    })
}

// ---------- query syntax (QueryParser.cpp) and filters ----------

pub struct Filter {
    pub field: String,
    pub value: String,
    date: Option<[u32; 6]>,
}

#[derive(Default)]
pub struct Query {
    pub free_text: Vec<String>,
    pub filters: Vec<Filter>,
}

fn extract_token(chars: &[char], pos: &mut usize) -> String {
    if *pos < chars.len() && chars[*pos] == '"' {
        *pos += 1;
        let start = *pos;
        while *pos < chars.len() && chars[*pos] != '"' {
            *pos += 1;
        }
        let token = chars[start..*pos].iter().collect();
        if *pos < chars.len() {
            *pos += 1;
        }
        return token;
    }
    let start = *pos;
    while *pos < chars.len() && !chars[*pos].is_whitespace() {
        *pos += 1;
    }
    chars[start..*pos].iter().collect()
}

pub fn parse(input: &str) -> Query {
    let chars: Vec<char> = input.chars().collect();
    let (mut q, mut pos) = (Query::default(), 0);
    while pos < chars.len() {
        while pos < chars.len() && chars[pos].is_whitespace() {
            pos += 1;
        }
        if pos >= chars.len() {
            break;
        }
        let colon = (chars[pos] != '"')
            .then(|| chars[pos..].iter().take_while(|c| !c.is_whitespace()).position(|&c| c == ':'))
            .flatten();
        match colon {
            Some(offset) => {
                let field: String = chars[pos..pos + offset].iter().collect();
                pos += offset + 1;
                let value = extract_token(&chars, &mut pos);
                let date = match field.to_lowercase().as_str() {
                    "after" => partial_date(&value, false),
                    "before" => partial_date(&value, true),
                    _ => None,
                };
                q.filters.push(Filter { field, value, date });
            }
            None => {
                let text = extract_token(&chars, &mut pos);
                if !text.is_empty() {
                    q.free_text.push(text);
                }
            }
        }
    }
    q
}

/// "YYYY-MM-DD[THH:MM[:SS[.fff]]]..." -> [y, m, d, H, M, S]. simplify: ignores the UTC offset (Qt applies it).
fn iso(s: &str) -> Option<[u32; 6]> {
    let num = |a: usize, b: usize| s.get(a..b).filter(|x| x.bytes().all(|c| c.is_ascii_digit()))?.parse().ok();
    let date = [num(0, 4)?, num(5, 7)?, num(8, 10)?];
    if s.as_bytes().get(4) != Some(&b'-') || s.as_bytes().get(7) != Some(&b'-') {
        return None;
    }
    let time = match s.as_bytes().get(10) {
        Some(b'T') | Some(b' ') => [num(11, 13)?, num(14, 16)?, num(17, 19).unwrap_or(0)],
        _ => [0, 0, 0],
    };
    Some([date[0], date[1], date[2], time[0], time[1], time[2]])
}

fn days_in_month(y: u32, m: u32) -> u32 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// QueryParser's parse_partial_date: "2020", "2020-3", "2020-03-15" or an ISO datetime.
fn partial_date(v: &str, end: bool) -> Option<[u32; 6]> {
    let parts: Vec<&str> = v.split('-').collect();
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit());
    let (eod, sod) = ([23, 59, 59], [0, 0, 0]);
    let t = if end { eod } else { sod };
    match parts.as_slice() {
        [y] if y.len() == 4 && digits(y) => {
            let y = y.parse().ok()?;
            Some(if end { [y, 12, 31, 23, 59, 59] } else { [y, 1, 1, 0, 0, 0] })
        }
        [y, m] if y.len() == 4 && digits(y) && (1..=2).contains(&m.len()) && digits(m) => {
            let (y, m) = (y.parse().ok()?, m.parse().ok()?);
            Some([y, m, if end { days_in_month(y, m) } else { 1 }, t[0], t[1], t[2]])
        }
        _ if v.len() == 10 => iso(v).map(|d| [d[0], d[1], d[2], t[0], t[1], t[2]]),
        _ => iso(v),
    }
}

fn meta_string(st: &Store, row: u32, key: &str) -> Option<String> {
    st.get(row, key).and_then(|m| scalar_text(st, &m))
}

fn filters_match(st: &Store, row: u32, filters: &[Filter]) -> bool {
    filters.iter().all(|f| match f.field.to_lowercase().as_str() {
        "after" => f.date.is_some_and(|d| meta_string(st, row, "stop_date").and_then(|s| iso(&s)).is_some_and(|n| n >= d)),
        "before" => {
            f.date.is_some_and(|d| meta_string(st, row, "start_date").and_then(|s| iso(&s)).is_some_and(|n| n <= d))
        }
        // simplify: `type:` needs SciQLop's ParameterType (data_serie_type); add it as a column at ingest
        "provider" => st.provider(row).eq_ignore_ascii_case(&f.value),
        _ => meta_string(st, row, &f.field).is_some_and(|v| v.to_lowercase() == f.value.to_lowercase()),
    })
}
