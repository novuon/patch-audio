//! Byte-bounded LRU for control-plane data. Never used by the RT callback.

use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;

pub struct Cache<K, V> {
    entries: HashMap<K, (V, usize, u64)>,
    order: BTreeMap<u64, K>,
    clock: u64,
    bytes: usize,
    budget: usize,
}

impl<K: Eq + Hash + Clone, V> Cache<K, V> {
    pub fn new(budget: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: BTreeMap::new(),
            clock: 0,
            bytes: 0,
            budget,
        }
    }

    pub fn get(&mut self, key: &K) -> Option<&V> {
        let entry = self.entries.get_mut(key)?;
        self.order.remove(&entry.2);
        self.clock += 1;
        entry.2 = self.clock;
        self.order.insert(self.clock, key.clone());
        Some(&entry.0)
    }

    pub fn insert(&mut self, key: K, value: V, bytes: usize) {
        if let Some((_, old, age)) = self.entries.remove(&key) {
            self.bytes -= old;
            self.order.remove(&age);
        }
        // Oversized entries may be used by the caller, but never retained.
        if bytes > self.budget {
            return;
        }
        while self.bytes > self.budget - bytes {
            let Some((_, oldest)) = self.order.pop_first() else {
                break;
            };
            if let Some((_, used, _)) = self.entries.remove(&oldest) {
                self.bytes -= used;
            }
        }
        self.clock += 1;
        self.bytes += bytes;
        self.order.insert(self.clock, key.clone());
        self.entries.insert(key, (value, bytes, self.clock));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicts_only_least_recent_and_accounts_for_replacements() {
        let mut c = Cache::new(10);
        c.insert(1, "one", 4);
        c.insert(2, "two", 4);
        assert_eq!(c.get(&1), Some(&"one"));
        c.insert(3, "three", 4);
        assert!(c.get(&2).is_none());
        assert_eq!(c.get(&1), Some(&"one"));
        c.insert(1, "new", 6);
        assert_eq!(c.bytes, 10);
        c.insert(1, "too large", 11);
        assert!(c.get(&1).is_none());
        assert_eq!(c.get(&3), Some(&"three"));
        assert_eq!(c.bytes, 4);
    }
}
