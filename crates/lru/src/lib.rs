//! Bounded, first-party LRU cache for the store-reader integration.
//!
//! This crate intentionally provides only the operations used by Tantivy's
//! decompressed-block cache. Keys are `Copy`, so eviction cannot invoke a
//! user-defined key destructor while the cache is being updated.

use std::collections::HashMap;
use std::hash::Hash;
use std::num::NonZeroUsize;

struct Node<K, V> {
    key: K,
    value: V,
    previous: Option<usize>,
    next: Option<usize>,
}

/// A capacity-bounded cache with average constant-time lookup and recency updates.
pub struct LruCache<K: Copy + Eq + Hash, V> {
    indices: HashMap<K, usize>,
    nodes: Vec<Option<Node<K, V>>>,
    free: Vec<usize>,
    head: Option<usize>,
    tail: Option<usize>,
    capacity: NonZeroUsize,
}

impl<K: Copy + Eq + Hash, V> LruCache<K, V> {
    /// Creates an empty cache with a nonzero maximum number of entries.
    #[must_use]
    pub fn new(capacity: NonZeroUsize) -> Self {
        Self {
            indices: HashMap::new(),
            nodes: Vec::new(),
            free: Vec::new(),
            head: None,
            tail: None,
            capacity,
        }
    }

    /// Returns the number of cached entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.indices.len()
    }

    /// Returns whether the cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    /// Looks up a value and marks it as the most recently used entry.
    pub fn get(&mut self, key: &K) -> Option<&mut V> {
        let index = *self.indices.get(key)?;
        self.touch(index);
        Some(&mut self.node_mut(index).value)
    }

    /// Inserts a value, returning the previous value only when the key existed.
    ///
    /// When full, insertion evicts and drops the least recently used value.
    /// The new entry remains usable even if that value's destructor panics.
    pub fn put(&mut self, key: K, value: V) -> Option<V> {
        if let Some(&index) = self.indices.get(&key) {
            let previous = std::mem::replace(&mut self.node_mut(index).value, value);
            self.touch(index);
            return Some(previous);
        }

        let evicted = if self.len() == self.capacity.get() {
            Some(self.evict_lru())
        } else {
            None
        };
        self.insert_new(key, value);
        drop(evicted);
        None
    }

    /// Borrows the least recently used entry without changing its position.
    #[must_use]
    pub fn peek_lru(&self) -> Option<(&K, &V)> {
        let node = self.node(self.tail?);
        Some((&node.key, &node.value))
    }

    fn node(&self, index: usize) -> &Node<K, V> {
        self.nodes[index].as_ref().expect("active cache node")
    }

    fn node_mut(&mut self, index: usize) -> &mut Node<K, V> {
        self.nodes[index].as_mut().expect("active cache node")
    }

    fn detach(&mut self, index: usize) {
        let previous = self.node(index).previous;
        let next = self.node(index).next;
        if let Some(previous) = previous {
            self.node_mut(previous).next = next;
        } else {
            self.head = next;
        }
        if let Some(next) = next {
            self.node_mut(next).previous = previous;
        } else {
            self.tail = previous;
        }
    }

    fn attach_head(&mut self, index: usize) {
        let old_head = self.head;
        let node = self.node_mut(index);
        node.previous = None;
        node.next = old_head;
        if let Some(old_head) = old_head {
            self.node_mut(old_head).previous = Some(index);
        } else {
            self.tail = Some(index);
        }
        self.head = Some(index);
    }

    fn touch(&mut self, index: usize) {
        if self.head != Some(index) {
            self.detach(index);
            self.attach_head(index);
        }
    }

    fn evict_lru(&mut self) -> Node<K, V> {
        let index = self.tail.expect("full cache has a tail");
        let key = self.node(index).key;
        self.indices.remove(&key);
        self.detach(index);
        let evicted = self.nodes[index].take().expect("active cache node");
        self.free.push(index);
        evicted
    }

    fn insert_new(&mut self, key: K, value: V) {
        let index = self.free.last().copied().unwrap_or(self.nodes.len());
        self.indices.insert(key, index);
        let node = Node {
            key,
            value,
            previous: None,
            next: None,
        };
        if self.free.last() == Some(&index) {
            self.free.pop();
            self.nodes[index] = Some(node);
        } else {
            self.nodes.push(Some(node));
        }
        self.attach_head(index);
    }
}

#[cfg(test)]
mod tests {
    use super::LruCache;
    use std::collections::VecDeque;
    use std::num::NonZeroUsize;

    #[test]
    fn capacity_one_replaces_and_evicts() {
        let mut cache = LruCache::new(NonZeroUsize::new(1).unwrap());
        assert!(cache.is_empty());
        assert_eq!(cache.put(3, "first"), None);
        assert_eq!(cache.put(3, "second"), Some("first"));
        assert_eq!(cache.peek_lru(), Some((&3, &"second")));
        assert_eq!(cache.put(4, "third"), None);
        assert_eq!(cache.get(&3), None);
        assert_eq!(cache.get(&4), Some(&mut "third"));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn recency_matches_reference_model() {
        for capacity in [1, 2, 3, 16] {
            let mut cache = LruCache::new(NonZeroUsize::new(capacity).unwrap());
            let mut reference = VecDeque::<(u16, u16)>::new();
            let mut state = 0x7f4a_7c15_u32;

            for _ in 0..20_000 {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                let key = (state % 23) as u16;
                let value = (state >> 16) as u16;
                let position = reference
                    .iter()
                    .position(|(candidate, _)| *candidate == key);

                if state & 1 == 0 {
                    let expected = position.map(|position| {
                        let (_, previous) = reference.remove(position).unwrap();
                        reference.push_front((key, previous));
                        previous
                    });
                    assert_eq!(cache.get(&key).copied(), expected);
                } else {
                    let expected = position.map(|position| reference.remove(position).unwrap().1);
                    reference.push_front((key, value));
                    reference.truncate(capacity);
                    assert_eq!(cache.put(key, value), expected);
                }

                assert_eq!(cache.len(), reference.len());
                assert_eq!(
                    cache.peek_lru().map(|(&key, &value)| (key, value)),
                    reference.back().copied()
                );
                for &(key, value) in &reference {
                    assert_eq!(cache.node(*cache.indices.get(&key).unwrap()).value, value);
                }
            }
        }
    }

    #[test]
    fn panicking_evicted_value_leaves_new_entry_consistent() {
        struct PanicOnDrop(bool);

        impl Drop for PanicOnDrop {
            fn drop(&mut self) {
                assert!(!self.0, "intentional destructor panic");
            }
        }

        let mut cache = LruCache::new(NonZeroUsize::new(1).unwrap());
        cache.put(1, PanicOnDrop(true));
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            cache.put(2, PanicOnDrop(false));
        }));
        assert!(outcome.is_err());
        assert_eq!(cache.len(), 1);
        assert!(cache.get(&1).is_none());
        assert!(cache.get(&2).is_some());
        assert_eq!(cache.peek_lru().map(|(&key, _)| key), Some(2));
    }
}
