use std::ops::Deref;
use std::sync::{Arc, Weak};

use crate::builder::Builder;
use crate::error::BuildError;
use crate::registry::{Key, Registry};

/// A factory over a [`Registry`]: the registry, plus building. It derefs
/// to the registry, so reads ([`get`](Registry::get),
/// [`get_for`](Registry::get_for), [`indexed`](Registry::indexed)) and
/// hand writes ([`begin`](Registry::begin)) are the registry's own
/// methods; the factory adds [`get_or_build`](Self::get_or_build),
/// [`get_or_build_for`](Self::get_or_build_for) and
/// [`builder`](Self::builder). Dropping the last `DependencyFactory` for a
/// registry drops the registry too, unless someone else holds its `Arc`.
/// A factory is cheap to clone, and a clone is the same factory.
///
/// The factory is one client of its registry: it reads the registry to
/// short-circuit and commits what it builds through
/// [`Transaction::get_or_insert_with`](crate::Transaction::get_or_insert_with).
/// Anything else may populate the same registry by hand through
/// `factory.begin()`; a value already present always wins over one the
/// factory would build.
///
/// Building goes through a [`Builder`], which holds a transaction on the
/// registry ([`Registry::begin`]) for as long as it exists.
/// [`get_or_build`](Self::get_or_build) takes one for the duration of the
/// call; [`builder`](Self::builder) hands one out to hold across several.
///
/// Most code should hold a [`DependencyFactoryHandle`] rather than a
/// factory: a handle is weak, so a resource that stores one cannot keep
/// its own registry alive or form a reference cycle, and it upgrades to
/// a `DependencyFactory` for the scope that needs one.
#[derive(Clone)]
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

    /// The registry this factory builds into, as the `Arc` the factory
    /// holds. The factory derefs to the registry, so this is for sharing
    /// the `Arc` itself, say with [`Builder::new`].
    pub fn registry(&self) -> &Arc<Registry> {
        &self.registry
    }

    /// A weak handle to this factory, for resources that need to look
    /// things up after they are built. Storing a handle inside a resource
    /// cannot keep the registry alive on its own and cannot create a
    /// reference cycle.
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

    /// Return the singleton instance of `R`, building it via `R::build`
    /// if it is not already cached. A cache miss takes a [`Builder`] for
    /// the duration of the call.
    pub fn get_or_build<R: ?Sized + Singleton>(&self) -> Result<Arc<R>, BuildError> {
        if let Some(arc) = self.registry.get::<R>() {
            return Ok(arc);
        }
        self.builder().get_or_build()
    }

    /// Return the value for `query`, building it via `Q::build` if it is
    /// not already cached. A cache miss takes a [`Builder`] for the
    /// duration of the call.
    pub fn get_or_build_for<Q: Query>(&self, query: Q) -> Result<Arc<Q::Value>, BuildError> {
        if let Some(arc) = self.registry.get_for(&query) {
            return Ok(arc);
        }
        self.builder().get_or_build_for(query)
    }
}

impl Deref for DependencyFactory {
    type Target = Registry;

    fn deref(&self) -> &Registry {
        &self.registry
    }
}

impl Default for DependencyFactory {
    fn default() -> Self {
        Self::new()
    }
}

/// A weak, cheap-to-clone handle to a [`DependencyFactory`]'s registry.
/// Storing one inside a resource is always safe: it cannot keep the
/// registry alive and cannot create a reference cycle. It does one thing,
/// [`upgrade`](Self::upgrade) to a `DependencyFactory`, which fails once
/// the last `DependencyFactory` for the registry has been dropped.
///
/// ```
/// # use std::sync::Arc;
/// # use dependency_factory::{BuildError, Builder, DependencyFactory, DependencyFactoryHandle, Singleton};
/// # struct Config;
/// # impl Singleton for Config {
/// #     fn build(_: &mut Builder<'_>) -> Result<Arc<Self>, BuildError> { Ok(Arc::new(Config)) }
/// # }
/// struct Lazy {
///     factory: DependencyFactoryHandle,
/// }
///
/// impl Lazy {
///     fn config(&self) -> Option<Arc<Config>> {
///         // A read never waits; a build would take the registry's lock.
///         self.factory.upgrade()?.get::<Config>()
///     }
/// }
///
/// impl Singleton for Lazy {
///     fn build(builder: &mut Builder<'_>) -> Result<Arc<Self>, BuildError> {
///         Ok(Arc::new(Lazy { factory: builder.handle() }))
///     }
/// }
/// ```
#[derive(Clone)]
pub struct DependencyFactoryHandle {
    pub(crate) registry: Weak<Registry>,
}

impl DependencyFactoryHandle {
    /// The factory this handle was taken from, or `None` if its registry
    /// has been dropped. The result holds the registry alive for as long
    /// as it lives, so take one for the scope that needs it rather than
    /// storing it.
    pub fn upgrade(&self) -> Option<DependencyFactory> {
        self.registry
            .upgrade()
            .map(|registry| DependencyFactory { registry })
    }
}

