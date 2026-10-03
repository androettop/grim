use std::collections::HashMap;

/// Global name, shared by every package (package name tables are per file).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NameId(pub u32);

/// Case-insensitive interner, like Unreal names. Keeps the first spelling seen.
#[derive(Default)]
pub struct Names {
    list: Vec<String>,
    index: HashMap<String, NameId>,
    /// Every spelling already seen, as it was written: the engine asks for the same few names
    /// thousands of times a tick, and this finds them without folding the case of a copy first.
    spelled: HashMap<String, NameId>,
}

impl Names {
    pub fn intern(&mut self, s: &str) -> NameId {
        if let Some(&id) = self.spelled.get(s) {
            return id;
        }
        let key = s.to_ascii_lowercase();
        let id = match self.index.get(&key) {
            Some(&id) => id,
            None => {
                let id = NameId(self.list.len() as u32);
                self.list.push(s.to_string());
                self.index.insert(key, id);
                id
            }
        };
        self.spelled.insert(s.to_string(), id);
        id
    }

    pub fn get(&self, s: &str) -> Option<NameId> {
        self.index.get(&s.to_ascii_lowercase()).copied()
    }

    pub fn str(&self, id: NameId) -> &str {
        &self.list[id.0 as usize]
    }
}
