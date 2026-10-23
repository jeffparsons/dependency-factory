use std::sync::{Arc, Weak};

use crate::builder::Builder;
use crate::error::{BuildError, ResolveError};
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
    /// The builder's methods return a [`BuildError`], for use inside
    /// `build`; here, convert one with [`ResolveError::from`] where you
    /// need a standard error.
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
    pub fn get_or_build<R: Singleton>(&self) -> Result<Arc<R>, ResolveError> {
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
    pub fn get_or_build_for<Q: Query>(&self, query: Q) -> Result<Arc<Q::Value>, ResolveError> {
        self.handle().get_or_build_for(query)
    }

    /// Return everything in the index for `T`, in the order it was added;
    /// see [`Registry::indexed`]. Never waits for a builder.
    pub fn indexed<T: ?Sized + Send + Sync + 'static>(&self) -> Vec<Arc<T>> {
        self.registry.indexed::<T>()
    }
}

impl Default for DependencyFactory {
    fn default() -> Self {
        Self::new()
    }
}

/// A cheap-to-clone handle to a [`DependencyFactory`]'s registry. Holds a
/// weak reference internally, so storing one inside a resource is always
/// safe and never creates a reference cycle. Operations return a
/// [`ResolveError`] wrapping [`BuildError::factory_dropped`] once the last
/// `Arc` to the registry, normally the owning [`DependencyFactory`], has
/// been dropped.
///
/// To hold a [`Builder`] across several calls through a handle, upgrade
/// it with [`registry`](Self::registry) and pass the result to
/// [`Builder::new`].
#[derive(Clone)]
pub struct DependencyFactoryHandle {
    pub(crate) registry: Weak<Registry>,
}

impl DependencyFactoryHandle {
    fn upgrade(&self) -> Result<Arc<Registry>, ResolveError> {
        self.registry
            .upgrade()
            .ok_or_else(|| BuildError::factory_dropped().into())
    }

    /// The registry behind this handle, or an error wrapping
    /// [`BuildError::factory_dropped`] if it is gone.
    pub fn registry(&self) -> Result<Arc<Registry>, ResolveError> {
        self.upgrade()
    }

    /// Return the cached singleton instance of `R`, if one is present.
    /// `Ok(None)` means the registry is alive but no instance is cached;
    /// `Err(_)` means the registry itself has been dropped. Never waits
    /// for a builder.
    pub fn get<R: Send + Sync + 'static>(&self) -> Result<Option<Arc<R>>, ResolveError> {
        Ok(self.upgrade()?.get::<R>())
    }

    /// Return the singleton instance of `R`, building it via `R::build`
    /// if it is not already cached. A cache miss takes a [`Builder`] for
    /// the duration of the call.
    pub fn get_or_build<R: Singleton>(&self) -> Result<Arc<R>, ResolveError> {
        let registry = self.upgrade()?;
        if let Some(arc) = registry.get::<R>() {
            return Ok(arc);
        }
        Ok(Builder::new(&registry).get_or_build()?)
    }

    /// Return the cached value for `query`, if one is present.
    /// `Ok(None)` means the registry is alive but no value is cached for
    /// this key; `Err(_)` means the registry itself has been dropped.
    /// Never waits for a builder.
    pub fn get_for<Q: Query>(&self, query: &Q) -> Result<Option<Arc<Q::Value>>, ResolveError> {
        Ok(self.upgrade()?.get_for(query))
    }

    /// Return the value for `query`, building it via `Q::build` if it is
    /// not already cached. A cache miss takes a [`Builder`] for the
    /// duration of the call.
    pub fn get_or_build_for<Q: Query>(&self, query: Q) -> Result<Arc<Q::Value>, ResolveError> {
        let registry = self.upgrade()?;
        if let Some(arc) = registry.get_for(&query) {
            return Ok(arc);
        }
        Ok(Builder::new(&registry).get_or_build_for(query)?)
    }

    /// Return everything in the index for `T`, in the order it was added;
    /// see [`Registry::indexed`]. `Err(_)` means the registry itself has
    /// been dropped. Never waits for a builder.
    pub fn indexed<T: ?Sized + Send + Sync + 'static>(&self) -> Result<Vec<Arc<T>>, ResolveError> {
        Ok(self.upgrade()?.indexed::<T>())
    }
}

