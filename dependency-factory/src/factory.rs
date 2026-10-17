use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::marker::PhantomData;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::thread::{self, ThreadId};

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

/// One cache slot. A key with no slot has never been requested, or its
/// last build failed.
enum Slot<V> {
    /// `build` is running for this key on the named thread. Other threads
    /// that ask for the key wait for the transition out of this state.
    Building {
        owner: ThreadId,
    },
    Ready(Arc<V>),
}

type KeyMap<Q, V> = HashMap<Q, Slot<V>>;

/// Why a parked thread is parked: the thread whose build it waits for,
/// and the name of the resource it wanted, for cycle reports.
struct Waiting {
    on: ThreadId,
    wants: &'static str,
}

struct State {
    /// Outer key: `TypeId` of the query key type `Q`.
    /// Stored value: a boxed `KeyMap<Q, V>` — a `HashMap<Q, Slot<V>>` —
    /// type-erased so that maps for different query types can live in one
    /// outer map. The inner map is recovered by downcasting on lookup.
    by_query_type: HashMap<TypeId, Box<dyn Any + Send + Sync>>,
    /// Threads currently parked on `Inner::slot_changed`, keyed by the
    /// parked thread. Used to detect cycles that span threads before
    /// parking would turn them into a deadlock.
    waiting: HashMap<ThreadId, Waiting>,
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

    /// If parking the current thread to wait for `owner` would close a
    /// cycle of threads each waiting on the next, return the names of the
    /// resources along that cycle. `wants` is what the current thread is
    /// asking for.
    fn wait_cycle(
        &self,
        owner: ThreadId,
        me: ThreadId,
        wants: &'static str,
    ) -> Option<Vec<&'static str>> {
        let mut path = vec![wants];
        let mut next = owner;
        // Each step consumes a distinct parked thread, so the chain is no
        // longer than the number of parked threads plus the first hop.
        for _ in 0..=self.waiting.len() {
            if next == me {
                path.push(wants);
                return Some(path);
            }
            let waiting = self.waiting.get(&next)?;
            path.push(waiting.wants);
            next = waiting.on;
        }
        None
    }
}

struct Inner {
    state: Mutex<State>,
    /// Notified on every slot transition. Parked threads re-examine their
    /// slot when woken; spurious wake-ups are harmless.
    slot_changed: Condvar,
}

/// What `Inner::claim` found for a key once it stopped waiting.
enum Claim<Q, V>
where
    Q: Hash + Eq + Clone + Send + Sync + 'static,
    V: Send + Sync + 'static,
{
    /// Another thread finished the build (or an `insert` landed) while we
    /// were looking. Nothing to do but return the value.
    Ready(Arc<V>),
    /// The slot was empty and is now ours to build.
    Claimed(ClaimGuard<Q, V>),
}

/// A snapshot of one slot taken under the lock. Lets `claim` release the
/// borrow of the map before it waits or inserts.
enum Found<V> {
    Ready(Arc<V>),
    Building(ThreadId),
    Empty,
}