/// A resource identified solely by its type. At most one instance per type
/// lives in a given registry. Implementors describe how to construct the
/// resource from a [`Builder`], which they use to resolve transitive
/// dependencies.
///
/// `build` returns the instance in an `Arc`, which is how the registry
/// holds it. Usually that is `Ok(Arc::new(..))` around a freshly built
/// value, and `#[derive(Singleton)]` writes exactly that. It need not be:
/// a build may hand over an `Arc` it already holds, say from a pool, use
/// [`Arc::new_cyclic`] to give the resource a `Weak` to itself, or
/// resolve another singleton and return that very `Arc`, so that two
/// slots share one instance. The last is how a trait object is bound to
/// an implementation; see below.
///
/// `build` must be cheap and side-effect-free: no I/O, no blocking, no
/// contended locking. Expensive setup belongs in the resource's own
/// methods, which the application calls when it is ready to; a resource
/// that should take part in a lifecycle event the application runs, such
/// as warming up or shutting down, makes itself findable by adding itself
/// to an index in [`register`](Self::register). A resource that needs to
/// look things up after assembly can store [`Builder::handle`].
///
/// # Trait objects
///
/// `Self` may be a trait object: `impl Singleton for dyn Mailer` binds
/// the trait to an implementation, and is what lets a resource depend on
/// `Arc<dyn Mailer>` rather than on `Arc<Smtp>`, and lets a test put
/// a fake in the slot before anything is built. The trait needs `Send`
/// and `Sync` as supertraits, as a trait used for an index does. The
/// usual body resolves the implementation's own singleton, so that both
/// slots share one instance and the implementation's `register` runs:
///
/// ```
/// use std::sync::Arc;
/// use dependency_factory::{BuildError, Builder, DependencyFactory, Singleton};
///
/// trait Mailer: Send + Sync {
///     fn transport(&self) -> &'static str;
/// }
///
/// struct Smtp;
///
/// impl Singleton for Smtp {
///     fn build(_: &mut Builder<'_>) -> Result<Arc<Self>, BuildError> {
///         Ok(Arc::new(Smtp))
///     }
/// }
///
/// impl Mailer for Smtp {
///     fn transport(&self) -> &'static str { "smtp" }
/// }
///
/// impl Singleton for dyn Mailer {
///     fn build(builder: &mut Builder<'_>) -> Result<Arc<Self>, BuildError> {
///         Ok(builder.get_or_build::<Smtp>()?)
///     }
/// }
///
/// struct Notifier {
///     mailer: Arc<dyn Mailer>,
/// }
///
/// impl Singleton for Notifier {
///     fn build(builder: &mut Builder<'_>) -> Result<Arc<Self>, BuildError> {
///         // Or `#[derive(Singleton)]`, which writes exactly this.
///         Ok(Arc::new(Notifier { mailer: builder.get_or_build()? }))
///     }
/// }
///
/// // In production the binding builds Smtp, once, for both slots.
/// let factory = DependencyFactory::new();
/// let notifier = factory.get_or_build::<Notifier>().unwrap();
/// assert_eq!(notifier.mailer.transport(), "smtp");
/// assert!(factory.get::<Smtp>().is_some());
///
/// // In a test a fake takes the slot, and Smtp is never built.
/// struct FakeMailer;
/// impl Mailer for FakeMailer {
///     fn transport(&self) -> &'static str { "fake" }
/// }
/// let factory = DependencyFactory::new();
/// factory.begin().insert::<dyn Mailer>(Arc::new(FakeMailer)).unwrap();
/// let notifier = factory.get_or_build::<Notifier>().unwrap();
/// assert_eq!(notifier.mailer.transport(), "fake");
/// assert!(factory.get::<Smtp>().is_none());
/// ```
///
/// The body may pick the implementation at run time, from configuration
/// it resolves through the builder.
pub trait Singleton: Send + Sync + 'static {
    /// The shared instance of `Self`, built or obtained through `builder`.
    /// Runs when the singleton slot for `Self` is empty; the result is
    /// cached there, so it runs at most once per registry.
    fn build(builder: &mut Builder<'_>) -> Result<Arc<Self>, BuildError>;

    /// Runs once, right after the `Arc` that `build` returned has been
    /// cached as `this`, with the builder still open and before
    /// `get_or_build` returns. It does not run for a value that was
    /// already present, or for one inserted by hand.
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
    ///     fn build(_: &mut Builder<'_>) -> Result<Arc<Self>, BuildError> {
    ///         Ok(Arc::new(Cache))
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
/// `build` returns the value in an `Arc`, as [`Singleton::build`] does
/// and with the same freedoms: usually `Ok(Arc::new(..))`, but an `Arc`
/// from anywhere will do, and `Value` may be a trait object.
///
/// `build` must be cheap and side-effect-free: no I/O, no blocking, no
/// contended locking. Expensive setup belongs in the value's own methods,
/// which the application calls when it is ready to; a value that should
/// take part in a lifecycle event adds itself to an index in
/// [`register`](Self::register).
pub trait Query: Key {
    /// The value for this key, built or obtained through `builder`. Runs
    /// when the slot for this key is empty; the result is cached there.
    fn build(&self, builder: &mut Builder<'_>) -> Result<Arc<Self::Value>, BuildError>;

    /// Runs once, right after the `Arc` that `build` returned has been
    /// cached as `value` under this key, with the builder still open and
    /// before `get_or_build_for` returns. It does not run for a value that
    /// was already present, or for one inserted by hand.
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