/// A resource identified solely by its type. At most one instance per type
/// lives in a given registry. Implementors describe how to construct the
/// resource from a [`Builder`], which they use to resolve transitive
/// dependencies.
///
/// `build` must be cheap and side-effect-free: no I/O, no blocking, no
/// contended locking. Expensive setup belongs in the resource's own
/// methods, which the application calls when it is ready to; a resource
/// that should take part in a lifecycle event the application runs, such
/// as warming up or shutting down, makes itself findable by adding itself
/// to an index in [`register`](Self::register). A resource that needs to
/// look things up after assembly can store [`Builder::handle`].
pub trait Singleton: Send + Sync + Sized + 'static {
    fn build(builder: &mut Builder<'_>) -> Result<Self, BuildError>;

    /// Runs once, right after the value `build` returned has been cached
    /// as `this`, with the builder still open and before `get_or_build`
    /// returns. It does not run for a value that was already present, or
    /// for one inserted by hand.
    ///
    /// The usual body adds `this` to the indexes the resource belongs to,
    /// which is where the unsizing from `Arc<Self>` to `Arc<dyn Trait>`
    /// happens, so a resource can be found later by trait without the
    /// registry knowing its concrete type:
    ///
    /// ```
    /// # use std::sync::Arc;
    /// # use dependency_factory::{BuildError, Builder, Singleton};
    /// # trait WarmUp: Send + Sync { fn warm_up(&self); }
    /// # struct Cache;
    /// # impl WarmUp for Cache { fn warm_up(&self) {} }
    /// impl Singleton for Cache {
    ///     fn build(_: &mut Builder<'_>) -> Result<Self, BuildError> {
    ///         Ok(Cache)
    ///     }
    ///
    ///     fn register(this: &Arc<Self>, builder: &mut Builder<'_>) -> Result<(), BuildError> {
    ///         builder.index::<dyn WarmUp>(this.clone());
    ///         Ok(())
    ///     }
    /// }
    /// ```
    ///
    /// `#[derive(Singleton)]` writes this body from a
    /// `#[factory(index(dyn WarmUp, ...))]` attribute on the struct.
    ///
    /// The builder is the same one `build` had, so `register` may resolve
    /// other resources through it, with the same rules. An error ends the
    /// `get_or_build` that was in progress, attributed to this type like
    /// an error from `build`; the value stays cached, as dependencies a
    /// failed `build` had already built do. The default does nothing.
    fn register(this: &Arc<Self>, builder: &mut Builder<'_>) -> Result<(), BuildError> {
        let _ = (this, builder);
        Ok(())
    }
}

/// A keyed resource the factory can build: the implementor is the key
/// value, and [`Key::Value`] is the resource produced for it. Multiple
/// instances per value type may coexist, distinguished by the key value.
/// Calling `get_or_build_for(key)` on the factory caches the produced
/// value in the registry under that key.
///
/// `build` must be cheap and side-effect-free: no I/O, no blocking, no
/// contended locking. Expensive setup belongs in the value's own methods,
/// which the application calls when it is ready to; a value that should
/// take part in a lifecycle event adds itself to an index in
/// [`register`](Self::register).
pub trait Query: Key {
    fn build(&self, builder: &mut Builder<'_>) -> Result<Self::Value, BuildError>;

    /// Runs once, right after the value `build` returned has been cached
    /// as `value` under this key, with the builder still open and before
    /// `get_or_build_for` returns. It does not run for a value that was
    /// already present, or for one inserted by hand.
    ///
    /// The same as [`Singleton::register`], with the key to hand: the
    /// usual body adds `value` to the indexes it belongs to, as
    /// `builder.index::<dyn Trait>(value.clone())`. An error ends the
    /// `get_or_build_for` in progress, attributed to the value type; the
    /// value stays cached. The default does nothing.
    fn register(
        &self,
        value: &Arc<Self::Value>,
        builder: &mut Builder<'_>,
    ) -> Result<(), BuildError> {
        let _ = (value, builder);
        Ok(())
    }
}
