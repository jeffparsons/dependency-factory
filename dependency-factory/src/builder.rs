use std::any::{Any, type_name};
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::Arc;

use crate::error::{BuildError, CycleError, Frame};
use crate::factory::{DependencyFactoryHandle, Query, Singleton};
use crate::registry::{Key, Registry, SingletonKey, Transaction};

/// A key held type-erased, so that crumbs compare with `Eq`. Comparing
/// hashes would not do: unequal keys may hash alike, and the breadcrumb
/// has no bucket to fall back on the way a `HashMap` does.
trait ErasedKey: Any + Send + Sync {
    fn as_any(&self) -> &dyn Any;
    fn eq_erased(&self, other: &dyn ErasedKey) -> bool;
    /// The key as an error should show it, or `None` for a singleton,
    /// whose key says nothing the value type does not.
    fn describe(&self) -> Option<String>;
}

impl<Q: Query> ErasedKey for Q {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn eq_erased(&self, other: &dyn ErasedKey) -> bool {
        other
            .as_any()
            .downcast_ref::<Q>()
            .is_some_and(|other| self == other)
    }

    fn describe(&self) -> Option<String> {
        Some(format!("{self:?}"))
    }
}

impl<R: ?Sized + Send + Sync + 'static> ErasedKey for SingletonKey<R> {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn eq_erased(&self, other: &dyn ErasedKey) -> bool {
        other.as_any().is::<Self>()
    }

    fn describe(&self) -> Option<String> {
        None
    }
}

/// One entry on the builder's "currently building" breadcrumb. Carries
/// the key itself so different keys of the same key type are
/// distinguished, however they hash.
#[derive(Clone)]
struct Crumb {
    key: Arc<dyn ErasedKey>,
    type_name: &'static str,
}

impl Crumb {
    fn new<K: Key + ErasedKey>(key: &K) -> Self {
        Self {
            key: Arc::new(key.clone()),
            type_name: type_name::<K::Value>(),
        }
    }

    /// This crumb as an error reports it.
    fn frame(&self) -> Frame {
        Frame::new(self.type_name, self.key.describe())
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
    breadcrumb: Vec<Crumb>,
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
    pub fn get<R: ?Sized + Send + Sync + 'static>(&self) -> Option<Arc<R>> {
        self.transaction.get::<R>()
    }

    /// Return the cached value for `query`, if one is present, including
    /// one this builder has committed and not yet published.
    pub fn get_for<Q: Query>(&self, query: &Q) -> Option<Arc<Q::Value>> {
        self.transaction.get_for(query)
    }

    /// Return the singleton instance of `R`, building it via `R::build`
    /// and then running [`R::register`](Singleton::register) if it is not
    /// already cached. `R` may be a trait object with a [`Singleton`]
    /// impl.
    pub fn get_or_build<R: ?Sized + Singleton>(&mut self) -> Result<Arc<R>, BuildError> {
        self.get_or_build_with(SingletonKey::<R>::new(), R::build, R::register)
    }

    /// Return the value for `query`, building it via `Q::build` and then
    /// running [`Q::register`](Query::register) if it is not already
    /// cached.
    pub fn get_or_build_for<Q: Query>(&mut self, query: Q) -> Result<Arc<Q::Value>, BuildError> {
        self.get_or_build_with(
            query.clone(),
            |builder| query.build(builder),
            |arc, builder| query.register(arc, builder),
        )
    }

    /// `get_or_build` and `get_or_build_for`, which differ only in how
    /// the key is spelled: a cache hit returns at once; otherwise `build`
    /// runs under the key's frame, what it returns is cached under `key`,
    /// and `register` runs under the same frame.
    fn get_or_build_with<K: Key + ErasedKey>(
        &mut self,
        key: K,
        build: impl FnOnce(&mut Self) -> Result<Arc<K::Value>, BuildError>,
        register: impl FnOnce(&Arc<K::Value>, &mut Self) -> Result<(), BuildError>,
    ) -> Result<Arc<K::Value>, BuildError> {
        if let Some(arc) = self.transaction.get_for(&key) {
            return Ok(arc);
        }
        let crumb = Crumb::new(&key);
        let arc = self.with_frame(crumb.clone(), build)?;
        let arc = self.transaction.get_or_insert_with_for(key, || arc);
        self.with_frame(crumb, |builder| register(&arc, builder))?;
        Ok(arc)
    }

    /// Add `item` to the end of the index for `T`, as
    /// [`Transaction::index`] does. What `register` hooks call.
    pub fn index<T: ?Sized + Send + Sync + 'static>(&mut self, item: Arc<T>) {
        self.transaction.index(item);
    }

    /// Remove `item` from the index for `T`, as [`Transaction::unindex`]
    /// does.
    pub fn unindex<T: ?Sized + Send + Sync + 'static>(&mut self, item: Arc<T>) {
        self.transaction.unindex(item);
    }

    /// Return everything in the index for `T`, in the order it was added,
    /// including what this builder has added and not yet published. The
    /// list is a snapshot: later additions are not in it.
    pub fn indexed<T: ?Sized + Send + Sync + 'static>(&self) -> Vec<Arc<T>> {
        self.transaction.indexed::<T>()
    }

    /// Run a user `build` or `register` for `crumb` with the breadcrumb
    /// maintained around it: a cycle is reported before the call starts,
    /// an error is attributed to the crumb's frame, and the crumb is
    /// popped again whether the call returns or unwinds.
    fn with_frame<T>(
        &mut self,
        crumb: Crumb,
        build: impl FnOnce(&mut Self) -> Result<T, BuildError>,
    ) -> Result<T, BuildError> {
        if let Some(start) = self
            .breadcrumb
            .iter()
            .position(|c| c.key.eq_erased(&*crumb.key))
        {
            let mut path: Vec<Frame> = self.breadcrumb[start..].iter().map(Crumb::frame).collect();
            path.push(crumb.frame());
            return Err(CycleError::new(path).into());
        }
        let depth = self.breadcrumb.len();
        self.breadcrumb.push(crumb.clone());
        let result = catch_unwind(AssertUnwindSafe(|| build(self)));
        self.breadcrumb.truncate(depth);
        match result {
            Ok(result) => result.map_err(|e| e.push_frame(crumb.frame())),
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
                    .map(|crumb| crumb.frame().to_string())
                    .collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}
