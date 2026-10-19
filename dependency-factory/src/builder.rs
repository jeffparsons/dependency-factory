use std::any::TypeId;
use std::fmt;
use std::hash::{DefaultHasher, Hasher};
use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::Arc;

use crate::error::{BuildError, CycleError};
use crate::factory::{DependencyFactoryHandle, Query, Singleton};
use crate::registry::{Registry, SingletonKey, Transaction};

/// One entry on the builder's "currently building" breadcrumb. Carries a
/// hashed key value so different keys of the same key type are
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
            type_name: std::any::type_name::<Q::Value>(),
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
    /// if it is not already cached.
    pub fn get_or_build<R: Singleton>(&mut self) -> Result<Arc<R>, BuildError> {
        if let Some(arc) = self.get::<R>() {
            return Ok(arc);
        }
        let value = self.run_build(Frame::singleton::<R>(), R::build)?;
        Ok(self.transaction.get_or_insert_with(|| value))
    }

    /// Return the value for `query`, building it via `Q::build` if it is
    /// not already cached.
    pub fn get_or_build_for<Q: Query>(&mut self, query: Q) -> Result<Arc<Q::Value>, BuildError> {
        if let Some(arc) = self.get_for(&query) {
            return Ok(arc);
        }
        let value = self.run_build(Frame::query(&query), |builder| query.build(builder))?;
        Ok(self.transaction.get_or_insert_with_for(query, || value))
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
            .breadcrumb
            .iter()
            .position(|f| f.type_id == frame.type_id && f.key_hash == frame.key_hash)
        {
            let mut path: Vec<&'static str> = self.breadcrumb[start..]
                .iter()
                .map(|f| f.type_name)
                .collect();
            path.push(frame.type_name);
            return Err(CycleError::new(path).into());
        }
        let depth = self.breadcrumb.len();
        self.breadcrumb.push(frame);
        let result = catch_unwind(AssertUnwindSafe(|| build(self)));
        self.breadcrumb.truncate(depth);
        match result {
            Ok(result) => result.map_err(|e| e.push_frame(frame.type_name)),
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
