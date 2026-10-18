use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::hash::Hash;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex};

use crate::error::OccupiedError;

/// A key that identifies one resource in a [`Registry`]. The key type fixes
/// the value type, so a lookup by key can never see a value of the wrong
/// type.
///
/// Implement `Key` alone for resources that are inserted by hand, and
/// [`Query`](crate::Query) on top of it for resources the factory can build.
pub trait Key: Hash + Eq + Clone + Send + Sync + 'static {
    type Value: Send + Sync + 'static;
}

/// Private nil-key adapter: identifies "the singleton instance of `R`" as a
/// key carrying no information. Singletons share storage with keyed
/// resources via this adapter, so the registry has only one mechanism.
pub(crate) struct SingletonKey<R: ?Sized>(PhantomData<fn() -> R>);

impl<R: ?Sized> SingletonKey<R> {
    pub(crate) fn new() -> Self {
        Self(PhantomData)
    }
}

impl<R: ?Sized> Clone for SingletonKey<R> {
    fn clone(&self) -> Self {
        Self(PhantomData)
    }
}

impl<R: ?Sized> PartialEq for SingletonKey<R> {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl<R: ?Sized> Eq for SingletonKey<R> {}

impl<R: ?Sized> Hash for SingletonKey<R> {
    fn hash<H: std::hash::Hasher>(&self, _state: &mut H) {}
}

impl<R: Send + Sync + 'static> Key for SingletonKey<R> {
    type Value = R;
}

type KeyMap<K> = HashMap<K, Arc<<K as Key>::Value>>;

/// A thread-safe, type-keyed map of `Arc`-shared resources.
///
/// A registry is a plain store: it knows nothing about building. The
/// [`DependencyFactory`](crate::DependencyFactory) is one client of it,
/// committing the resources it builds through
/// [`get_or_insert_with`](Self::get_or_insert_with); any other code can
/// populate the same registry by hand on equal footing.
///
/// Two kinds of entry share one mechanism: singletons, identified by their
/// type alone (`insert`, `get`, ...), and keyed resources, identified by a
/// [`Key`] value (`insert_for`, `get_for`, ...).
///
/// Writes come in three flavours. `insert` fills an empty slot and refuses
/// an occupied one; `get_or_insert_with` fills an empty slot and otherwise
/// returns what is there; `replace` overwrites. Replacing does not affect
/// anyone already holding the previous `Arc`.
#[derive(Default)]
pub struct Registry {
    /// Outer key: `TypeId` of the key type `K`. Stored value: a boxed
    /// `KeyMap<K>`, type-erased so that maps for different key types can
    /// live in one outer map. The inner map is recovered by downcasting on
    /// lookup; because `K` fixes its value type, the downcast cannot fail.
    by_key_type: Mutex<HashMap<TypeId, Box<dyn Any + Send + Sync>>>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the singleton instance of `R`, if one is present.
    pub fn get<R: Send + Sync + 'static>(&self) -> Option<Arc<R>> {
        self.get_for(&SingletonKey::<R>::new())
    }

    /// Store `value` as the singleton instance of `R`, if there is none
    /// yet. Returns the cached `Arc<R>`, or the rejected `value` wrapped in
    /// an [`OccupiedError`] if an instance was already present.
    pub fn insert<R: Send + Sync + 'static>(&self, value: R) -> Result<Arc<R>, OccupiedError<R>> {
        self.insert_for(SingletonKey::<R>::new(), value)
    }

    /// Store `value` as the singleton instance of `R`, overwriting any
    /// instance already present. Returns the new `Arc<R>`.
    pub fn replace<R: Send + Sync + 'static>(&self, value: R) -> Arc<R> {
        self.replace_for(SingletonKey::<R>::new(), value)
    }

    /// Return the singleton instance of `R`, storing the result of `make`
    /// first if there is none. `make` is not called when an instance is
    /// already present.
    pub fn get_or_insert_with<R: Send + Sync + 'static>(&self, make: impl FnOnce() -> R) -> Arc<R> {
        self.get_or_insert_with_for(SingletonKey::<R>::new(), make)
    }

    /// Return the resource stored under `key`, if one is present.
    pub fn get_for<K: Key>(&self, key: &K) -> Option<Arc<K::Value>> {
        let by_key_type = self.by_key_type.lock().unwrap();
        let map = by_key_type.get(&TypeId::of::<K>())?;
        downcast::<K>(map).get(key).cloned()
    }

    /// Store `value` under `key`, if the slot is empty. Returns the cached
    /// `Arc`, or the rejected `value` wrapped in an [`OccupiedError`] if
    /// the slot was already filled.
    pub fn insert_for<K: Key>(
        &self,
        key: K,
        value: K::Value,
    ) -> Result<Arc<K::Value>, OccupiedError<K::Value>> {
        let mut by_key_type = self.by_key_type.lock().unwrap();
        match map_mut::<K>(&mut by_key_type).entry(key) {
            std::collections::hash_map::Entry::Occupied(_) => Err(OccupiedError::new(value)),
            std::collections::hash_map::Entry::Vacant(slot) => {
                Ok(slot.insert(Arc::new(value)).clone())
            }
        }
    }

    /// Store `value` under `key`, overwriting any resource already there.
    /// Returns the new `Arc`.
    pub fn replace_for<K: Key>(&self, key: K, value: K::Value) -> Arc<K::Value> {
        let arc = Arc::new(value);
        let mut by_key_type = self.by_key_type.lock().unwrap();
        map_mut::<K>(&mut by_key_type).insert(key, arc.clone());
        arc
    }

    /// Return the resource stored under `key`, storing the result of `make`
    /// first if the slot is empty. `make` is not called when the slot is
    /// already filled.
    pub fn get_or_insert_with_for<K: Key>(
        &self,
        key: K,
        make: impl FnOnce() -> K::Value,
    ) -> Arc<K::Value> {
        let mut by_key_type = self.by_key_type.lock().unwrap();
        map_mut::<K>(&mut by_key_type)
            .entry(key)
            .or_insert_with(|| Arc::new(make()))
            .clone()
    }
}

