//! The maps under a [`Registry`](crate::Registry): type-keyed, type-erased,
//! and nothing more. Locking and sharing live in `registry`. A `Store` is
//! plain data, so the registry's two buffers and a transaction's overlay
//! are all the same type and merge into one another with one method.
//!
//! A store holds entries, `Arc`-shared values under [`Key`]s, and indexes,
//! lists of `Arc<T>` for some `T` that is usually a trait object. An index
//! is a single value as far as merging goes: a store's copy of an index is
//! the whole list, and merging it into another store replaces that store's
//! copy.

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
    /// Whether an entry holds an `Arc` to the allocation at `ptr`, not
    /// counting entries whose key `shadow`, a map for the same key type,
    /// also has.
    fn holds(&self, ptr: *const (), shadow: Option<&dyn AnyMap>) -> bool;
}

impl<K: Key> AnyMap for KeyMap<K> {
    fn merge_into(&self, into: &mut Store) {
        into.map_mut::<K>()
            .extend(self.iter().map(|(k, v)| (k.clone(), Arc::clone(v))));
    }

    fn move_into(self: Box<Self>, into: &mut Store) {
        into.map_mut::<K>().extend(*self);
    }

    fn holds(&self, ptr: *const (), shadow: Option<&dyn AnyMap>) -> bool {
        let shadow = shadow.map(|shadow| {
            let any: &dyn Any = shadow;
            any.downcast_ref::<KeyMap<K>>()
                .expect("a store map is always stored under the TypeId of its own key type")
        });
        self.iter().any(|(key, value)| {
            data_ptr(value) == ptr && !shadow.is_some_and(|shadow| shadow.contains_key(key))
        })
    }
}

/// A `Vec<Arc<T>>` behind a type-erased box, for the indexes of a
/// [`Store`]: `Any` so lookups can downcast it, and enough else to be
/// copied between stores and have one `Arc` removed without the caller
/// knowing `T`.
trait AnyIndex: Any + Send + Sync {
    /// A copy of the list. Entries are `Arc`s, so each is a refcount bump.
    fn clone_box(&self) -> Box<dyn AnyIndex>;
    /// Whether the list holds an `Arc` to the allocation at `ptr`.
    fn contains(&self, ptr: *const ()) -> bool;
    /// Remove every `Arc` to the allocation at `ptr`.
    fn remove(&mut self, ptr: *const ());
}

impl<T: ?Sized + Send + Sync + 'static> AnyIndex for Vec<Arc<T>> {
    fn clone_box(&self) -> Box<dyn AnyIndex> {
        Box::new(self.clone())
    }

    fn contains(&self, ptr: *const ()) -> bool {
        self.iter().any(|arc| data_ptr(arc) == ptr)
    }

    fn remove(&mut self, ptr: *const ()) {
        self.retain(|arc| data_ptr(arc) != ptr);
    }
}

/// The address of the value an `Arc` points to, without its metadata, so
/// that an `Arc<T>` and an `Arc<dyn Trait>` coerced from it compare equal.
pub(crate) fn data_ptr<T: ?Sized>(arc: &Arc<T>) -> *const () {
    Arc::as_ptr(arc) as *const ()
}

/// A map from [`Key`] values to `Arc`-shared values, for any number of
/// key types at once, plus an index for any number of `T`s.
///
/// Entries: outer key is the `TypeId` of the key type `K`, stored value a
/// boxed `KeyMap<K>`, type-erased so that maps for different key types can
/// live in one outer map. Indexes: outer key is the `TypeId` of `T`,
/// stored value a boxed `Vec<Arc<T>>`. Each is recovered by downcasting on
/// lookup; because `K` fixes its value type and `T` fixes its list type,
/// the downcast cannot fail.
///
/// An inner map is created only by [`insert`](Self::insert), which always
/// adds an entry, and an index only by [`index`](Self::index), which always
/// adds to it, so a store with no inner maps and no indexes holds nothing.
/// Nothing removes an entry; [`unindex`](Self::unindex) can empty an
/// index, which then stays as an empty list.
#[derive(Default)]
pub(crate) struct Store {
    maps: HashMap<TypeId, Box<dyn AnyMap>>,
    indexes: HashMap<TypeId, Box<dyn AnyIndex>>,
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

