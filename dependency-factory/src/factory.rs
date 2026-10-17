use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::marker::PhantomData;
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError, Weak};
use std::thread::{self, ThreadId};

use once_cell::sync::OnceCell;

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

/// One cache slot. The cell is empty while the key has never been built,
/// or its last build failed, and is filled by the one thread whose `build`
/// succeeds or by an `insert`. Threads that ask for the key while another
/// thread is filling it park inside the cell. If that build fails or
/// panics the cell is left empty and the parked threads wake and race to
/// fill it themselves.
///
/// Slots are shared by `Arc` so that a thread can hold one across the
/// build with no factory lock held, and so that an `insert` can replace
/// the slot in the map without disturbing a build in progress on the old
/// one.
type Slot<V> = Arc<OnceCell<Arc<V>>>;

type KeyMap<Q, V> = HashMap<Q, Slot<V>>;

struct State {
    /// Outer key: `TypeId` of the query key type `Q`.
    /// Stored value: a boxed `KeyMap<Q, V>` — a `HashMap<Q, Slot<V>>` —
    /// type-erased so that maps for different query types can live in one
    /// outer map. The inner map is recovered by downcasting on lookup.
    by_query_type: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
}

impl State {
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

    fn map<Q, V>(&self) -> Option<&KeyMap<Q, V>>
    where
        Q: Hash + Eq + Clone + Send + Sync + 'static,
        V: Send + Sync + 'static,
    {
        let entry = self.by_query_type.get(&TypeId::of::<Q>())?;
        Some(
            entry
                .downcast_ref::<KeyMap<Q, V>>()
                .expect("stored map had a different value type than expected for this query type"),
        )
    }
}

struct Inner {
    state: Mutex<State>,
}

impl Inner {
    fn new() -> Self {
        Self {
            state: Mutex::new(State {
                by_query_type: HashMap::new(),
            }),
        }
    }

    /// The internal lock is never held across user code, so poisoning can
    /// only come from a bug in the factory itself. The state is still
    /// structurally valid in that case, so recover rather than cascade
    /// panics into every other thread.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn insert_keyed<Q, V>(&self, key: Q, value: V) -> Arc<V>
    where
        Q: Hash + Eq + Clone + Send + Sync + 'static,
        V: Send + Sync + 'static,
    {
        let arc = Arc::new(value);
        // A build for this key may be in progress on another slot. It
        // keeps that slot; the map now points at ours, and every caller
        // that finishes after this point re-reads the map and sees it.
        let slot: Slot<V> = Arc::new(OnceCell::with_value(arc.clone()));
        self.lock().map_mut::<Q, V>().insert(key, slot);
        arc
    }

    /// The cached value for `key`, if a finished one is present. A key
    /// whose build is in progress on another thread reads as absent.
    fn get_keyed<Q, V>(&self, key: &Q) -> Option<Arc<V>>
    where
        Q: Hash + Eq + Clone + Send + Sync + 'static,
        V: Send + Sync + 'static,
    {
        let state = self.lock();
        state.map::<Q, V>()?.get(key)?.get().cloned()
    }

    /// The slot for `key`, created empty if the key has never been seen.
    fn slot<Q, V>(&self, key: &Q) -> Slot<V>
    where
        Q: Hash + Eq + Clone + Send + Sync + 'static,
        V: Send + Sync + 'static,
    {
        let mut state = self.lock();
        state
            .map_mut::<Q, V>()
            .entry(key.clone())
            .or_default()
            .clone()
    }

    /// Resolve `key`, calling `build` at most once per cache miss across
    /// all threads. The slot's cell does the claiming and the waiting; the
    /// factory lock is only ever taken briefly to find the slot, and never
    /// while user code runs.
    fn build_keyed<Q, V>(
        &self,
        key: Q,
        frame: Frame,
        build: impl FnOnce() -> Result<V, BuildError>,
    ) -> Result<Arc<V>, BuildError>
    where
        Q: Hash + Eq + Clone + Send + Sync + 'static,
        V: Send + Sync + 'static,
    {
        if let Some(arc) = self.get_keyed::<Q, V>(&key) {
            return Ok(arc);
        }
        // Same-thread cycles are caught here, before anything else, with
        // the full path from the breadcrumb.
        let _breadcrumb = CycleGuard::enter(frame)?;
        let slot = self.slot::<Q, V>(&key);
        // Cross-thread cycles are caught here, before parking would turn
        // them into a deadlock.
        let _waiting = WaitingGuard::enter(&slot, frame.type_name)?;
        let built = slot
            .get_or_try_init(|| {
                let _owner = OwnerGuard::enter(&slot);
                build().map(Arc::new)
            })
            .map_err(|e| e.push_frame(frame.type_name))?
            .clone();
        // If an `insert` for this key landed meanwhile, the map holds its
        // slot rather than ours, and the inserted value wins.
        Ok(self.get_keyed::<Q, V>(&key).unwrap_or(built))
    }

