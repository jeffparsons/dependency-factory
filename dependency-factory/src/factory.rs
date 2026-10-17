use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::{Arc, OnceLock, Weak};

use crate::error::{BuildError, CycleError};

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

/// The cached values. Mutated only through `&mut DependencyFactory` while
/// assembling, then moved into an `Arc` by [`DependencyFactory::freeze`]
/// and read-only from then on, so it needs no lock.
#[derive(Default)]
struct Storage {
    /// Outer key: `TypeId` of the query key type `Q`.
    /// Stored value: a boxed `KeyMap<Q, V>` — a `HashMap<Q, Arc<V>>` —
    /// type-erased so that maps for different query types can live in one
    /// outer map. The inner map is recovered by downcasting on lookup.
    by_query_type: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
}

impl Storage {
    fn map_mut<Q, V>(&mut self) -> &mut KeyMap<Q, V>
    where
        Q: Hash + Eq + Clone + Send + Sync + 'static,
        V: Send + Sync + 'static,
    {
        self.by_query_type
            .entry(TypeId::of::<Q>())
            .or_insert_with(|| Box::new(KeyMap::<Q, V>::new()))
            .downcast_mut::<KeyMap<Q, V>>()
            .expect("stored map had a different value type than expected for this query type")
    }

    fn insert_keyed<Q, V>(&mut self, key: Q, value: V) -> Arc<V>
    where
        Q: Hash + Eq + Clone + Send + Sync + 'static,
        V: Send + Sync + 'static,
    {
        let arc = Arc::new(value);
        self.map_mut::<Q, V>().insert(key, arc.clone());
        arc
    }

    /// Insert `value` under `key` only if the slot is currently empty,
    /// returning whichever `Arc` ends up cached. Used by the build path so
    /// that an `insert` made from inside `build` for the key being built
    /// takes precedence over the build's own result.
    fn insert_if_absent<Q, V>(&mut self, key: Q, value: V) -> Arc<V>
    where
        Q: Hash + Eq + Clone + Send + Sync + 'static,
        V: Send + Sync + 'static,
    {
        self.map_mut::<Q, V>()
            .entry(key)
            .or_insert_with(|| Arc::new(value))
            .clone()
    }

    fn get_keyed<Q, V>(&self, key: &Q) -> Option<Arc<V>>
    where
        Q: Hash + Eq + Clone + Send + Sync + 'static,
        V: Send + Sync + 'static,
    {
        let entry = self.by_query_type.get(&TypeId::of::<Q>())?;
        let map = entry
            .downcast_ref::<KeyMap<Q, V>>()
            .expect("stored map had a different value type than expected for this query type");
        map.get(key).cloned()
    }
}

/// One entry on the factory's "currently building" breadcrumb. Carries a
/// hashed key value so different keys of the same query type are
/// distinguished.
#[derive(Clone, Copy)]
struct Frame {
    type_id: TypeId,
    key_hash: u64,
    type_name: &'static str,
}

impl Frame {
    fn singleton<R: 'static>() -> Self {
        Self {
            type_id: TypeId::of::<SingletonKey<R>>(),
            key_hash: 0,
            type_name: std::any::type_name::<R>(),
        }
    }

    fn query<Q: Query>(key: &Q) -> Self {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        Self {
            type_id: TypeId::of::<Q>(),
            key_hash: hasher.finish(),
            type_name: std::any::type_name::<Q::Output>(),
        }
    }
}

/// The factory while its dependency graph is being assembled. Every
/// operation that can add to the graph takes `&mut self`, so assembly is
/// single-threaded by construction: a `build` runs at most once per key,
/// and the only cycles are same-thread cycles, which are reported as
/// [`CycleError`]s.
///
/// Call [`freeze`](Self::freeze) once assembly is done to get a
/// [`FrozenFactory`] that can be shared across threads and read through
/// [`DependencyFactoryHandle`]s.
#[derive(Default)]
pub struct DependencyFactory {
    storage: Storage,
    /// Keys whose `build` is on the call stack, outermost first.
    in_progress: Vec<Frame>,
    /// Filled by `freeze`. Handles created during assembly point here and
    /// become usable once it is set.
    shared: Arc<OnceLock<Storage>>,
}

impl DependencyFactory {
    pub fn new() -> Self {
        Self::default()
    }