/// Takes the box rather than `&dyn Any` on purpose: passing a `&Box<dyn
/// Any>` to a `&dyn Any` parameter unsizes the box itself into the trait
/// object, and the downcast then fails.
fn downcast<K: Key>(map: &Box<dyn Any + Send + Sync>) -> &KeyMap<K> {
    (**map)
        .downcast_ref::<KeyMap<K>>()
        .expect("a registry map is always stored under the TypeId of its own key type")
}

fn map_mut<K: Key>(
    by_key_type: &mut HashMap<TypeId, Box<dyn Any + Send + Sync>>,
) -> &mut KeyMap<K> {
    by_key_type
        .entry(TypeId::of::<K>())
        .or_insert_with(|| Box::new(KeyMap::<K>::new()))
        .downcast_mut::<KeyMap<K>>()
        .expect("a registry map is always stored under the TypeId of its own key type")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn insert_and_get_singleton() {
        let registry = Registry::new();
        let inserted = registry.insert(42_u32).unwrap();
        assert_eq!(*inserted, 42);
        let got = registry.get::<u32>().unwrap();
        assert!(Arc::ptr_eq(&inserted, &got));
    }

    #[test]
    fn get_returns_none_when_absent() {
        let registry = Registry::new();
        assert!(registry.get::<u32>().is_none());
    }

    #[test]
    fn singletons_are_keyed_by_type() {
        let registry = Registry::new();
        registry.insert(1_u32).unwrap();
        registry.insert(2_u64).unwrap();
        assert_eq!(*registry.get::<u32>().unwrap(), 1);
        assert_eq!(*registry.get::<u64>().unwrap(), 2);
    }

    #[test]
    fn insert_refuses_an_occupied_slot() {
        let registry = Registry::new();
        registry.insert(1_u32).unwrap();
        let err = registry.insert(2_u32).unwrap_err();
        assert_eq!(err.into_value(), 2);
        assert_eq!(*registry.get::<u32>().unwrap(), 1);
    }

    #[test]
    fn replace_overwrites_an_occupied_slot() {
        let registry = Registry::new();
        let first = registry.insert(1_u32).unwrap();
        let second = registry.replace(2_u32);
        assert_eq!(*registry.get::<u32>().unwrap(), 2);
        // The previous Arc is unaffected for anyone still holding it.
        assert_eq!(*first, 1);
        assert!(Arc::ptr_eq(&second, &registry.get::<u32>().unwrap()));
    }

    #[test]
    fn get_or_insert_with_fills_an_empty_slot_only() {
        let registry = Registry::new();
        let made = registry.get_or_insert_with(|| 1_u32);
        assert_eq!(*made, 1);
        let kept = registry.get_or_insert_with(|| -> u32 { panic!("must not be called") });
        assert!(Arc::ptr_eq(&made, &kept));
    }

    #[derive(Hash, Eq, PartialEq, Clone, Debug)]
    struct StringKey(u32);

    impl Key for StringKey {
        type Value = String;
    }

    #[test]
    fn insert_for_and_get_for_round_trip() {
        let registry = Registry::new();
        let inserted = registry
            .insert_for(StringKey(7), "seven".to_string())
            .unwrap();
        assert_eq!(*inserted, "seven");
        let got = registry.get_for(&StringKey(7)).unwrap();
        assert!(Arc::ptr_eq(&inserted, &got));
    }

    #[test]
    fn get_for_returns_none_when_absent() {
        let registry = Registry::new();
        assert!(registry.get_for(&StringKey(7)).is_none());
    }

    #[test]
    fn keyed_entries_with_different_keys_are_independent() {
        let registry = Registry::new();
        registry.insert_for(StringKey(1), "one".into()).unwrap();
        registry.insert_for(StringKey(2), "two".into()).unwrap();
        assert_eq!(*registry.get_for(&StringKey(1)).unwrap(), "one");
        assert_eq!(*registry.get_for(&StringKey(2)).unwrap(), "two");
    }

    #[test]
    fn insert_for_refuses_an_occupied_slot() {
        let registry = Registry::new();
        registry.insert_for(StringKey(1), "first".into()).unwrap();
        let err = registry
            .insert_for(StringKey(1), "second".into())
            .unwrap_err();
        assert_eq!(err.into_value(), "second");
        assert_eq!(*registry.get_for(&StringKey(1)).unwrap(), "first");
    }

    #[test]
    fn replace_for_overwrites_an_occupied_slot() {
        let registry = Registry::new();
        registry.insert_for(StringKey(1), "first".into()).unwrap();
        registry.replace_for(StringKey(1), "second".into());
        assert_eq!(*registry.get_for(&StringKey(1)).unwrap(), "second");
    }

    #[test]
    fn get_or_insert_with_for_fills_an_empty_slot_only() {
        let registry = Registry::new();
        let made = registry.get_or_insert_with_for(StringKey(3), || "three".into());
        let kept = registry.get_or_insert_with_for(StringKey(3), || panic!("must not be called"));
        assert!(Arc::ptr_eq(&made, &kept));
    }

    #[test]
    fn occupied_error_is_debug_without_a_debug_value() {
        struct Opaque;
        let registry = Registry::new();
        registry.insert(Opaque).unwrap();
        let err = registry.insert(Opaque).err().expect("slot is occupied");
        let rendered = format!("{err:?}");
        assert!(rendered.contains("Opaque"), "{rendered}");
    }
}
