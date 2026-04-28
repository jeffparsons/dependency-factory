use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::hash::Hash;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex, Weak};

use crate::error::BuildError;

/// Private nil-query adapter: identifies "the singleton instance of `R`"
/// as a query key carrying no information. Singletons share storage with
/// keyed queries via this adapter, so the internal machinery has only one
/// mechanism.
struct SingletonKey<R: ?Sized>(PhantomData<fn() -> R>);

impl<R: ?Sized> SingletonKey<R> {
    fn new() -> Self {
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

type KeyMap<Q, V> = HashMap<Q, Arc<V>>;

struct Inner {
    /// Outer key: `TypeId` of the query key type `Q`.
    /// Stored value: a boxed `KeyMap<Q, V>` — a `HashMap<Q, Arc<V>>` —
    /// type-erased so that maps for different query types can live in one
    /// outer map. The inner map is recovered by downcasting on lookup.
    by_query_type: Mutex<HashMap<TypeId, Box<dyn Any + Send + Sync>>>,
}

impl Inner {
    fn new() -> Self {
        Self {
            by_query_type: Mutex::new(HashMap::new()),
        }
    }

    fn insert_keyed<Q, V>(&self, key: Q, value: V) -> Arc<V>
    where
        Q: Hash + Eq + Clone + Send + Sync + 'static,
        V: Send + Sync + 'static,
    {
        let arc = Arc::new(value);
        let mut by_qtype = self.by_query_type.lock().unwrap();
        let entry = by_qtype
            .entry(TypeId::of::<Q>())
            .or_insert_with(|| Box::new(KeyMap::<Q, V>::new()));
        let map = entry
            .downcast_mut::<KeyMap<Q, V>>()
            .expect("stored map had a different value type than expected for this query type");
        map.insert(key, arc.clone());
        arc
    }

    fn get_keyed<Q, V>(&self, key: &Q) -> Option<Arc<V>>
    where
        Q: Hash + Eq + Clone + Send + Sync + 'static,
        V: Send + Sync + 'static,
    {
        let by_qtype = self.by_query_type.lock().unwrap();
        let entry = by_qtype.get(&TypeId::of::<Q>())?;
        let map = entry
            .downcast_ref::<KeyMap<Q, V>>()
            .expect("stored map had a different value type than expected for this query type");
        map.get(key).cloned()
    }
}

/// The owning factory. Holds the storage alive; dropping it tears the
/// factory down. Most code should pass [`DependencyFactoryHandle`] around
/// instead, and reach for `DependencyFactory` only at the application's
/// top level where ownership lives.
pub struct DependencyFactory {
    inner: Arc<Inner>,
}

impl DependencyFactory {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner::new()),
        }
    }

    /// A cheap-to-clone handle to this factory. Internally weak, so
    /// storing a handle inside a resource cannot keep the factory alive
    /// on its own and cannot create a reference cycle.
    pub fn handle(&self) -> DependencyFactoryHandle {
        DependencyFactoryHandle {
            inner: Arc::downgrade(&self.inner),
        }
    }

    /// Pre-populate the singleton slot for type `R`. Overwrites any
    /// existing entry for `R`. Returns the cached `Arc<R>`.
    pub fn insert<R: Send + Sync + 'static>(&self, value: R) -> Arc<R> {
        self.inner.insert_keyed(SingletonKey::<R>::new(), value)
    }

    /// Return the cached singleton instance of `R`, if one is present.
    pub fn get<R: Send + Sync + 'static>(&self) -> Option<Arc<R>> {
        self.inner
            .get_keyed::<SingletonKey<R>, R>(&SingletonKey::<R>::new())
    }
}

impl Default for DependencyFactory {
    fn default() -> Self {
        Self::new()
    }
}

/// A cheap-to-clone handle to a [`DependencyFactory`]. Holds a weak
/// reference internally, so storing one inside a resource is always safe
/// and never creates a reference cycle. Operations return
/// [`BuildError::factory_dropped`] if the owning [`DependencyFactory`]
/// has been dropped.
#[derive(Clone)]
pub struct DependencyFactoryHandle {
    inner: Weak<Inner>,
}

impl DependencyFactoryHandle {
    fn upgrade(&self) -> Result<Arc<Inner>, BuildError> {
        self.inner.upgrade().ok_or_else(BuildError::factory_dropped)
    }

    /// Pre-populate the singleton slot for type `R`. Overwrites any
    /// existing entry for `R`. Returns the cached `Arc<R>`, or
    /// [`BuildError::factory_dropped`] if the owning factory is gone.
    pub fn insert<R: Send + Sync + 'static>(&self, value: R) -> Result<Arc<R>, BuildError> {
        let inner = self.upgrade()?;
        Ok(inner.insert_keyed(SingletonKey::<R>::new(), value))
    }

    /// Return the cached singleton instance of `R`, if one is present.
    /// `Ok(None)` means the factory is alive but no instance is cached;
    /// `Err(_)` means the factory itself has been dropped.
    pub fn get<R: Send + Sync + 'static>(&self) -> Result<Option<Arc<R>>, BuildError> {
        let inner = self.upgrade()?;
        Ok(inner.get_keyed::<SingletonKey<R>, R>(&SingletonKey::<R>::new()))
    }
}