    /// A cheap-to-clone handle to this factory. Internally weak, so
    /// storing a handle inside a resource cannot keep the factory alive
    /// on its own and cannot create a reference cycle.
    ///
    /// A handle taken during assembly is dormant: its lookups return
    /// [`BuildError::factory_not_frozen`] until [`freeze`](Self::freeze)
    /// is called, after which they see the frozen contents.
    pub fn handle(&self) -> DependencyFactoryHandle {
        DependencyFactoryHandle {
            shared: Arc::downgrade(&self.shared),
        }
    }

    /// Pre-populate the singleton slot for type `R`. Overwrites any
    /// existing entry for `R`. Returns the cached `Arc<R>`.
    pub fn insert<R: Send + Sync + 'static>(&mut self, value: R) -> Arc<R> {
        self.storage.insert_keyed(SingletonKey::<R>::new(), value)
    }

    /// Return the cached singleton instance of `R`, if one is present.
    pub fn get<R: Send + Sync + 'static>(&self) -> Option<Arc<R>> {
        self.storage
            .get_keyed::<SingletonKey<R>, R>(&SingletonKey::<R>::new())
    }

    /// Resolve the singleton instance of `R`, building it if it is not
    /// already cached. `R::build` receives this same factory, so it can
    /// resolve its own dependencies recursively.
    pub fn build<R: Singleton>(&mut self) -> Result<Arc<R>, BuildError> {
        if let Some(arc) = self.get::<R>() {
            return Ok(arc);
        }
        let value = self.run_build(Frame::singleton::<R>(), R::build)?;
        Ok(self
            .storage
            .insert_if_absent(SingletonKey::<R>::new(), value))
    }

    /// Pre-populate the cached output for `query`. Overwrites any existing
    /// entry for the same key. Returns the cached `Arc<Q::Output>`.
    pub fn insert_for<Q: Query>(&mut self, query: Q, value: Q::Output) -> Arc<Q::Output> {
        self.storage.insert_keyed(query, value)
    }

    /// Return the cached output for `query`, if one is present.
    pub fn get_for<Q: Query>(&self, query: Q) -> Option<Arc<Q::Output>> {
        self.storage.get_keyed::<Q, Q::Output>(&query)
    }

    /// Resolve the output for `query`, building it via `Q::build` if it is
    /// not already cached.
    pub fn build_for<Q: Query>(&mut self, query: Q) -> Result<Arc<Q::Output>, BuildError> {
        if let Some(arc) = self.get_for(query.clone()) {
            return Ok(arc);
        }
        let value = self.run_build(Frame::query(&query), |factory| query.build(factory))?;
        Ok(self.storage.insert_if_absent(query, value))
    }

    /// Finish assembly. The cached values move into shared, read-only
    /// storage; every handle taken from this factory starts resolving
    /// against it.
    pub fn freeze(self) -> FrozenFactory {
        // `shared` is set only here, and `self` is consumed, so this
        // cannot already be set.
        let _ = self.shared.set(self.storage);
        FrozenFactory {
            shared: self.shared,
        }
    }

    /// Run a user `build` for `frame` with the breadcrumb maintained around
    /// it: a cycle is reported before the build starts, and the frame is
    /// popped again whether the build returns or unwinds.
    fn run_build<T>(
        &mut self,
        frame: Frame,
        build: impl FnOnce(&mut Self) -> Result<T, BuildError>,
    ) -> Result<T, BuildError> {
        if let Some(start) = self
            .in_progress
            .iter()
            .position(|f| f.type_id == frame.type_id && f.key_hash == frame.key_hash)
        {
            let mut path: Vec<&'static str> = self.in_progress[start..]
                .iter()
                .map(|f| f.type_name)
                .collect();
            path.push(frame.type_name);
            return Err(CycleError::new(path).into());
        }
        let depth = self.in_progress.len();
        self.in_progress.push(frame);
        let result = catch_unwind(AssertUnwindSafe(|| build(self)));
        self.in_progress.truncate(depth);
        match result {
            Ok(result) => result.map_err(|e| e.push_frame(frame.type_name)),
            Err(payload) => resume_unwind(payload),
        }
    }
}