    fn build_singleton<R: Singleton>(
        self: &Arc<Self>,
        handle: &DependencyFactoryHandle,
    ) -> Result<Arc<R>, BuildError> {
        self.build_keyed(
            SingletonKey::<R>::new(),
            Frame::singleton::<R>(self),
            || R::build(handle),
        )
    }

    fn build_query<Q: Query>(
        self: &Arc<Self>,
        query: Q,
        handle: &DependencyFactoryHandle,
    ) -> Result<Arc<Q::Output>, BuildError> {
        let frame = Frame::query::<Q>(self, &query);
        self.build_keyed(query.clone(), frame, || query.build(handle))
    }
}

/// Who is building which slot, and which thread is parked on which slot,
/// across every factory in the process. Together these describe the
/// wait-for graph between threads, which is what lets a thread about to
/// park notice that it would close a cycle.
///
/// Process-wide rather than per factory so that a cycle spanning two
/// factories is reported too. Its lock is never held across user code and
/// is never taken while a factory lock is held, so it cannot take part in
/// a lock-order cycle of its own.
struct Registry {
    /// Slots whose build is in progress, by slot identity, with the thread
    /// running the build.
    owners: HashMap<SlotId, ThreadId>,
    /// Threads parked, or about to park, on a slot. A thread that is
    /// building appears here only while a nested build has it waiting.
    waiting: HashMap<ThreadId, Waiting>,
}

/// Why a parked thread is parked: the slot it waits for, and the name of
/// the resource it wanted, for cycle reports.
struct Waiting {
    on: SlotId,
    wants: &'static str,
}

/// Address of a slot's cell. Every registry entry naming a slot is held by
/// a thread that also holds an `Arc` to that slot, so the address cannot
/// be reused while the entry exists.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct SlotId(usize);

impl SlotId {
    fn of<V>(slot: &Slot<V>) -> Self {
        Self(Arc::as_ptr(slot) as *const () as usize)
    }
}

static REGISTRY: LazyLock<Mutex<Registry>> = LazyLock::new(|| {
    Mutex::new(Registry {
        owners: HashMap::new(),
        waiting: HashMap::new(),
    })
});

fn registry() -> MutexGuard<'static, Registry> {
    REGISTRY.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Registry {
    /// If parking the current thread on `slot` would close a ring of
    /// threads each waiting on the next, return the names of the resources
    /// along that ring. `wants` is what the current thread is asking for.
    ///
    /// A slot with no registered owner ends the chain. Its builder may
    /// have won the cell and not yet registered, in which case that
    /// builder's own check, which runs after both registrations, is the
    /// one that reports the cycle.
    fn wait_cycle(
        &self,
        slot: SlotId,
        me: ThreadId,
        wants: &'static str,
    ) -> Option<Vec<&'static str>> {
        let mut path = vec![wants];
        let mut next = slot;
        // Each step consumes a distinct parked thread, so the chain is no
        // longer than the number of parked threads plus the first hop.
        for _ in 0..=self.waiting.len() {
            let owner = *self.owners.get(&next)?;
            if owner == me {
                path.push(wants);
                return Some(path);
            }
            let waiting = self.waiting.get(&owner)?;
            path.push(waiting.wants);
            next = waiting.on;
        }
        None
    }
}

/// Registers the current thread as waiting on a slot for as long as it is
/// inside that slot's `get_or_try_init`, and refuses to do so if waiting
/// would deadlock.
struct WaitingGuard(ThreadId);

impl WaitingGuard {
    fn enter<V>(slot: &Slot<V>, wants: &'static str) -> Result<Self, BuildError> {
        let me = thread::current().id();
        let on = SlotId::of(slot);
        let mut registry = registry();
        if let Some(path) = registry.wait_cycle(on, me, wants) {
            return Err(CycleError::new(path).into());
        }
        registry.waiting.insert(me, Waiting { on, wants });
        Ok(Self(me))
    }
}