    /// The index for `T`, if this store holds one.
    pub(crate) fn indexed<T: ?Sized + Send + Sync + 'static>(&self) -> Option<&Vec<Arc<T>>> {
        let any: &dyn Any = self.indexes.get(&TypeId::of::<T>())?.as_ref();
        Some(
            any.downcast_ref::<Vec<Arc<T>>>()
                .expect("a store index is always stored under the TypeId of its own item type"),
        )
    }

    /// Add `item` to the end of the index for `T`, creating the index if
    /// there is none.
    pub(crate) fn index<T: ?Sized + Send + Sync + 'static>(&mut self, item: Arc<T>) {
        let any: &mut dyn Any = self
            .indexes
            .entry(TypeId::of::<T>())
            .or_insert_with(|| Box::new(Vec::<Arc<T>>::new()))
            .as_mut();
        any.downcast_mut::<Vec<Arc<T>>>()
            .expect("a store index is always stored under the TypeId of its own item type")
            .push(item);
    }

    /// The `TypeId`s of every index this store holds.
    pub(crate) fn index_ids(&self) -> impl Iterator<Item = TypeId> + '_ {
        self.indexes.keys().copied()
    }

    /// Whether the index with `TypeId` `id` holds an `Arc` to `ptr`.
    pub(crate) fn index_contains(&self, id: TypeId, ptr: *const ()) -> bool {
        self.indexes
            .get(&id)
            .is_some_and(|index| index.contains(ptr))
    }

    /// Copy the index with `TypeId` `id` from `from` into this store,
    /// unless this store already has one or `from` has none. For an
    /// overlay about to change an index it has not touched yet.
    pub(crate) fn adopt_index(&mut self, id: TypeId, from: &Store) {
        if let (Entry::Vacant(slot), Some(index)) = (self.indexes.entry(id), from.indexes.get(&id))
        {
            slot.insert(index.clone_box());
        }
    }

    /// Whether an entry of this store, or one of `under` that no entry of
    /// this store shadows under the same key, holds an `Arc` to the
    /// allocation at `ptr`. For a transaction's overlay over the buffer
    /// it will commit into. Costs a pass over the entries.
    pub(crate) fn holds(&self, ptr: *const (), under: &Store) -> bool {
        self.maps.values().any(|map| map.holds(ptr, None))
            || under
                .maps
                .iter()
                .any(|(id, map)| map.holds(ptr, self.maps.get(id).map(Box::as_ref)))
    }

    /// Remove every `Arc` to `ptr` from every index in this store.
    pub(crate) fn unindex(&mut self, ptr: *const ()) {
        for index in self.indexes.values_mut() {
            index.remove(ptr);
        }
    }

    /// Copy every entry and index of `self` into `into`, overwriting any
    /// entry already there under the same key and replacing any index for
    /// the same `T`. Cost is proportional to the contents of `self`, not of
    /// `into`.
    pub(crate) fn merge_into(&self, into: &mut Store) {
        for map in self.maps.values() {
            map.merge_into(into);
        }
        for (type_id, index) in &self.indexes {
            into.indexes.insert(*type_id, index.clone_box());
        }
    }

    /// Move every entry and index of `self` into `into`, overwriting any
    /// entry already there under the same key and replacing any index for
    /// the same `T`. Like [`merge_into`](Self::merge_into) but without
    /// cloning keys or bumping refcounts, for the last use of a store.
    pub(crate) fn move_into(self, into: &mut Store) {
        for (type_id, map) in self.maps {
            match into.maps.entry(type_id) {
                Entry::Vacant(slot) => {
                    slot.insert(map);
                }
                Entry::Occupied(_) => map.move_into(into),
            }
        }
        into.indexes.extend(self.indexes);
    }

    /// Whether the store holds no entries and no indexes at all.
    pub(crate) fn is_empty(&self) -> bool {
        self.maps.is_empty() && self.indexes.is_empty()
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

    trait Named: Send + Sync {
        fn name(&self) -> &str;
    }

    impl Named for String {
        fn name(&self) -> &str {
            self
        }
    }

    fn names(index: Option<&Vec<Arc<dyn Named>>>) -> Vec<String> {
        index
            .map(|index| index.iter().map(|n| n.name().to_string()).collect())
            .unwrap_or_default()
    }

    #[test]
    fn index_is_created_on_first_add_and_keeps_order() {
        let mut store = Store::default();
        assert!(store.indexed::<dyn Named>().is_none());
        store.index::<dyn Named>(Arc::new("one".to_string()));
        assert!(!store.is_empty());
        store.index::<dyn Named>(Arc::new("two".to_string()));
        assert_eq!(names(store.indexed::<dyn Named>()), ["one", "two"]);
        // An index for another type is independent.
        assert!(store.indexed::<String>().is_none());
    }

    #[test]
    fn merge_and_move_replace_an_index_wholesale() {
        let one = Arc::new("one".to_string());
        let mut source = Store::default();
        source.index::<dyn Named>(Arc::clone(&one) as Arc<dyn Named>);
        source.index::<dyn Named>(Arc::new("two".to_string()));

        let mut target = Store::default();
        target.index::<dyn Named>(Arc::new("stale".to_string()));
        target.index::<String>(Arc::new("kept".to_string()));

        source.merge_into(&mut target);
        assert_eq!(names(target.indexed::<dyn Named>()), ["one", "two"]);
        assert_eq!(target.indexed::<String>().unwrap().len(), 1);
        // The source is unchanged and can be merged again.
        assert_eq!(names(source.indexed::<dyn Named>()), ["one", "two"]);
        drop(target);

        let mut target = Store::default();
        target.index::<dyn Named>(Arc::new("stale".to_string()));
        source.move_into(&mut target);
        assert_eq!(names(target.indexed::<dyn Named>()), ["one", "two"]);
        // Moved, not copied: `one` is held by its binding and the target only.
        assert_eq!(Arc::strong_count(&one), 2);
    }

    #[test]
    fn unindex_removes_by_pointer_from_every_index() {
        let one = Arc::new("one".to_string());
        let mut store = Store::default();
        store.index::<dyn Named>(Arc::clone(&one) as Arc<dyn Named>);
        store.index::<dyn Named>(Arc::new("two".to_string()));
        store.index::<String>(Arc::clone(&one));
        // An equal but distinct value is not the same allocation.
        store.index::<String>(Arc::new("one".to_string()));

        let named = TypeId::of::<dyn Named>();
        assert!(store.index_contains(named, data_ptr(&one)));
        assert!(!store.index_contains(named, data_ptr(&Arc::new(0_u8))));
        assert!(!store.index_contains(TypeId::of::<u8>(), data_ptr(&one)));
        assert_eq!(store.index_ids().count(), 2);

        store.unindex(data_ptr(&one));
        assert_eq!(names(store.indexed::<dyn Named>()), ["two"]);
        assert_eq!(store.indexed::<String>().unwrap().len(), 1);
        assert!(!Arc::ptr_eq(&store.indexed::<String>().unwrap()[0], &one));
        // An emptied index stays, empty.
        store.unindex(data_ptr(&store.indexed::<String>().unwrap()[0].clone()));
        assert!(store.indexed::<String>().unwrap().is_empty());
        assert!(!store.is_empty());
    }

    #[test]
    fn adopt_index_copies_only_what_is_missing() {
        let mut from = Store::default();
        from.index::<dyn Named>(Arc::new("one".to_string()));
        let named = TypeId::of::<dyn Named>();

        let mut store = Store::default();
        store.adopt_index(TypeId::of::<u8>(), &from);
        assert!(store.is_empty(), "nothing to adopt for a type `from` lacks");
        store.adopt_index(named, &from);
        assert_eq!(names(store.indexed::<dyn Named>()), ["one"]);

        store.index::<dyn Named>(Arc::new("two".to_string()));
        store.adopt_index(named, &from);
        assert_eq!(
            names(store.indexed::<dyn Named>()),
            ["one", "two"],
            "an index already here is left alone"
        );
    }

    #[test]
    fn merging_an_empty_store_is_a_no_op() {
        let mut target = Store::default();
        target.insert(Id(1), Arc::new("one".into()));
        Store::default().merge_into(&mut target);
        assert_eq!(*target.get(&Id(1)).unwrap(), "one");
    }
}
