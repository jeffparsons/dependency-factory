use std::any::{Any, type_name};
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::Arc;

use crate::error::{BuildError, CycleError};
use crate::factory::{DependencyFactoryHandle, Query, Singleton};
use crate::registry::{Key, Registry, SingletonKey, Transaction};

/// A key held type-erased, so that frames compare with `Eq`. Comparing
/// hashes would not do: unequal keys may hash alike, and the breadcrumb
/// has no bucket to fall back on the way a `HashMap` does.
trait ErasedKey: Any + Send + Sync {
    fn as_any(&self) -> &dyn Any;
    fn eq_erased(&self, other: &dyn ErasedKey) -> bool;
}

impl<K: Key> ErasedKey for K {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn eq_erased(&self, other: &dyn ErasedKey) -> bool {
        other
            .as_any()
            .downcast_ref::<K>()
            .is_some_and(|other| self == other)
    }
}

/// One entry on the builder's "currently building" breadcrumb. Carries
/// the key itself so different keys of the same key type are
/// distinguished, however they hash.
#[derive(Clone)]
struct Frame {
    key: Arc<dyn ErasedKey>,
    type_name: &'static str,
}

impl Frame {
    fn new<K: Key>(key: &K) -> Self {
        Self {
            key: Arc::new(key.clone()),
            type_name: type_name::<K::Value>(),
        }
    }
}

/// Exclusive access to build resources into a registry.
///
/// A `Builder` is what [`Singleton::build`] and [`Query::build`] receive,
/// and what they pass down when they resolve their own dependencies. It is
/// a [`Transaction`] plus the record of which builds are in progress:
/// taking one waits for any other transaction on the registry to end, so
/// a `build` runs at most once per key, and every dependency cycle is seen
/// by a single builder and reported as a [`CycleError`].
///
/// What a builder commits is published to lookups outside it when the
/// builder is dropped, all at once. Until then only the builder itself
/// sees it. A `build` that returns an error ends that build alone, and
/// dependencies it had already built stay committed; a `build` that panics
/// unwinds through the builder, and its transaction is discarded.
///
/// Because the builder holds a transaction, and so the registry's lock,
/// across user code, builds of different keys in one registry never
/// overlap, and a thread that
/// blocks inside `build` on something another thread will only do after
/// taking a builder deadlocks. Keep `build` cheap, store dependencies
/// rather than calling into them, and resolve through the builder you were
/// given: a `get_or_build` through a stored [`DependencyFactoryHandle`] on
/// the same thread asks for the lock this builder holds and waits forever.
/// A `get` through a handle is fine; it reads what was last published
/// and never waits on a build.
pub struct Builder<'a> {
    transaction: Transaction<'a>,
    registry: &'a Arc<Registry>,
    /// Keys whose `build` is on this builder's call stack, outermost
    /// first.
    breadcrumb: Vec<Frame>,
}

impl<'a> Builder<'a> {
    /// Take exclusive access to build into `registry`, waiting for any
    /// other [`Builder`] or [`Transaction`] on it to end first.
    /// [`DependencyFactory::builder`](crate::DependencyFactory::builder)
    /// is this for the factory's own registry.
    pub fn new(registry: &'a Arc<Registry>) -> Self {
        Self {
            transaction: registry.begin(),
            registry,
            breadcrumb: Vec::new(),
        }
    }

    /// The registry this builder commits into. Reads on it never wait;
    /// writes on it take the lock this builder holds, so make those
    /// through the builder instead.
    pub fn registry(&self) -> &Arc<Registry> {
        self.registry
    }

    /// A cheap-to-clone, weak handle to the factory, for resources that
    /// need to look things up after they are built.
    pub fn handle(&self) -> DependencyFactoryHandle {
        DependencyFactoryHandle {
            registry: Arc::downgrade(self.registry),
        }
    }

    /// Return the cached singleton instance of `R`, if one is present,
    /// including one this builder has committed and not yet published.
    pub fn get<R: Send + Sync + 'static>(&self) -> Option<Arc<R>> {
        self.transaction.get::<R>()
    }

    /// Return the cached value for `query`, if one is present, including
    /// one this builder has committed and not yet published.
    pub fn get_for<Q: Query>(&self, query: &Q) -> Option<Arc<Q::Value>> {
        self.transaction.get_for(query)
    }

    /// Return the singleton instance of `R`, building it via `R::build`
    /// and then running [`R::register`](Singleton::register) if it is not
    /// already cached.
    pub fn get_or_build<R: Singleton>(&mut self) -> Result<Arc<R>, BuildError> {
        if let Some(arc) = self.get::<R>() {
            return Ok(arc);
        }
        let frame = Frame::new(&SingletonKey::<R>::new());
        let value = self.with_frame(frame.clone(), R::build)?;
        let arc = self.transaction.get_or_insert_with(|| value);
        self.with_frame(frame, |builder| R::register(&arc, builder))?;
        Ok(arc)
    }

    /// Return the value for `query`, building it via `Q::build` and then
    /// running [`Q::register`](Query::register) if it is not already
    /// cached.
    pub fn get_or_build_for<Q: Query>(&mut self, query: Q) -> Result<Arc<Q::Value>, BuildError> {
        if let Some(arc) = self.get_for(&query) {
            return Ok(arc);
        }
        let frame = Frame::new(&query);
        let value = self.with_frame(frame.clone(), |builder| query.build(builder))?;
        let arc = self
            .transaction
            .get_or_insert_with_for(query.clone(), || value);
        self.with_frame(frame, |builder| query.register(&arc, builder))?;
        Ok(arc)
    }

    /// Add `item` to the end of the index for `T`, as
    /// [`Transaction::index`] does. What `register` hooks call.
    pub fn index<T: ?Sized + Send + Sync + 'static>(&mut self, item: Arc<T>) {
        self.transaction.index(item);
    }

    /// Return everything in the index for `T`, in the order it was added,
    /// including what this builder has added and not yet published. The
    /// list is a snapshot: later additions are not in it.
    pub fn indexed<T: ?Sized + Send + Sync + 'static>(&self) -> Vec<Arc<T>> {
        self.transaction.indexed::<T>()
    }

    /// Run a user `build` or `register` for `frame` with the breadcrumb
    /// maintained around it: a cycle is reported before the call starts,
    /// an error is attributed to the frame, and the frame is popped again
    /// whether the call returns or unwinds.
    fn with_frame<T>(
        &mut self,
        frame: Frame,
        build: impl FnOnce(&mut Self) -> Result<T, BuildError>,
    ) -> Result<T, BuildError> {
        if let Some(start) = self
            .breadcrumb
            .iter()
            .position(|f| f.key.eq_erased(&*frame.key))
        {
            let mut path: Vec<&'static str> = self.breadcrumb[start..]
                .iter()
                .map(|f| f.type_name)
                .collect();
            path.push(frame.type_name);
            return Err(CycleError::new(path).into());
        }
        let depth = self.breadcrumb.len();
        let frame_name = frame.type_name;
        self.breadcrumb.push(frame);
        let result = catch_unwind(AssertUnwindSafe(|| build(self)));
        self.breadcrumb.truncate(depth);
        match result {
            Ok(result) => result.map_err(|e| e.push_frame(frame_name)),
            Err(payload) => resume_unwind(payload),
        }
    }
}

impl fmt::Debug for Builder<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Builder")
            .field(
                "building",
                &self
                    .breadcrumb
                    .iter()
                    .map(|frame| frame.type_name)
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}
