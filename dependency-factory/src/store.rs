//! The map under a [`Registry`](crate::Registry): type-keyed, type-erased,
//! and nothing more. Locking and sharing live in `registry`. A `Store` is
//! plain data, so the registry's two buffers and a transaction's overlay
//! are all the same type and merge into one another with one method.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::Arc;

use crate::registry::Key;

type KeyMap<K> = HashMap<K, Arc<<K as Key>::Value>>;

/// A `KeyMap<K>` behind a type-erased box: `Any` so lookups can downcast
/// it, `merge_into` and `move_into` so its entries can be copied or moved
/// into another [`Store`] without the caller knowing `K`.
trait AnyMap: Any + Send + Sync {
    /// Copy every entry into `into`, overwriting any already there under
    /// the same key. Entries are `Arc`s, so each copy is a refcount bump.
    fn merge_into(&self, into: &mut Store);
    /// Move every entry into `into`, overwriting any already there under
    /// the same key.
    fn move_into(self: Box<Self>, into: &mut Store);
}

impl<K: Key> AnyMap for KeyMap<K> {
    fn merge_into(&self, into: &mut Store) {
        into.map_mut::<K>()
            .extend(self.iter().map(|(k, v)| (k.clone(), Arc::clone(v))));
    }

    fn move_into(self: Box<Self>, into: &mut Store) {
        into.map_mut::<K>().extend(*self);
    }
}

/// A map from [`Key`] values to `Arc`-shared values, for any number of
/// key types at once.
///
/// Outer key: `TypeId` of the key type `K`. Stored value: a boxed
/// `KeyMap<K>`, type-erased so that maps for different key types can live
/// in one outer map. The inner map is recovered by downcasting on lookup;
/// because `K` fixes its value type, the downcast cannot fail.
///
/// An inner map is created only by [`insert`](Self::insert), which always
/// adds an entry, and nothing removes entries, so a store with no inner
/// maps has no entries.
#[derive(Default)]
pub(crate) struct Store {
    maps: HashMap<TypeId, Box<dyn AnyMap>>,
}

impl Store {
    /// Return the value stored under `key`, if one is present.
    pub(crate) fn get<K: Key>(&self, key: &K) -> Option<Arc<K::Value>> {
        let any: &dyn Any = self.maps.get(&TypeId::of::<K>())?.as_ref();
        any.downcast_ref::<KeyMap<K>>()
            .expect("a store map is always stored under the TypeId of its own key type")
            .get(key)
            .cloned()
    }

    /// Store `value` under `key`, overwriting any value already there.
    pub(crate) fn insert<K: Key>(&mut self, key: K, value: Arc<K::Value>) {
        self.map_mut::<K>().insert(key, value);
    }

    /// Copy every entry of `self` into `into`, overwriting any already
    /// there under the same key. Cost is proportional to the entries of
    /// `self`, not of `into`.
    pub(crate) fn merge_into(&self, into: &mut Store) {
        for map in self.maps.values() {
            map.merge_into(into);
        }
    }

    /// Move every entry of `self` into `into`, overwriting any already
    /// there under the same key. Like [`merge_into`](Self::merge_into)
    /// but without cloning keys or bumping refcounts, for the last use of
    /// a store.
    pub(crate) fn move_into(self, into: &mut Store) {
        for (type_id, map) in self.maps {
            match into.maps.entry(type_id) {
                Entry::Vacant(slot) => {
                    slot.insert(map);
                }
                Entry::Occupied(_) => map.move_into(into),
            }
        }
    }

    /// Whether the store holds no entries at all.
    pub(crate) fn is_empty(&self) -> bool {
        self.maps.is_empty()
    }

    fn map_mut<K: Key>(&mut self) -> &mut KeyMap<K> {
        let any: &mut dyn Any = self
            .maps
            .entry(TypeId::of::<K>())
            .or_insert_with(|| Box::new(KeyMap::<K>::new()))
            .as_mut();
        any.downcast_mut::<KeyMap<K>>()
            .expect("a store map is always stored under the TypeId of its own key type")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Hash, Eq, PartialEq, Clone, Debug)]
    struct Id(u32);

    impl Key for Id {
        type Value = String;
    }

    #[derive(Hash, Eq, PartialEq, Clone, Debug)]
    struct Other(u32);

    impl Key for Other {
        type Value = u64;
    }

    #[test]
    fn empty_until_the_first_insert() {
        let mut store = Store::default();
        assert!(store.is_empty());
        assert!(store.get(&Id(1)).is_none());
        store.insert(Id(1), Arc::new("one".into()));
        assert!(!store.is_empty());
        assert_eq!(*store.get(&Id(1)).unwrap(), "one");
    }

    #[test]
    fn insert_overwrites() {
        let mut store = Store::default();
        store.insert(Id(1), Arc::new("one".into()));
        store.insert(Id(1), Arc::new("uno".into()));
        assert_eq!(*store.get(&Id(1)).unwrap(), "uno");
    }

    #[test]
    fn merge_copies_every_key_type_and_overwrites_collisions() {
        let mut source = Store::default();
        let one = Arc::new("one".to_string());
        source.insert(Id(1), Arc::clone(&one));
        source.insert(Other(1), Arc::new(10));

        let mut target = Store::default();
        target.insert(Id(1), Arc::new("stale".into()));
        target.insert(Id(2), Arc::new("two".into()));

        source.merge_into(&mut target);

        // Collision overwritten, by the same Arc.
        assert!(Arc::ptr_eq(&target.get(&Id(1)).unwrap(), &one));
        // Untouched entries of the target survive.
        assert_eq!(*target.get(&Id(2)).unwrap(), "two");
        // A key type new to the target is created.
        assert_eq!(*target.get(&Other(1)).unwrap(), 10);
        // The source is unchanged and can be merged again.
        assert_eq!(*source.get(&Id(1)).unwrap(), "one");
        assert!(source.get(&Id(2)).is_none());
    }

    #[test]
    fn move_adopts_new_key_types_and_overwrites_collisions() {
        let mut source = Store::default();
        let one = Arc::new("one".to_string());
        source.insert(Id(1), Arc::clone(&one));
        let ten = Arc::new(10_u64);
        source.insert(Other(1), Arc::clone(&ten));

        let mut target = Store::default();
        target.insert(Id(1), Arc::new("stale".into()));
        target.insert(Id(2), Arc::new("two".into()));

        source.move_into(&mut target);

        assert!(Arc::ptr_eq(&target.get(&Id(1)).unwrap(), &one));
        assert_eq!(*target.get(&Id(2)).unwrap(), "two");
        assert!(Arc::ptr_eq(&target.get(&Other(1)).unwrap(), &ten));
        // Moved, not copied: the target holds the only other reference.
        assert_eq!(Arc::strong_count(&one), 2);
        assert_eq!(Arc::strong_count(&ten), 2);
    }

    #[test]
    fn merging_an_empty_store_is_a_no_op() {
        let mut target = Store::default();
        target.insert(Id(1), Arc::new("one".into()));
        Store::default().merge_into(&mut target);
        assert_eq!(*target.get(&Id(1)).unwrap(), "one");
    }
}
