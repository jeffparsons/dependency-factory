use std::sync::{Arc, Weak};

use crate::builder::Builder;
use crate::error::BuildError;
use crate::registry::{Key, Registry};

/// The owning factory. Holds its [`Registry`] alive; dropping it tears the
/// factory down. Most code should pass [`DependencyFactoryHandle`] around
/// instead, and reach for `DependencyFactory` only at the application's
/// top level where ownership lives.
///
/// The factory is one client of its registry: it reads the registry to
/// short-circuit and commits what it builds through
/// [`Transaction::get_or_insert_with`](crate::Transaction::get_or_insert_with).
/// Anything else may populate the
/// same registry by hand through [`registry`](Self::registry) and
/// [`Registry::begin`]; a value already present always wins over one the
/// factory would build.
///
/// Building goes through a [`Builder`], which holds a transaction on the
/// registry ([`Registry::begin`]) for as long as it exists.
/// [`get_or_build`](Self::get_or_build) takes one for the duration of the
/// call; [`builder`](Self::builder) hands one out to hold across several.
///
/// Every operation forwards to a [`DependencyFactoryHandle`]; the owning
/// type only adds the guarantee that the handle's upgrade succeeds.
pub struct DependencyFactory {
    registry: Arc<Registry>,
}

impl DependencyFactory {
    /// A factory over a fresh, empty registry.
    pub fn new() -> Self {
        Self::with_registry(Arc::new(Registry::new()))
    }

    /// A factory over an existing registry, for example one that has been
    /// populated by hand. Builds commit into it, see what is already
    /// there, and hold a transaction on it while they run.
    pub fn with_registry(registry: Arc<Registry>) -> Self {
        Self { registry }
    }

    /// The registry this factory builds into.
    pub fn registry(&self) -> &Arc<Registry> {
        &self.registry
    }

    /// A cheap-to-clone handle to this factory. Internally weak, so
    /// storing a handle inside a resource cannot keep the registry alive
    /// on its own and cannot create a reference cycle.
    pub fn handle(&self) -> DependencyFactoryHandle {
        DependencyFactoryHandle {
            registry: Arc::downgrade(&self.registry),
        }
    }

    /// Take exclusive access to build into this factory's registry,
    /// waiting for any other [`Builder`] or [`Transaction`] to end
    /// first. What the builder commits is published when it is dropped.
    ///
    /// [`Transaction`]: crate::Transaction
    pub fn builder(&self) -> Builder<'_> {
        Builder::new(&self.registry)
    }

    /// Return the cached singleton instance of `R`, if one is present.
    /// Never waits for a builder.
    pub fn get<R: Send + Sync + 'static>(&self) -> Option<Arc<R>> {
        self.registry.get::<R>()
    }

    /// Return the singleton instance of `R`, building it via `R::build`
    /// if it is not already cached. A cache miss takes a [`Builder`] for
    /// the duration of the call.
    pub fn get_or_build<R: Singleton>(&self) -> Result<Arc<R>, BuildError> {
        self.handle().get_or_build()
    }

    /// Return the cached value for `query`, if one is present. Never waits
    /// for a builder.
    pub fn get_for<Q: Query>(&self, query: &Q) -> Option<Arc<Q::Value>> {
        self.registry.get_for(query)
    }

    /// Return the value for `query`, building it via `Q::build` if it is
    /// not already cached. A cache miss takes a [`Builder`] for the
    /// duration of the call.
    pub fn get_or_build_for<Q: Query>(&self, query: Q) -> Result<Arc<Q::Value>, BuildError> {
        self.handle().get_or_build_for(query)
    }
}

impl Default for DependencyFactory {
    fn default() -> Self {
        Self::new()
    }
}

/// A cheap-to-clone handle to a [`DependencyFactory`]'s registry. Holds a
/// weak reference internally, so storing one inside a resource is always
/// safe and never creates a reference cycle. Operations return
/// [`BuildError::factory_dropped`] once the last `Arc` to the registry,
/// normally the owning [`DependencyFactory`], has been dropped.
///
/// To hold a [`Builder`] across several calls through a handle, upgrade
/// it with [`registry`](Self::registry) and pass the result to
/// [`Builder::new`].
#[derive(Clone)]
pub struct DependencyFactoryHandle {
    pub(crate) registry: Weak<Registry>,
}

impl DependencyFactoryHandle {
    fn upgrade(&self) -> Result<Arc<Registry>, BuildError> {
        self.registry
            .upgrade()
            .ok_or_else(BuildError::factory_dropped)
    }

    /// The registry behind this handle, or [`BuildError::factory_dropped`]
    /// if it is gone.
    pub fn registry(&self) -> Result<Arc<Registry>, BuildError> {
        self.upgrade()
    }

    /// Return the cached singleton instance of `R`, if one is present.
    /// `Ok(None)` means the registry is alive but no instance is cached;
    /// `Err(_)` means the registry itself has been dropped. Never waits
    /// for a builder.
    pub fn get<R: Send + Sync + 'static>(&self) -> Result<Option<Arc<R>>, BuildError> {
        Ok(self.upgrade()?.get::<R>())
    }

    /// Return the singleton instance of `R`, building it via `R::build`
    /// if it is not already cached. A cache miss takes a [`Builder`] for
    /// the duration of the call.
    pub fn get_or_build<R: Singleton>(&self) -> Result<Arc<R>, BuildError> {
        let registry = self.upgrade()?;
        if let Some(arc) = registry.get::<R>() {
            return Ok(arc);
        }
        Builder::new(&registry).get_or_build()
    }

    /// Return the cached value for `query`, if one is present.
    /// `Ok(None)` means the registry is alive but no value is cached for
    /// this key; `Err(_)` means the registry itself has been dropped.
    /// Never waits for a builder.
    pub fn get_for<Q: Query>(&self, query: &Q) -> Result<Option<Arc<Q::Value>>, BuildError> {
        Ok(self.upgrade()?.get_for(query))
    }

    /// Return the value for `query`, building it via `Q::build` if it is
    /// not already cached. A cache miss takes a [`Builder`] for the
    /// duration of the call.
    pub fn get_or_build_for<Q: Query>(&self, query: Q) -> Result<Arc<Q::Value>, BuildError> {
        let registry = self.upgrade()?;
        if let Some(arc) = registry.get_for(&query) {
            return Ok(arc);
        }
        Builder::new(&registry).get_or_build_for(query)
    }
}

/// A resource identified solely by its type. At most one instance per type
/// lives in a given registry. Implementors describe how to construct the
/// resource from a [`Builder`], which they use to resolve transitive
/// dependencies.
///
/// `build` must be cheap and side-effect-free: no I/O, no blocking, no
/// contended locking. Expensive setup belongs in async lifecycle hooks
/// (added later) or in the resource's own methods, not here. A resource
/// that needs to look things up after assembly can store
/// [`Builder::handle`].
pub trait Singleton: Send + Sync + Sized + 'static {
    fn build(builder: &mut Builder<'_>) -> Result<Self, BuildError>;
}

/// A keyed resource the factory can build: the implementor is the key
/// value, and [`Key::Value`] is the resource produced for it. Multiple
/// instances per value type may coexist, distinguished by the key value.
/// Calling `get_or_build_for(key)` on the factory caches the produced
/// value in the registry under that key.
///
/// `build` must be cheap and side-effect-free: no I/O, no blocking, no
/// contended locking. Expensive setup belongs in async lifecycle hooks
/// (added later) or in the resource's own methods, not here.
pub trait Query: Key {
    fn build(&self, builder: &mut Builder<'_>) -> Result<Self::Value, BuildError>;
}
