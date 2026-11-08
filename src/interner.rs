use hashbrown::{DefaultHashBuilder, HashTable};
use std::hash::BuildHasher;

/// Each distinct string is stored once and named by its index. The hash table holds indices only, so a string's
/// bytes are not duplicated between the table and the list.
#[derive(Default)]
pub struct Interner {
    strings: Vec<Box<str>>,
    table: HashTable<u32>,
    hasher: DefaultHashBuilder,
}

impl Interner {
    pub fn intern(&mut self, s: &str) -> u32 {
        let Self { strings, table, hasher } = self;
        let hash = hasher.hash_one(s);
        if let Some(&id) = table.find(hash, |&id| &*strings[id as usize] == s) {
            return id;
        }
        let id = strings.len() as u32;
        strings.push(s.into());
        table.insert_unique(hash, id, |&id| hasher.hash_one(&*strings[id as usize]));
        id
    }

    pub fn get(&self, s: &str) -> Option<u32> {
        let hash = self.hasher.hash_one(s);
        self.table.find(hash, |&id| &*self.strings[id as usize] == s).copied()
    }

    pub fn str(&self, id: u32) -> &str {
        &self.strings[id as usize]
    }

    pub fn len(&self) -> usize {
        self.strings.len()
    }

    pub fn heap_bytes(&self) -> usize {
        self.strings.iter().map(|s| s.len()).sum::<usize>()
            + self.strings.capacity() * size_of::<Box<str>>()
            + self.table.capacity() * (size_of::<u32>() + 1)
    }
}
