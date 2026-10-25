use std::any::TypeId;
use std::fmt;
use std::hash::Hash;
use std::marker::PhantomData;
use std::mem;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::thread;

use crate::error::OccupiedError;
use crate::store::{Store, data_ptr};

/// A key that identifies one resource in a [`Registry`]. The key type fixes
/// the value type, so a lookup by key can never see a value of the wrong
/// type.
///
/// Implement `Key` alone for resources that are inserted by hand, and
/// [`Query`](crate::Query) on top of it for resources the factory can build.
/// `Value` may be a trait object; values are held, inserted and built as
/// `Arc`s, so nothing needs it to be sized.
///
/// A key appears in error messages through its [`Debug`](fmt::Debug)
/// impl, as in `while building Db for DbKey("users")`. A key that holds
/// a secret should implement `Debug` by hand and leave the secret out.
pub trait Key: fmt::Debug + Hash + Eq + Clone + Send + Sync + 'static {
    type Value: ?Sized + Send + Sync + 'static;
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

impl<R: ?Sized> fmt::Debug for SingletonKey<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SingletonKey<{}>", std::any::type_name::<R>())
    }
}

impl<R: ?Sized + Send + Sync + 'static> Key for SingletonKey<R> {
    type Value = R;
}

/// A thread-safe, type-keyed map of `Arc`-shared resources.
///
/// A registry is a plain store: it knows nothing about building. The
/// [`DependencyFactory`](crate::DependencyFactory) is one client of it,
/// committing the resources it builds through
/// [`Transaction::get_or_insert_with`]; any other code can populate the
/// same registry by hand on equal footing.
///
/// Two kinds of entry share one mechanism: singletons, identified by their
/// type alone (`insert`, `get`, ...), and keyed resources, identified by a
/// [`Key`] value (`insert_for`, `get_for`, ...). Every value is held as an
/// `Arc`, and goes in as one. The type may be a trait object: the
/// singleton slot for `dyn Mailer` holds an `Arc<dyn Mailer>`, put
/// there by `insert::<dyn Mailer>(Arc::new(fake))`, which coerces the
/// `Arc` at the call site, or by the factory through a
/// [`Singleton`](crate::Singleton) impl for the trait object.
///
/// Alongside its entries a registry keeps *indexes*: for any `T`, usually
/// a trait object such as `dyn WarmUp`, the list of `Arc<T>` that have
/// been added to the index for `T` by [`Transaction::index`], in the
/// order they were added. An index is how code that handles a lifecycle
/// event finds every resource that takes part in it without knowing their
/// concrete types; see [`indexed`](Self::indexed). Indexes are not
/// registered ahead of time: the index for `T` exists once something is
/// added to it.
///
/// Reads live here and never wait for a [`Transaction`], not even for a
/// build in progress: they look at the front buffer, which a transaction
/// touches only for the moment it takes to publish. Writes live on the
/// transaction, from [`begin`](Self::begin), and are published together
/// when it commits.
#[derive(Default)]
pub struct Registry {
    /// What reads see. Written only by a committing [`Transaction`], which
    /// takes the write lock to swap in the back buffer and lets go again,
    /// so readers are never held off for longer than that swap.
    front: RwLock<Store>,
    /// What writes go into. Its mutex is the registry's lock, held through
    /// a [`Transaction`] for as long as the transaction lives. Between
    /// transactions the two buffers hold the same entries.
    back: Mutex<Store>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Begin a transaction, waiting for any other [`Transaction`]
    /// (including one inside a [`Builder`](crate::Builder)) to end first.
    /// Writes made through it land together when it commits, which
    /// dropping it does. For a single write, `registry.begin().insert(value)`
    /// is complete: the temporary transaction drops, and commits, at the
    /// end of the statement.
    ///
    /// Reads on the registry never wait for a transaction, so they are
    /// safe to make from the thread with one open. A second `begin`, or a
    /// build, on that thread waits on the open transaction forever. The
    /// usual way to trip over this is a transaction temporary in a `match`
    /// scrutinee, which lives until the end of the `match`; bind the
    /// transaction to a local instead and make every write through it.
    pub fn begin(&self) -> Transaction<'_> {
        Transaction {
            back: self.back.lock().unwrap_or_else(PoisonError::into_inner),
            overlay: Store::default(),
            registry: self,
        }
    }

    /// Return the singleton instance of `R`, if one is present.
    pub fn get<R: ?Sized + Send + Sync + 'static>(&self) -> Option<Arc<R>> {
        self.get_for(&SingletonKey::<R>::new())
    }

    /// Return the resource stored under `key`, if one is present.
    pub fn get_for<K: Key>(&self, key: &K) -> Option<Arc<K::Value>> {
        self.front
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
    }

    /// Return everything in the index for `T`, in the order it was added.
    /// Empty if nothing has been added to it. The list is a snapshot:
    /// additions published after this call are not in it.
    ///
    /// Resources the factory builds are added to their indexes by their
    /// [`Singleton::register`](crate::Singleton::register) or
    /// [`Query::register`](crate::Query::register) after their
    /// dependencies have been built and registered, so a resource always
    /// follows its dependencies in an index: iterate forwards to start
    /// things up, backwards to shut them down.
    pub fn indexed<T: ?Sized + Send + Sync + 'static>(&self) -> Vec<Arc<T>> {
        self.front
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .indexed::<T>()
            .cloned()
            .unwrap_or_default()
    }
}