impl Inner {
    fn new() -> Self {
        Self {
            state: Mutex::new(State {
                by_query_type: HashMap::new(),
                waiting: HashMap::new(),
            }),
            slot_changed: Condvar::new(),
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
        {
            let mut state = self.lock();
            state
                .map_mut::<Q, V>()
                .insert(key, Slot::Ready(arc.clone()));
        }
        // A build for this key may be in progress elsewhere with threads
        // parked behind it. They can have the inserted value now.
        self.slot_changed.notify_all();
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
        match state.map::<Q, V>()?.get(key)? {
            Slot::Ready(arc) => Some(arc.clone()),
            Slot::Building { .. } => None,
        }
    }

    /// Resolve `key`, calling `build` at most once per cache miss across
    /// all threads: claim the slot under the lock, run `build` with the
    /// lock released, then commit the value under the lock again. Threads
    /// that ask for the key in between wait for the commit.
    fn build_keyed<Q, V>(
        self: &Arc<Self>,
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
        // Same-thread cycles are caught here, before any claim, with the
        // full path from the breadcrumb.
        let _breadcrumb = CycleGuard::enter(frame)?;
        let guard = match self.claim::<Q, V>(&key, frame.type_name)? {
            Claim::Ready(arc) => return Ok(arc),
            Claim::Claimed(guard) => guard,
        };
        // No lock is held while user code runs. If `build` fails or
        // unwinds, dropping `guard` releases the slot and wakes waiters.
        let value = build().map_err(|e| e.push_frame(frame.type_name))?;
        Ok(guard.commit(value))
    }

    /// Take ownership of the slot for `key`, waiting while another thread
    /// is building it. Returns the finished value instead if one appears
    /// while waiting, and a `CycleError` if waiting would deadlock a ring
    /// of threads.
    fn claim<Q, V>(
        self: &Arc<Self>,
        key: &Q,
        type_name: &'static str,
    ) -> Result<Claim<Q, V>, BuildError>
    where
        Q: Hash + Eq + Clone + Send + Sync + 'static,
        V: Send + Sync + 'static,
    {
        let me = thread::current().id();
        let mut state = self.lock();
        loop {
            let found = match state.map::<Q, V>().and_then(|map| map.get(key)) {
                Some(Slot::Ready(arc)) => Found::Ready(arc.clone()),
                Some(Slot::Building { owner }) => Found::Building(*owner),
                None => Found::Empty,
            };
            match found {
                Found::Ready(arc) => return Ok(Claim::Ready(arc)),
                Found::Building(owner) => {
                    if let Some(path) = state.wait_cycle(owner, me, type_name) {
                        return Err(CycleError::new(path).into());
                    }
                    state.waiting.insert(
                        me,
                        Waiting {
                            on: owner,
                            wants: type_name,
                        },
                    );
                    state = self
                        .slot_changed
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner);
                    state.waiting.remove(&me);
                }
                Found::Empty => {
                    state
                        .map_mut::<Q, V>()
                        .insert(key.clone(), Slot::Building { owner: me });
                    return Ok(Claim::Claimed(ClaimGuard {
                        inner: Arc::clone(self),
                        key: key.clone(),
                        owner: me,
                        _value: PhantomData,
                    }));
                }
            }
        }
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

/// Ownership of one in-progress slot. Dropping it without `commit`
/// (error or unwind inside `build`) empties the slot and wakes waiters so
/// one of them can try again.
struct ClaimGuard<Q, V>
where
    Q: Hash + Eq + Clone + Send + Sync + 'static,
    V: Send + Sync + 'static,
{
    inner: Arc<Inner>,
    key: Q,
    owner: ThreadId,
    _value: PhantomData<fn() -> V>,
}

impl<Q, V> ClaimGuard<Q, V>
where
    Q: Hash + Eq + Clone + Send + Sync + 'static,
    V: Send + Sync + 'static,
{
    /// True if the slot is still the in-progress marker this guard placed.
    /// False if an `insert` replaced it with a finished value meanwhile.
    fn still_mine(&self, map: &KeyMap<Q, V>) -> bool {
        matches!(map.get(&self.key), Some(Slot::Building { owner }) if *owner == self.owner)
    }

    /// Store the built value and wake waiters. If an `insert` for the same
    /// key landed during the build, the inserted value stays and is
    /// returned instead; the built value is dropped.
    fn commit(self, value: V) -> Arc<V> {
        let built = Arc::new(value);
        let result = {
            let mut state = self.inner.lock();
            let map = state.map_mut::<Q, V>();
            if self.still_mine(map) {
                map.insert(self.key.clone(), Slot::Ready(built.clone()));
                built
            } else {
                match map.get(&self.key) {
                    Some(Slot::Ready(inserted)) => inserted.clone(),
                    // Only this guard removes its own marker, so the slot
                    // cannot be empty or owned by someone else here.
                    _ => unreachable!("in-progress slot vanished while its builder held the claim"),
                }
            }
        };
        self.inner.slot_changed.notify_all();
        result
    }
}

impl<Q, V> Drop for ClaimGuard<Q, V>
where
    Q: Hash + Eq + Clone + Send + Sync + 'static,
    V: Send + Sync + 'static,
{
    fn drop(&mut self) {
        let released = {
            let mut state = self.inner.lock();
            let map = state.map_mut::<Q, V>();
            if self.still_mine(map) {
                map.remove(&self.key);
                true
            } else {
                false
            }
        };
        if released {
            self.inner.slot_changed.notify_all();
        }
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