/// A fully assembled factory. Owns the cached values; dropping it tears
/// the factory down. It is `Send + Sync` and read-only, so it can be
/// shared freely across threads, and [`DependencyFactoryHandle`]s taken
/// before or after freezing resolve against it.
pub struct FrozenFactory {
    shared: Arc<OnceLock<Storage>>,
}

impl FrozenFactory {
    fn storage(&self) -> &Storage {
        self.shared
            .get()
            .expect("FrozenFactory is only constructed by freeze, which sets the storage")
    }

    /// A cheap-to-clone handle to this factory. See
    /// [`DependencyFactory::handle`].
    pub fn handle(&self) -> DependencyFactoryHandle {
        DependencyFactoryHandle {
            shared: Arc::downgrade(&self.shared),
        }
    }

    /// Return the cached singleton instance of `R`, if one is present.
    pub fn get<R: Send + Sync + 'static>(&self) -> Option<Arc<R>> {
        self.storage()
            .get_keyed::<SingletonKey<R>, R>(&SingletonKey::<R>::new())
    }

    /// Return the cached output for `query`, if one is present.
    pub fn get_for<Q: Query>(&self, query: Q) -> Option<Arc<Q::Output>> {
        self.storage().get_keyed::<Q, Q::Output>(&query)
    }
}

/// A cheap-to-clone handle to a factory. Holds a weak reference
/// internally, so storing one inside a resource is always safe and never
/// creates a reference cycle.
///
/// A handle only reads. It resolves against the [`FrozenFactory`] produced
/// by [`DependencyFactory::freeze`]: before that, operations return
/// [`BuildError::factory_not_frozen`], and once the `FrozenFactory` has
/// been dropped they return [`BuildError::factory_dropped`].
#[derive(Clone)]
pub struct DependencyFactoryHandle {
    shared: Weak<OnceLock<Storage>>,
}

impl DependencyFactoryHandle {
    fn with_storage<T>(&self, f: impl FnOnce(&Storage) -> T) -> Result<T, BuildError> {
        let shared = self
            .shared
            .upgrade()
            .ok_or_else(BuildError::factory_dropped)?;
        let storage = shared.get().ok_or_else(BuildError::factory_not_frozen)?;
        Ok(f(storage))
    }

    /// Return the cached singleton instance of `R`, if one is present.
    /// `Ok(None)` means the factory is frozen but no instance is cached;
    /// `Err(_)` means the factory is not frozen yet or has been dropped.
    pub fn get<R: Send + Sync + 'static>(&self) -> Result<Option<Arc<R>>, BuildError> {
        self.with_storage(|storage| {
            storage.get_keyed::<SingletonKey<R>, R>(&SingletonKey::<R>::new())
        })
    }

    /// Return the cached output for `query`, if one is present.
    /// `Ok(None)` means the factory is frozen but no output is cached for
    /// this key; `Err(_)` means the factory is not frozen yet or has been
    /// dropped.
    pub fn get_for<Q: Query>(&self, query: Q) -> Result<Option<Arc<Q::Output>>, BuildError> {
        self.with_storage(|storage| storage.get_keyed::<Q, Q::Output>(&query))
    }
}

/// A resource identified solely by its type. At most one instance per type
/// lives in a given factory. Implementors describe how to construct the
/// resource from the factory, which they may use to resolve transitive
/// dependencies.
///
/// `build` must be cheap and side-effect-free: no I/O, no blocking, no
/// contended locking. Expensive setup belongs in async lifecycle hooks
/// (added later) or in the resource's own methods, not here. A resource
/// that needs to look things up after assembly can store
/// [`DependencyFactory::handle`].
pub trait Singleton: Send + Sync + Sized + 'static {
    fn build(factory: &mut DependencyFactory) -> Result<Self, BuildError>;
}

/// A keyed resource: the implementor is the key value, and `Output` is the
/// resource produced for that key. Multiple instances per output type may
/// coexist, distinguished by the key value. Calling `build_for(key)` on the
/// factory caches the produced output under that key.
///
/// `build` must be cheap and side-effect-free: no I/O, no blocking, no
/// contended locking. Expensive setup belongs in async lifecycle hooks
/// (added later) or in the resource's own methods, not here.
pub trait Query: Hash + Eq + Clone + Send + Sync + 'static {
    type Output: Send + Sync + 'static;

    fn build(&self, factory: &mut DependencyFactory) -> Result<Self::Output, BuildError>;
}