impl fmt::Debug for Registry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Registry").finish_non_exhaustive()
    }
}

/// A transaction on a [`Registry`], from [`Registry::begin`], and the only
/// way to write to one. Writes are staged in the transaction and published
/// together when it commits; until then reads through the transaction see
/// them and nobody else does. A [`Builder`](crate::Builder) holds one for
/// as long as it lives.
///
/// Dropping a transaction commits it, so `registry.begin().insert(value)`
/// is a complete single write. [`commit`](Self::commit) says the same
/// thing explicitly, and [`discard`](Self::discard) throws the pending
/// writes away instead. A transaction dropped while its thread is
/// unwinding from a panic is discarded, so a panic never publishes a
/// partial batch.
///
/// Writes take the value in an `Arc`, which is how the registry holds it,
/// and come in three flavours. `insert` fills an empty slot and refuses an
/// occupied one; `get_or_insert_with` fills an empty slot and otherwise
/// returns what is there; `replace` overwrites, and hands back what it
/// displaced. Replacing does not affect anyone already holding the
/// previous `Arc`. The same `Arc` may sit in more than one slot.
///
/// Indexes are written separately. [`index`](Self::index) adds an `Arc`
/// to the index for some `T`, and [`unindex`](Self::unindex) takes it out
/// again; nothing else changes an index. Inserting a value by hand does
/// not index it, and replacing one does not unindex it, since the
/// registry cannot tell which traits a value implements; make those
/// writes next to the insert or replace. Resources the factory builds
/// index themselves through
/// [`Singleton::register`](crate::Singleton::register) and
/// [`Query::register`](crate::Query::register).
pub struct Transaction<'a> {
    /// The registry's back buffer, held for the life of the transaction.
    back: MutexGuard<'a, Store>,
    /// The pending writes, staged here until the transaction commits. A
    /// lookup through the transaction checks the overlay before the back
    /// buffer, so the transaction sees its own writes; nobody else does
    /// until they are published.
    overlay: Store,
    registry: &'a Registry,
}