impl Drop for WaitingGuard {
    fn drop(&mut self) {
        registry().waiting.remove(&self.0);
    }
}

/// Registers the current thread as the builder of a slot for as long as
/// the user's `build` runs. Dropped on success, error and unwind alike,
/// before the cell lets any waiter proceed.
struct OwnerGuard(SlotId);

impl OwnerGuard {
    fn enter<V>(slot: &Slot<V>) -> Self {
        let me = thread::current().id();
        let id = SlotId::of(slot);
        let mut registry = registry();
        registry.owners.insert(id, me);
        // We registered as waiting on this slot before the cell handed it
        // to us. We are building it now, not waiting on it.
        registry.waiting.remove(&me);
        Self(id)
    }
}

impl Drop for OwnerGuard {
    fn drop(&mut self) {
        registry().owners.remove(&self.0);
    }
}

thread_local! {
    static IN_PROGRESS: RefCell<Vec<Frame>> = const { RefCell::new(Vec::new()) };
}

/// One entry on the per-thread "currently building" breadcrumb. Carries a
/// factory identifier so frames produced by different factories on the
/// same call stack do not look like cycles, plus a hashed key value so
/// different keys of the same query type are distinguished.
#[derive(Clone, Copy)]
struct Frame {
    factory: usize,
    type_id: TypeId,
    key_hash: u64,
    type_name: &'static str,
}

impl Frame {
    fn singleton<R: 'static>(inner: &Arc<Inner>) -> Self {
        Self {
            factory: Arc::as_ptr(inner) as usize,
            type_id: TypeId::of::<SingletonKey<R>>(),
            key_hash: 0,
            type_name: std::any::type_name::<R>(),
        }
    }

    fn query<Q: Query>(inner: &Arc<Inner>, key: &Q) -> Self {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        Self {
            factory: Arc::as_ptr(inner) as usize,
            type_id: TypeId::of::<Q>(),
            key_hash: hasher.finish(),
            type_name: std::any::type_name::<Q::Output>(),
        }
    }
}

/// RAII guard around the thread-local breadcrumb. `enter` returns a
/// `BuildError` carrying a `CycleError` source if the same frame is
/// already on the stack; otherwise the new frame is pushed and popped
/// again on drop.
struct CycleGuard;

impl CycleGuard {
    fn enter(frame: Frame) -> Result<Self, BuildError> {
        IN_PROGRESS.with(|cell| -> Result<(), BuildError> {
            let mut breadcrumb = cell.borrow_mut();
            if let Some(start) = breadcrumb.iter().position(|f| {
                f.factory == frame.factory
                    && f.type_id == frame.type_id
                    && f.key_hash == frame.key_hash
            }) {
                let mut path: Vec<&'static str> =
                    breadcrumb[start..].iter().map(|f| f.type_name).collect();
                path.push(frame.type_name);
                return Err(CycleError::new(path).into());
            }
            breadcrumb.push(frame);
            Ok(())
        })?;
        Ok(CycleGuard)
    }
}

impl Drop for CycleGuard {
    fn drop(&mut self) {
        IN_PROGRESS.with(|cell| {
            cell.borrow_mut().pop();
        });
    }
}

