use std::fmt;
use std::hash::Hash;
use std::marker::PhantomData;
use std::mem;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::thread;

use crate::error::OccupiedError;
use crate::store::Store;

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
/// [`Key`] value (`insert_for`, `get_for`, ...).
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
    pub fn get<R: Send + Sync + 'static>(&self) -> Option<Arc<R>> {
        self.get_for(&SingletonKey::<R>::new())
    }

    /// Return the resource stored under `key`, if one is present.
    pub fn get_for<K: Key>(&self, key: &K) -> Option<Arc<K::Value>> {
        self.front
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
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
/// Writes come in three flavours. `insert` fills an empty slot and refuses
/// an occupied one; `get_or_insert_with` fills an empty slot and otherwise
/// returns what is there; `replace` overwrites. Replacing does not affect
/// anyone already holding the previous `Arc`.
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
    pub fn get<R: Send + Sync + 'static>(&self) -> Option<Arc<R>> {
        self.get_for(&SingletonKey::<R>::new())
    }

    /// Store `value` as the singleton instance of `R`, if there is none
    /// yet. Returns the cached `Arc<R>`, or the rejected `value` wrapped in
    /// an [`OccupiedError`] if an instance was already present.
    pub fn insert<R: Send + Sync + 'static>(
        &mut self,
        value: R,
    ) -> Result<Arc<R>, OccupiedError<R>> {
        self.insert_for(SingletonKey::<R>::new(), value)
    }

    /// Store `value` as the singleton instance of `R`, overwriting any
    /// instance already present. Returns the new `Arc<R>`.
    pub fn replace<R: Send + Sync + 'static>(&mut self, value: R) -> Arc<R> {
        self.replace_for(SingletonKey::<R>::new(), value)
    }

    /// Return the singleton instance of `R`, storing the result of `make`
    /// first if there is none. `make` is not called when an instance is
    /// already present.
    pub fn get_or_insert_with<R: Send + Sync + 'static>(
        &mut self,
        make: impl FnOnce() -> R,
    ) -> Arc<R> {
        self.get_or_insert_with_for(SingletonKey::<R>::new(), make)
    }

    /// Return the resource stored under `key`, if one is present.
    pub fn get_for<K: Key>(&self, key: &K) -> Option<Arc<K::Value>> {
        self.overlay.get(key).or_else(|| self.back.get(key))
    }

    /// Store `value` under `key`, if the slot is empty. Returns the cached
    /// `Arc`, or the rejected `value` wrapped in an [`OccupiedError`] if
    /// the slot was already filled.
    pub fn insert_for<K: Key>(
        &mut self,
        key: K,
        value: K::Value,
    ) -> Result<Arc<K::Value>, OccupiedError<K::Value>> {
        if self.get_for(&key).is_some() {
            return Err(OccupiedError::new(value));
        }
        Ok(self.replace_for(key, value))
    }

    /// Store `value` under `key`, overwriting any resource already there.
    /// Returns the new `Arc`.
    pub fn replace_for<K: Key>(&mut self, key: K, value: K::Value) -> Arc<K::Value> {
        let arc = Arc::new(value);
        self.overlay.insert(key, Arc::clone(&arc));
        arc
    }

    /// Return the resource stored under `key`, storing the result of `make`
    /// first if the slot is empty. `make` is not called when the slot is
    /// already filled.
    pub fn get_or_insert_with_for<K: Key>(
        &mut self,
        key: K,
        make: impl FnOnce() -> K::Value,
    ) -> Arc<K::Value> {
        match self.get_for(&key) {
            Some(arc) => arc,
            None => self.replace_for(key, make()),
        }
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
        let inserted = registry.begin().insert(42_u32).unwrap();
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
        registry.begin().insert(1_u32).unwrap();
        registry.begin().insert(2_u64).unwrap();
        assert_eq!(*registry.get::<u32>().unwrap(), 1);
        assert_eq!(*registry.get::<u64>().unwrap(), 2);
    }

    #[test]
    fn insert_refuses_an_occupied_slot() {
        let registry = Registry::new();
        registry.begin().insert(1_u32).unwrap();
        let err = registry.begin().insert(2_u32).unwrap_err();
        assert_eq!(err.into_value(), 2);
        assert_eq!(*registry.get::<u32>().unwrap(), 1);
    }

    #[test]
    fn replace_overwrites_an_occupied_slot() {
        let registry = Registry::new();
        let first = registry.begin().insert(1_u32).unwrap();
        let second = registry.begin().replace(2_u32);
        assert_eq!(*registry.get::<u32>().unwrap(), 2);
        // The previous Arc is unaffected for anyone still holding it.
        assert_eq!(*first, 1);
        assert!(Arc::ptr_eq(&second, &registry.get::<u32>().unwrap()));
    }

    #[test]
    fn get_or_insert_with_fills_an_empty_slot_only() {
        let registry = Registry::new();
        let made = registry.begin().get_or_insert_with(|| 1_u32);
        assert_eq!(*made, 1);
        let kept = registry
            .begin()
            .get_or_insert_with(|| -> u32 { panic!("must not be called") });
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
            .begin()
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
        registry
            .begin()
            .insert_for(StringKey(1), "one".into())
            .unwrap();
        registry
            .begin()
            .insert_for(StringKey(2), "two".into())
            .unwrap();
        assert_eq!(*registry.get_for(&StringKey(1)).unwrap(), "one");
        assert_eq!(*registry.get_for(&StringKey(2)).unwrap(), "two");
    }

    #[test]
    fn insert_for_refuses_an_occupied_slot() {
        let registry = Registry::new();
        registry
            .begin()
            .insert_for(StringKey(1), "first".into())
            .unwrap();
        let err = registry
            .begin()
            .insert_for(StringKey(1), "second".into())
            .unwrap_err();
        assert_eq!(err.into_value(), "second");
        assert_eq!(*registry.get_for(&StringKey(1)).unwrap(), "first");
    }

    #[test]
    fn replace_for_overwrites_an_occupied_slot() {
        let registry = Registry::new();
        registry
            .begin()
            .insert_for(StringKey(1), "first".into())
            .unwrap();
        registry.begin().replace_for(StringKey(1), "second".into());
        assert_eq!(*registry.get_for(&StringKey(1)).unwrap(), "second");
    }

    #[test]
    fn get_or_insert_with_for_fills_an_empty_slot_only() {
        let registry = Registry::new();
        let made = registry
            .begin()
            .get_or_insert_with_for(StringKey(3), || "three".into());
        let kept = registry
            .begin()
            .get_or_insert_with_for(StringKey(3), || panic!("must not be called"));
        assert!(Arc::ptr_eq(&made, &kept));
    }

    #[test]
    fn occupied_error_is_debug_without_a_debug_value() {
        struct Opaque;
        let registry = Registry::new();
        registry.begin().insert(Opaque).unwrap();
        let err = registry
            .begin()
            .insert(Opaque)
            .err()
            .expect("slot is occupied");
        let rendered = format!("{err:?}");
        assert!(rendered.contains("Opaque"), "{rendered}");
    }

    // --- Transactions ------------------------------------------------------------

    #[test]
    fn a_transaction_sees_its_own_writes_before_they_are_published() {
        let registry = Registry::new();
        let mut tx = registry.begin();
        let inserted = tx.insert(1_u32).unwrap();
        assert!(Arc::ptr_eq(&inserted, &tx.get::<u32>().unwrap()));
        let err = tx.insert(2_u32).unwrap_err();
        assert_eq!(err.into_value(), 2);
        assert_eq!(*tx.get_or_insert_with(|| 3_u32), 1);
        tx.replace(4_u32);
        assert_eq!(*tx.get::<u32>().unwrap(), 4);
    }

    #[test]
    fn a_transaction_publishes_its_writes_together_when_dropped() {
        let registry = Registry::new();
        let mut tx = registry.begin();
        tx.insert(1_u32).unwrap();
        tx.insert(2_u64).unwrap();
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
        registry.begin().insert(1_u32).unwrap();
        let (held_tx, held_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel::<()>();

        let holder = {
            let registry = Arc::clone(&registry);
            thread::spawn(move || {
                let mut tx = registry.begin();
                tx.replace(2_u32);
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
            thread::spawn(move || registry.begin().replace(3_u32))
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
        registry.begin().insert(0_u32).unwrap();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut tx = registry.begin();
            tx.replace(1_u32);
            tx.insert(1_u64).unwrap();
            panic!("inside the transaction");
        }));
        assert!(outcome.is_err());
        // Nothing from the transaction was published...
        assert_eq!(*registry.get::<u32>().unwrap(), 0);
        assert!(registry.get::<u64>().is_none());
        // ...and the registry is usable again.
        registry.begin().insert(2_u64).unwrap();
        assert_eq!(*registry.get::<u64>().unwrap(), 2);
    }

    #[test]
    fn commit_publishes_and_discard_does_not() {
        let registry = Registry::new();
        registry.begin().insert(0_u32).unwrap();

        let mut tx = registry.begin();
        tx.replace(1_u32);
        tx.insert(1_u64).unwrap();
        tx.discard();
        assert_eq!(*registry.get::<u32>().unwrap(), 0);
        assert!(registry.get::<u64>().is_none());

        let mut tx = registry.begin();
        tx.replace(2_u32);
        tx.insert(2_u64).unwrap();
        tx.commit();
        assert_eq!(*registry.get::<u32>().unwrap(), 2);
        assert_eq!(*registry.get::<u64>().unwrap(), 2);
    }
    // --- The overlay and the two buffers -------------------------------------

    #[test]
    fn a_transaction_shadows_a_published_entry_until_it_commits() {
        let registry = Registry::new();
        registry.begin().insert(1_u32).unwrap();
        let mut tx = registry.begin();
        tx.replace(2_u32);
        assert_eq!(*tx.get::<u32>().unwrap(), 2);
        assert_eq!(*registry.get::<u32>().unwrap(), 1);
        drop(tx);
        assert_eq!(*registry.get::<u32>().unwrap(), 2);
    }

    #[test]
    fn a_transaction_refuses_a_slot_filled_before_it_began() {
        let registry = Registry::new();
        registry.begin().insert(1_u32).unwrap();
        let mut tx = registry.begin();
        assert_eq!(tx.insert(2_u32).unwrap_err().into_value(), 2);
        assert_eq!(*tx.get_or_insert_with(|| 3_u32), 1);
    }

    #[test]
    fn a_transaction_that_only_reads_publishes_nothing() {
        let registry = Registry::new();
        registry.begin().insert(1_u32).unwrap();
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
            .insert_for(StringKey(1), "one".into())
            .unwrap();
        assert_buffers_agree(&registry);
        {
            let mut tx = registry.begin();
            tx.insert_for(StringKey(2), "two".into()).unwrap();
            tx.replace_for(StringKey(1), "uno".into());
            tx.insert(7_u32).unwrap();
        }
        assert_buffers_agree(&registry);
        registry.begin().replace(8_u32);
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
        registry.begin().insert(0_u32).unwrap();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let writer = {
            let registry = Arc::clone(&registry);
            let stop = Arc::clone(&stop);
            thread::spawn(move || {
                for i in 1..=200_u32 {
                    let mut tx = registry.begin();
                    tx.replace(i);
                    tx.replace_for(StringKey(1), i.to_string());
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