impl Transaction<'_> {
    /// Return the singleton instance of `R`, if one is present.
    pub fn get<R: ?Sized + Send + Sync + 'static>(&self) -> Option<Arc<R>> {
        self.get_for(&SingletonKey::<R>::new())
    }

    /// Store `value` as the singleton instance of `R`, if there is none
    /// yet. Returns the rejected `value` in an [`OccupiedError`] if an
    /// instance was already present. For a trait object,
    /// `insert::<dyn Mailer>(Arc::new(fake))` coerces the `Arc` at the
    /// call site.
    pub fn insert<R: ?Sized + Send + Sync + 'static>(
        &mut self,
        value: Arc<R>,
    ) -> Result<(), OccupiedError<R>> {
        self.insert_for(SingletonKey::<R>::new(), value)
    }

    /// Store `value` as the singleton instance of `R`, overwriting any
    /// instance already present, and return that previous instance.
    pub fn replace<R: ?Sized + Send + Sync + 'static>(&mut self, value: Arc<R>) -> Option<Arc<R>> {
        self.replace_for(SingletonKey::<R>::new(), value)
    }

    /// Return the singleton instance of `R`, storing the result of `make`
    /// first if there is none. `make` is not called when an instance is
    /// already present.
    pub fn get_or_insert_with<R: ?Sized + Send + Sync + 'static>(
        &mut self,
        make: impl FnOnce() -> Arc<R>,
    ) -> Arc<R> {
        self.get_or_insert_with_for(SingletonKey::<R>::new(), make)
    }

    /// Return the resource stored under `key`, if one is present.
    pub fn get_for<K: Key>(&self, key: &K) -> Option<Arc<K::Value>> {
        self.overlay.get(key).or_else(|| self.back.get(key))
    }

    /// Store `value` under `key`, if the slot is empty. Returns the
    /// rejected `value` in an [`OccupiedError`] if the slot was already
    /// filled.
    pub fn insert_for<K: Key>(
        &mut self,
        key: K,
        value: Arc<K::Value>,
    ) -> Result<(), OccupiedError<K::Value>> {
        if self.get_for(&key).is_some() {
            return Err(OccupiedError::new(value));
        }
        self.overlay.insert(key, value);
        Ok(())
    }

    /// Store `value` under `key`, overwriting any resource already there,
    /// and return that previous resource. Indexes are untouched: the
    /// previous `Arc` stays in any it was added to until
    /// [`unindex`](Self::unindex) takes it out, and the new one is not
    /// indexed.
    pub fn replace_for<K: Key>(&mut self, key: K, value: Arc<K::Value>) -> Option<Arc<K::Value>> {
        let previous = self.get_for(&key);
        self.overlay.insert(key, value);
        previous
    }

    /// Return the resource stored under `key`, storing the result of `make`
    /// first if the slot is empty. `make` is not called when the slot is
    /// already filled. What a [`Builder`](crate::Builder) commits with.
    pub fn get_or_insert_with_for<K: Key>(
        &mut self,
        key: K,
        make: impl FnOnce() -> Arc<K::Value>,
    ) -> Arc<K::Value> {
        match self.get_for(&key) {
            Some(arc) => arc,
            None => {
                let arc = make();
                self.overlay.insert(key, Arc::clone(&arc));
                arc
            }
        }
    }

    /// Add `item` to the end of the index for `T`. `T` is usually a trait
    /// object, so that `Arc<Resource>` coerces to `Arc<dyn Trait>` at the
    /// call site, where the compiler can see the `impl`:
    ///
    /// ```
    /// # use std::sync::Arc;
    /// # use dependency_factory::Registry;
    /// # trait WarmUp: Send + Sync { fn warm_up(&self); }
    /// # struct Cache;
    /// # impl WarmUp for Cache { fn warm_up(&self) {} }
    /// let registry = Registry::new();
    /// let mut tx = registry.begin();
    /// let cache = Arc::new(Cache);
    /// tx.insert(Arc::clone(&cache)).unwrap();
    /// tx.index::<dyn WarmUp>(cache);
    /// tx.commit();
    /// for resource in registry.indexed::<dyn WarmUp>() {
    ///     resource.warm_up();
    /// }
    /// ```
    ///
    /// Nothing checks that `item` is a registry value; any `Arc` can be
    /// indexed, and nothing but [`unindex`](Self::unindex) takes it out
    /// again.
    pub fn index<T: ?Sized + Send + Sync + 'static>(&mut self, item: Arc<T>) {
        self.overlay.adopt_index(TypeId::of::<T>(), &self.back);
        self.overlay.index(item);
    }

    /// Remove `item` from the index for `T`, wherever it appears in it.
    /// The inverse of [`index`](Self::index): `item` is matched by
    /// allocation, so an `Arc<Resource>` coerced to `Arc<dyn Trait>` here
    /// matches the one `index` was given, and only the index for `T` is
    /// touched. A value replaced in its slot is still in its indexes
    /// until this takes it out:
    ///
    /// ```
    /// # use std::sync::Arc;
    /// # use dependency_factory::Registry;
    /// # trait WarmUp: Send + Sync { fn warm_up(&self); }
    /// # struct Cache;
    /// # impl WarmUp for Cache { fn warm_up(&self) {} }
    /// # let registry = Registry::new();
    /// # registry.begin().insert(Arc::new(Cache)).unwrap();
    /// let mut tx = registry.begin();
    /// let fresh = Arc::new(Cache);
    /// if let Some(stale) = tx.replace(Arc::clone(&fresh)) {
    ///     tx.unindex::<dyn WarmUp>(stale);
    /// }
    /// tx.index::<dyn WarmUp>(fresh);
    /// ```
    pub fn unindex<T: ?Sized + Send + Sync + 'static>(&mut self, item: Arc<T>) {
        self.overlay.adopt_index(TypeId::of::<T>(), &self.back);
        self.overlay.unindex::<T>(data_ptr(&item));
    }

    /// Return everything in the index for `T`, in the order it was added,
    /// including what this transaction has added and not yet published.
    /// Empty if nothing has been added to it. The list is a snapshot:
    /// later additions are not in it.
    pub fn indexed<T: ?Sized + Send + Sync + 'static>(&self) -> Vec<Arc<T>> {
        self.overlay
            .indexed::<T>()
            .or_else(|| self.back.indexed::<T>())
            .cloned()
            .unwrap_or_default()
    }

    /// End the transaction and publish its writes. Dropping the
    /// transaction does the same; this spells it out.
    pub fn commit(self) {
        drop(self);
    }

    /// End the transaction and throw its writes away. Nothing is
    /// published, and the registry is as it was when the transaction
    /// began.
    pub fn discard(mut self) {
        self.overlay = Store::default();
    }
}