/// The owning factory. Holds the storage alive; dropping it tears the
/// factory down. Most code should pass [`DependencyFactoryHandle`] around
/// instead, and reach for `DependencyFactory` only at the application's
/// top level where ownership lives.
///
/// For any one key, `build` runs at most once per cache miss, even when
/// several threads ask for the key at the same time: the first caller
/// builds and the others wait for its result. A cycle that spans threads
/// is reported as a [`CycleError`] rather than deadlocking.
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
    /// existing entry for `R`, and takes precedence over a build of `R`
    /// that is in progress on another thread. Returns the cached `Arc<R>`.
    pub fn insert<R: Send + Sync + 'static>(&self, value: R) -> Arc<R> {
        self.inner.insert_keyed(SingletonKey::<R>::new(), value)
    }

    /// Return the cached singleton instance of `R`, if one is present.
    /// Never waits: while `R` is being built on another thread this
    /// returns `None`.
    pub fn get<R: Send + Sync + 'static>(&self) -> Option<Arc<R>> {
        self.inner
            .get_keyed::<SingletonKey<R>, R>(&SingletonKey::<R>::new())
    }

    /// Resolve the singleton instance of `R`, building it if it is not
    /// already cached. Recursive `build` calls inside `R::build` see the
    /// same factory through a [`DependencyFactoryHandle`]. If another
    /// thread is already building `R`, waits for and returns its result.
    pub fn build<R: Singleton>(&self) -> Result<Arc<R>, BuildError> {
        self.inner.build_singleton::<R>(&self.handle())
    }

    /// Pre-populate the cached output for `query`. Overwrites any existing
    /// entry for the same key, and takes precedence over a build for the
    /// key that is in progress on another thread. Returns the cached
    /// `Arc<Q::Output>`.
    pub fn insert_for<Q: Query>(&self, query: Q, value: Q::Output) -> Arc<Q::Output> {
        self.inner.insert_keyed(query, value)
    }

    /// Return the cached output for `query`, if one is present. Never
    /// waits: while the key is being built on another thread this returns
    /// `None`.
    pub fn get_for<Q: Query>(&self, query: Q) -> Option<Arc<Q::Output>> {
        self.inner.get_keyed::<Q, Q::Output>(&query)
    }

    /// Resolve the output for `query`, building it via `Q::build` if it is
    /// not already cached. If another thread is already building this
    /// key, waits for and returns its result.
    pub fn build_for<Q: Query>(&self, query: Q) -> Result<Arc<Q::Output>, BuildError> {
        self.inner.build_query(query, &self.handle())
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

    /// Resolve the singleton instance of `R`, building it if it is not
    /// already cached. Returns [`BuildError::factory_dropped`] if the
    /// owning factory is gone.
    pub fn build<R: Singleton>(&self) -> Result<Arc<R>, BuildError> {
        let inner = self.upgrade()?;
        inner.build_singleton::<R>(self)
    }

    /// Pre-populate the cached output for `query`. Overwrites any existing
    /// entry for the same key. Returns [`BuildError::factory_dropped`] if
    /// the owning factory is gone.
    pub fn insert_for<Q: Query>(
        &self,
        query: Q,
        value: Q::Output,
    ) -> Result<Arc<Q::Output>, BuildError> {
        let inner = self.upgrade()?;
        Ok(inner.insert_keyed(query, value))
    }

    /// Return the cached output for `query`, if one is present.
    /// `Ok(None)` means the factory is alive but no output is cached for
    /// this key; `Err(_)` means the factory itself has been dropped.
    pub fn get_for<Q: Query>(&self, query: Q) -> Result<Option<Arc<Q::Output>>, BuildError> {
        let inner = self.upgrade()?;
        Ok(inner.get_keyed::<Q, Q::Output>(&query))
    }

    /// Resolve the output for `query`, building it via `Q::build` if it is
    /// not already cached. Returns [`BuildError::factory_dropped`] if the
    /// owning factory is gone.
    pub fn build_for<Q: Query>(&self, query: Q) -> Result<Arc<Q::Output>, BuildError> {
        let inner = self.upgrade()?;
        inner.build_query(query, self)
    }
}

/// A resource identified solely by its type. At most one instance per type
/// lives in a given factory. Implementors describe how to construct the
/// resource from the factory's handle, which they may use to resolve
/// transitive dependencies.
///
/// `build` must be cheap and side-effect-free: no I/O, no blocking, no
/// contended locking. Expensive setup belongs in async lifecycle hooks
/// (added later) or in the resource's own methods, not here.
///
/// The factory calls `build` at most once per cache miss, even under
/// concurrent requests, and other threads wait on the result while it
/// runs. A `build` that blocks therefore stalls them too.
pub trait Singleton: Send + Sync + Sized + 'static {
    fn build(factory: &DependencyFactoryHandle) -> Result<Self, BuildError>;
}

/// A keyed resource: the implementor is the key value, and `Output` is the
/// resource produced for that key. Multiple instances per output type may
/// coexist, distinguished by the key value. Calling `build_for(key)` on the
/// factory caches the produced output under that key.
///
/// `build` must be cheap and side-effect-free: no I/O, no blocking, no
/// contended locking. Expensive setup belongs in async lifecycle hooks
/// (added later) or in the resource's own methods, not here.
///
/// The factory calls `build` at most once per key per cache miss, even
/// under concurrent requests, and other threads wait on the result while
/// it runs. A `build` that blocks therefore stalls them too.
pub trait Query: Hash + Eq + Clone + Send + Sync + 'static {
    type Output: Send + Sync + 'static;

    fn build(&self, factory: &DependencyFactoryHandle) -> Result<Self::Output, BuildError>;
}