impl Drop for Transaction<'_> {
    /// Commit, unless there is nothing to commit or the thread is
    /// unwinding. The back buffer is brought up to date under the lock
    /// this transaction already holds, swapped in as the front buffer
    /// under the front's write lock, and then the old front, now the back
    /// buffer and reachable only through this transaction, is brought up
    /// to date too, by moving the overlay in. Readers are held off for the
    /// swap alone, and the cost is one copy and one move of the batch,
    /// never a pass over the registry.
    fn drop(&mut self) {
        if self.overlay.is_empty() || thread::panicking() {
            return;
        }
        let overlay = mem::take(&mut self.overlay);
        overlay.merge_into(&mut self.back);
        {
            let mut front = self
                .registry
                .front
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            mem::swap(&mut *front, &mut *self.back);
        }
        overlay.move_into(&mut self.back);
    }
}

impl fmt::Debug for Transaction<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Transaction").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use super::*;

    #[test]
    fn insert_and_get_singleton() {
        let registry = Registry::new();
        let inserted = Arc::new(42_u32);
        registry.begin().insert(Arc::clone(&inserted)).unwrap();
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
        registry.begin().insert(Arc::new(1_u32)).unwrap();
        registry.begin().insert(Arc::new(2_u64)).unwrap();
        assert_eq!(*registry.get::<u32>().unwrap(), 1);
        assert_eq!(*registry.get::<u64>().unwrap(), 2);
    }

    #[test]
    fn insert_refuses_an_occupied_slot() {
        let registry = Registry::new();
        registry.begin().insert(Arc::new(1_u32)).unwrap();
        let err = registry.begin().insert(Arc::new(2_u32)).unwrap_err();
        assert_eq!(**err.value(), 2);
        assert_eq!(*err.into_value(), 2);
        assert_eq!(*registry.get::<u32>().unwrap(), 1);
    }

    #[test]
    fn replace_overwrites_an_occupied_slot_and_returns_the_previous_value() {
        let registry = Registry::new();
        assert!(registry.begin().replace(Arc::new(1_u32)).is_none());
        let first = registry.get::<u32>().unwrap();
        let second = Arc::new(2_u32);
        let previous = registry.begin().replace(Arc::clone(&second)).unwrap();
        assert!(Arc::ptr_eq(&previous, &first));
        assert!(Arc::ptr_eq(&second, &registry.get::<u32>().unwrap()));
        // The previous Arc is unaffected for anyone still holding it.
        assert_eq!(*first, 1);
    }

    #[test]
    fn get_or_insert_with_fills_an_empty_slot_only() {
        let registry = Registry::new();
        let made = registry.begin().get_or_insert_with(|| Arc::new(1_u32));
        assert_eq!(*made, 1);
        let kept = registry
            .begin()
            .get_or_insert_with(|| -> Arc<u32> { panic!("must not be called") });
        assert!(Arc::ptr_eq(&made, &kept));
    }

    #[derive(Hash, Eq, PartialEq, Clone, Debug)]
    struct StringKey(u32);

    impl Key for StringKey {
        type Value = String;
    }

    fn string(s: &str) -> Arc<String> {
        Arc::new(s.to_string())
    }

    #[test]
    fn insert_for_and_get_for_round_trip() {
        let registry = Registry::new();
        let inserted = string("seven");
        registry
            .begin()
            .insert_for(StringKey(7), Arc::clone(&inserted))
            .unwrap();
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
        registry
            .begin()
            .insert_for(StringKey(1), string("one"))
            .unwrap();
        registry
            .begin()
            .insert_for(StringKey(2), string("two"))
            .unwrap();
        assert_eq!(*registry.get_for(&StringKey(1)).unwrap(), "one");
        assert_eq!(*registry.get_for(&StringKey(2)).unwrap(), "two");
    }

    #[test]
    fn insert_for_refuses_an_occupied_slot() {
        let registry = Registry::new();
        registry
            .begin()
            .insert_for(StringKey(1), string("first"))
            .unwrap();
        let err = registry
            .begin()
            .insert_for(StringKey(1), string("second"))
            .unwrap_err();
        assert_eq!(*err.into_value(), "second");
        assert_eq!(*registry.get_for(&StringKey(1)).unwrap(), "first");
    }

    #[test]
    fn replace_for_overwrites_an_occupied_slot() {
        let registry = Registry::new();
        registry
            .begin()
            .insert_for(StringKey(1), string("first"))
            .unwrap();
        let previous = registry
            .begin()
            .replace_for(StringKey(1), string("second"))
            .unwrap();
        assert_eq!(*previous, "first");
        assert_eq!(*registry.get_for(&StringKey(1)).unwrap(), "second");
    }

    #[test]
    fn get_or_insert_with_for_fills_an_empty_slot_only() {
        let registry = Registry::new();
        let made = registry
            .begin()
            .get_or_insert_with_for(StringKey(3), || string("three"));
        let kept = registry
            .begin()
            .get_or_insert_with_for(StringKey(3), || panic!("must not be called"));
        assert!(Arc::ptr_eq(&made, &kept));
    }

    #[test]
    fn occupied_error_is_debug_without_a_debug_value() {
        struct Opaque;
        let registry = Registry::new();
        registry.begin().insert(Arc::new(Opaque)).unwrap();
        let err = registry
            .begin()
            .insert(Arc::new(Opaque))
            .expect_err("slot is occupied");
        let rendered = format!("{err:?}");
        assert!(rendered.contains("Opaque"), "{rendered}");
    }

    // --- Transactions ------------------------------------------------------------

    #[test]
    fn a_transaction_sees_its_own_writes_before_they_are_published() {
        let registry = Registry::new();
        let mut tx = registry.begin();
        let inserted = Arc::new(1_u32);
        tx.insert(Arc::clone(&inserted)).unwrap();
        assert!(Arc::ptr_eq(&inserted, &tx.get::<u32>().unwrap()));
        let err = tx.insert(Arc::new(2_u32)).unwrap_err();
        assert_eq!(*err.into_value(), 2);
        assert_eq!(*tx.get_or_insert_with(|| Arc::new(3_u32)), 1);
        let previous = tx.replace(Arc::new(4_u32)).unwrap();
        assert!(Arc::ptr_eq(&previous, &inserted));
        assert_eq!(*tx.get::<u32>().unwrap(), 4);
    }

    #[test]
    fn a_transaction_publishes_its_writes_together_when_dropped() {
        let registry = Registry::new();
        let mut tx = registry.begin();
        tx.insert(Arc::new(1_u32)).unwrap();
        tx.insert(Arc::new(2_u64)).unwrap();
        // Same thread, transaction open: reads neither wait nor see the batch.
        assert!(registry.get::<u32>().is_none());
        assert!(registry.get::<u64>().is_none());
        drop(tx);
        assert_eq!(*registry.get::<u32>().unwrap(), 1);
        assert_eq!(*registry.get::<u64>().unwrap(), 2);
    }

    #[test]
    fn a_transaction_on_another_thread_holds_off_writes_but_not_reads() {
        let registry = Arc::new(Registry::new());
        registry.begin().insert(Arc::new(1_u32)).unwrap();
        let (held_tx, held_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();

        let holder = {
            let registry = Arc::clone(&registry);
            thread::spawn(move || {
                let mut tx = registry.begin();
                tx.replace(Arc::new(2_u32));
                held_tx.send(()).unwrap();
                // Hold until told to let go, or until the writer below has
                // had long enough to prove it is blocked.
                let _ = release_rx.recv_timeout(Duration::from_millis(100));
                drop(tx);
            })
        };
        held_rx.recv().unwrap();

        // Reads go through, and see the state before the batch.
        assert_eq!(*registry.get::<u32>().unwrap(), 1);

        // A write waits for the transaction, and lands after its batch.
        let writer = {
            let registry = Arc::clone(&registry);
            thread::spawn(move || registry.begin().replace(Arc::new(3_u32)))
        };
        thread::sleep(Duration::from_millis(20));
        assert_eq!(*registry.get::<u32>().unwrap(), 1, "writer got in early");
        drop(release_tx);
        holder.join().unwrap();
        writer.join().unwrap();
        assert_eq!(*registry.get::<u32>().unwrap(), 3);
    }

    #[test]
    fn a_panic_discards_the_transaction_and_does_not_poison_the_registry() {
        let registry = Registry::new();
        registry.begin().insert(Arc::new(0_u32)).unwrap();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut tx = registry.begin();
            tx.replace(Arc::new(1_u32));
            tx.insert(Arc::new(1_u64)).unwrap();
            panic!("inside the transaction");
        }));
        assert!(outcome.is_err());
        // Nothing from the transaction was published...
        assert_eq!(*registry.get::<u32>().unwrap(), 0);
        assert!(registry.get::<u64>().is_none());
        // ...and the registry is usable again.
        registry.begin().insert(Arc::new(2_u64)).unwrap();
        assert_eq!(*registry.get::<u64>().unwrap(), 2);
    }

    #[test]
    fn commit_publishes_and_discard_does_not() {
        let registry = Registry::new();
        registry.begin().insert(Arc::new(0_u32)).unwrap();

        let mut tx = registry.begin();
        tx.replace(Arc::new(1_u32));
        tx.insert(Arc::new(1_u64)).unwrap();
        tx.discard();
        assert_eq!(*registry.get::<u32>().unwrap(), 0);
        assert!(registry.get::<u64>().is_none());

        let mut tx = registry.begin();
        tx.replace(Arc::new(2_u32));
        tx.insert(Arc::new(2_u64)).unwrap();
        tx.commit();
        assert_eq!(*registry.get::<u32>().unwrap(), 2);
        assert_eq!(*registry.get::<u64>().unwrap(), 2);
    }

    // --- Indexes -------------------------------------------------------------

    trait Named: Send + Sync {
        fn name(&self) -> &str;
    }

    impl Named for String {
        fn name(&self) -> &str {
            self
        }
    }

    fn names(index: Vec<Arc<dyn Named>>) -> Vec<String> {
        index.iter().map(|n| n.name().to_string()).collect()
    }

    #[test]
    fn indexed_is_empty_until_something_is_added() {
        let registry = Registry::new();
        assert!(registry.indexed::<dyn Named>().is_empty());
        assert!(registry.begin().indexed::<dyn Named>().is_empty());
    }

    #[test]
    fn index_keeps_insertion_order_across_transactions() {
        let registry = Registry::new();
        let one = string("one");
        registry.begin().insert(Arc::clone(&one)).unwrap();
        registry
            .begin()
            .index::<dyn Named>(Arc::clone(&one) as Arc<dyn Named>);
        {
            let mut tx = registry.begin();
            tx.index::<dyn Named>(string("two"));
            tx.index::<dyn Named>(string("three"));
        }
        assert_eq!(
            names(registry.indexed::<dyn Named>()),
            ["one", "two", "three"]
        );
        let first = &registry.indexed::<dyn Named>()[0];
        assert_eq!(data_ptr(first), data_ptr(&one));
    }

    #[test]
    fn an_index_addition_is_published_with_the_batch() {
        let registry = Registry::new();
        registry.begin().index::<dyn Named>(string("one"));
        let mut tx = registry.begin();
        tx.index::<dyn Named>(string("two"));
        assert_eq!(names(tx.indexed::<dyn Named>()), ["one", "two"]);
        assert_eq!(names(registry.indexed::<dyn Named>()), ["one"]);
        drop(tx);
        assert_eq!(names(registry.indexed::<dyn Named>()), ["one", "two"]);
    }

    #[test]
    fn discard_and_panic_drop_index_changes() {
        let registry = Registry::new();
        let one = string("one");
        registry
            .begin()
            .index::<dyn Named>(Arc::clone(&one) as Arc<dyn Named>);

        let mut tx = registry.begin();
        tx.index::<dyn Named>(string("two"));
        tx.unindex::<dyn Named>(Arc::clone(&one) as Arc<dyn Named>);
        tx.discard();
        assert_eq!(names(registry.indexed::<dyn Named>()), ["one"]);

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut tx = registry.begin();
            tx.index::<dyn Named>(string("two"));
            tx.unindex::<dyn Named>(Arc::clone(&one) as Arc<dyn Named>);
            panic!("inside the transaction");
        }));
        assert!(outcome.is_err());
        assert_eq!(names(registry.indexed::<dyn Named>()), ["one"]);
    }

    #[test]
    fn replace_leaves_indexes_alone() {
        let registry = Registry::new();
        let old = string("old");
        {
            let mut tx = registry.begin();
            tx.insert(Arc::clone(&old)).unwrap();
            tx.index::<dyn Named>(Arc::clone(&old) as Arc<dyn Named>);
        }
        let new = string("new");
        let previous = registry.begin().replace(Arc::clone(&new)).unwrap();
        assert!(Arc::ptr_eq(&previous, &old));
        assert!(Arc::ptr_eq(&new, &registry.get::<String>().unwrap()));
        // The old value is still indexed, and the new one is not.
        assert_eq!(names(registry.indexed::<dyn Named>()), ["old"]);
    }

    #[test]
    fn unindex_removes_from_one_index_and_is_published_with_the_batch() {
        let registry = Registry::new();
        let old = string("old");
        {
            let mut tx = registry.begin();
            tx.insert(Arc::clone(&old)).unwrap();
            tx.index::<dyn Named>(string("before"));
            tx.index::<dyn Named>(Arc::clone(&old) as Arc<dyn Named>);
            tx.index::<dyn Named>(string("after"));
            tx.index::<String>(Arc::clone(&old));
        }

        // The removal is staged with the batch, like any other write.
        let mut tx = registry.begin();
        tx.unindex::<dyn Named>(Arc::clone(&old) as Arc<dyn Named>);
        assert_eq!(names(tx.indexed::<dyn Named>()), ["before", "after"]);
        assert_eq!(
            names(registry.indexed::<dyn Named>()),
            ["before", "old", "after"]
        );
        drop(tx);
        assert_eq!(names(registry.indexed::<dyn Named>()), ["before", "after"]);
        // Only the index for the named type is touched...
        assert_eq!(registry.indexed::<String>().len(), 1);
        // ...and the slot is untouched.
        assert!(Arc::ptr_eq(&old, &registry.get::<String>().unwrap()));
    }

    #[test]
    fn unindex_within_one_transaction() {
        let registry = Registry::new();
        let mut tx = registry.begin();
        let old = string("old");
        tx.index::<dyn Named>(Arc::clone(&old) as Arc<dyn Named>);
        tx.unindex::<dyn Named>(old);
        assert!(tx.indexed::<dyn Named>().is_empty());
        drop(tx);
        assert!(registry.indexed::<dyn Named>().is_empty());
    }

    #[test]
    fn unindex_matches_by_allocation_not_by_value() {
        let registry = Registry::new();
        let one = string("one");
        registry
            .begin()
            .index::<dyn Named>(Arc::clone(&one) as Arc<dyn Named>);
        // An equal but distinct value is not the same allocation.
        registry.begin().unindex::<dyn Named>(string("one"));
        assert_eq!(names(registry.indexed::<dyn Named>()), ["one"]);
        // Something never indexed, and an index that does not exist, are
        // no-ops rather than errors.
        registry.begin().unindex::<dyn Named>(string("two"));
        registry.begin().unindex::<u32>(Arc::new(1));
        assert_eq!(names(registry.indexed::<dyn Named>()), ["one"]);
        assert!(registry.indexed::<u32>().is_empty());
    }

    #[test]
    fn the_buffers_agree_on_indexes_between_transactions() {
        let registry = Registry::new();

        fn assert_buffers_agree(registry: &Registry) {
            let front = registry.front.read().unwrap();
            let back = registry.back.lock().unwrap();
            let f = front.indexed::<dyn Named>().cloned().unwrap_or_default();
            let b = back.indexed::<dyn Named>().cloned().unwrap_or_default();
            assert_eq!(f.len(), b.len());
            for (f, b) in f.iter().zip(&b) {
                assert_eq!(data_ptr(f), data_ptr(b));
            }
        }

        assert_buffers_agree(&registry);
        registry.begin().index::<dyn Named>(string("one"));
        assert_buffers_agree(&registry);
        let two = string("two");
        {
            let mut tx = registry.begin();
            tx.insert(Arc::clone(&two)).unwrap();
            tx.index::<dyn Named>(Arc::clone(&two) as Arc<dyn Named>);
            tx.index::<dyn Named>(string("three"));
        }
        assert_buffers_agree(&registry);
        registry.begin().unindex::<dyn Named>(two);
        assert_buffers_agree(&registry);
        assert_eq!(names(registry.indexed::<dyn Named>()), ["one", "three"]);
    }

    // --- Trait-object values, and one Arc in two slots ------------------------

    #[test]
    fn a_singleton_slot_may_hold_a_trait_object() {
        let registry = Registry::new();
        let one: Arc<dyn Named> = string("one");
        registry
            .begin()
            .insert::<dyn Named>(Arc::clone(&one))
            .unwrap();
        assert!(Arc::ptr_eq(&registry.get::<dyn Named>().unwrap(), &one));
        // Occupied like any other slot; the rejected Arc comes back.
        let rejected = registry
            .begin()
            .insert::<dyn Named>(string("two"))
            .expect_err("the slot is occupied");
        assert_eq!(rejected.into_value().name(), "two");
        let previous = registry
            .begin()
            .replace::<dyn Named>(string("three"))
            .unwrap();
        assert_eq!(previous.name(), "one");
        assert_eq!(registry.get::<dyn Named>().unwrap().name(), "three");
        assert!(registry.begin().get::<dyn Named>().is_some());
    }

    #[derive(Hash, Eq, PartialEq, Clone, Debug)]
    struct Slot(u32);

    impl Key for Slot {
        type Value = dyn Named;
    }

    #[test]
    fn a_key_may_have_a_trait_object_value() {
        let registry = Registry::new();
        registry.begin().insert_for(Slot(1), string("one")).unwrap();
        assert_eq!(registry.get_for(&Slot(1)).unwrap().name(), "one");
        assert!(registry.get_for(&Slot(2)).is_none());
        assert!(registry.begin().insert_for(Slot(1), string("uno")).is_err());
        registry.begin().replace_for(Slot(1), string("uno"));
        assert_eq!(registry.get_for(&Slot(1)).unwrap().name(), "uno");
        let made = registry
            .begin()
            .get_or_insert_with_for(Slot(2), || string("two"));
        assert_eq!(made.name(), "two");
    }

    #[test]
    fn one_arc_may_sit_in_two_slots() {
        let registry = Registry::new();
        let shared = string("shared");
        {
            let mut tx = registry.begin();
            tx.insert(Arc::clone(&shared)).unwrap();
            tx.insert(Arc::clone(&shared) as Arc<dyn Named>).unwrap();
        }
        assert!(Arc::ptr_eq(&registry.get::<String>().unwrap(), &shared));
        assert_eq!(
            data_ptr(&registry.get::<dyn Named>().unwrap()),
            data_ptr(&shared)
        );
        // Replacing one slot leaves the other alone.
        registry.begin().replace::<dyn Named>(string("other"));
        assert!(Arc::ptr_eq(&registry.get::<String>().unwrap(), &shared));
    }

    // --- The overlay and the two buffers -------------------------------------

    #[test]
    fn a_transaction_shadows_a_published_entry_until_it_commits() {
        let registry = Registry::new();
        registry.begin().insert(Arc::new(1_u32)).unwrap();
        let mut tx = registry.begin();
        tx.replace(Arc::new(2_u32));
        assert_eq!(*tx.get::<u32>().unwrap(), 2);
        assert_eq!(*registry.get::<u32>().unwrap(), 1);
        drop(tx);
        assert_eq!(*registry.get::<u32>().unwrap(), 2);
    }

    #[test]
    fn a_transaction_refuses_a_slot_filled_before_it_began() {
        let registry = Registry::new();
        registry.begin().insert(Arc::new(1_u32)).unwrap();
        let mut tx = registry.begin();
        assert_eq!(*tx.insert(Arc::new(2_u32)).unwrap_err().into_value(), 2);
        assert_eq!(*tx.get_or_insert_with(|| Arc::new(3_u32)), 1);
    }

    #[test]
    fn a_transaction_that_only_reads_publishes_nothing() {
        let registry = Registry::new();
        registry.begin().insert(Arc::new(1_u32)).unwrap();
        let before = registry.get::<u32>().unwrap();
        {
            let tx = registry.begin();
            assert!(tx.get::<u32>().is_some());
            assert!(tx.get::<u64>().is_none());
        }
        assert!(Arc::ptr_eq(&before, &registry.get::<u32>().unwrap()));
    }

    /// The invariant the double buffer rests on: whenever no transaction is open,
    /// the front and back buffers hold the same entries.
    #[test]
    fn the_buffers_agree_between_transactions() {
        let registry = Registry::new();

        fn assert_buffers_agree(registry: &Registry) {
            let front = registry.front.read().unwrap();
            let back = registry.back.lock().unwrap();
            for key in [StringKey(1), StringKey(2), StringKey(3)] {
                match (front.get(&key), back.get(&key)) {
                    (Some(f), Some(b)) => assert!(Arc::ptr_eq(&f, &b), "{key:?}"),
                    (None, None) => {}
                    (f, b) => panic!("{key:?}: front {f:?}, back {b:?}"),
                }
            }
            match (
                front.get(&SingletonKey::<u32>::new()),
                back.get(&SingletonKey::<u32>::new()),
            ) {
                (Some(f), Some(b)) => assert!(Arc::ptr_eq(&f, &b)),
                (None, None) => {}
                (f, b) => panic!("u32: front {f:?}, back {b:?}"),
            }
        }

        assert_buffers_agree(&registry);
        registry
            .begin()
            .insert_for(StringKey(1), string("one"))
            .unwrap();
        assert_buffers_agree(&registry);
        {
            let mut tx = registry.begin();
            tx.insert_for(StringKey(2), string("two")).unwrap();
            tx.replace_for(StringKey(1), string("uno"));
            tx.insert(Arc::new(7_u32)).unwrap();
        }
        assert_buffers_agree(&registry);
        registry.begin().replace(Arc::new(8_u32));
        assert_buffers_agree(&registry);
        assert_eq!(*registry.get_for(&StringKey(1)).unwrap(), "uno");
        assert_eq!(*registry.get_for(&StringKey(2)).unwrap(), "two");
        assert!(registry.get_for(&StringKey(3)).is_none());
        assert_eq!(*registry.get::<u32>().unwrap(), 8);
    }

    #[test]
    fn readers_are_held_off_for_the_swap_alone() {
        // A committing transaction on another thread holds the back mutex
        // throughout, and the front write lock only to swap; a read in
        // flight on this thread finishes either before or after, never
        // half way.
        let registry = Arc::new(Registry::new());
        registry.begin().insert(Arc::new(0_u32)).unwrap();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let writer = {
            let registry = Arc::clone(&registry);
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                for i in 1..=200_u32 {
                    let mut tx = registry.begin();
                    tx.replace(Arc::new(i));
                    tx.replace_for(StringKey(1), Arc::new(i.to_string()));
                }
                stop.store(true, std::sync::atomic::Ordering::SeqCst);
            })
        };
        let mut seen = 0;
        while !stop.load(std::sync::atomic::Ordering::SeqCst) {
            let n = *registry.get::<u32>().unwrap();
            assert!(n >= seen, "went backwards: {seen} then {n}");
            seen = n;
        }
        writer.join().unwrap();
        assert_eq!(*registry.get::<u32>().unwrap(), 200);
        assert_eq!(*registry.get_for(&StringKey(1)).unwrap(), "200");
    }
}
