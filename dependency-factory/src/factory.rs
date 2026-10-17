use std::any::TypeId;
use std::cell::RefCell;
use std::hash::{DefaultHasher, Hasher};
use std::sync::{Arc, Weak};

use crate::error::{BuildError, CycleError};
use crate::registry::{Key, Registry, SingletonKey};

thread_local! {
    static IN_PROGRESS: RefCell<Vec<Frame>> = const { RefCell::new(Vec::new()) };
}

/// One entry on the per-thread "currently building" breadcrumb. Carries a
/// registry identifier so frames produced by different factories on the
/// same call stack do not look like cycles, plus a hashed key value so
/// different keys of the same key type are distinguished.
#[derive(Clone, Copy)]
struct Frame {
    registry: usize,
    type_id: TypeId,
    key_hash: u64,
    type_name: &'static str,
}

impl Frame {
    fn singleton<R: 'static>(registry: &Arc<Registry>) -> Self {
        Self {
            registry: Arc::as_ptr(registry) as usize,
            type_id: TypeId::of::<SingletonKey<R>>(),
            key_hash: 0,
            type_name: std::any::type_name::<R>(),
        }
    }

    fn query<Q: Query>(registry: &Arc<Registry>, key: &Q) -> Self {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        Self {
            registry: Arc::as_ptr(registry) as usize,
            type_id: TypeId::of::<Q>(),
            key_hash: hasher.finish(),
            type_name: std::any::type_name::<Q::Value>(),
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
                f.registry == frame.registry
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

/// The owning factory. Holds its [`Registry`] alive; dropping it tears the
/// factory down. Most code should pass [`DependencyFactoryHandle`] around
/// instead, and reach for `DependencyFactory` only at the application's
/// top level where ownership lives.
///
/// The factory is one client of its registry: `get_or_build` reads the
/// registry to short-circuit and commits what it builds through
/// [`Registry::get_or_insert_with`]. Anything else may populate the same
/// registry directly via [`registry`](Self::registry); a value already
/// present always wins over one the factory would build.
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
    /// populated by hand. Builds commit into it and see what is already
    /// there. Use one factory per registry.
    pub fn with_registry(registry: Arc<Registry>) -> Self {
        Self { registry }
    }

    /// The registry this factory builds into.
    pub fn registry(&self) -> &Arc<Registry> {
        &self.registry
    }

    /// A cheap-to-clone handle to this factory. Internally weak, so
    /// storing a handle inside a resource cannot keep the factory alive
    /// on its own and cannot create a reference cycle.
    pub fn handle(&self) -> DependencyFactoryHandle {
        DependencyFactoryHandle {
            registry: Arc::downgrade(&self.registry),
        }
    }

    /// Return the cached singleton instance of `R`, if one is present.
    pub fn get<R: Send + Sync + 'static>(&self) -> Option<Arc<R>> {
        self.registry.get::<R>()
    }

    /// Return the singleton instance of `R`, building it via `R::build`
    /// if it is not already cached. Recursive `get_or_build` calls inside
    /// `R::build` see the same factory through a
    /// [`DependencyFactoryHandle`].
    pub fn get_or_build<R: Singleton>(&self) -> Result<Arc<R>, BuildError> {
        self.handle().get_or_build()
    }

    /// Return the cached value for `query`, if one is present.
    pub fn get_for<Q: Query>(&self, query: &Q) -> Option<Arc<Q::Value>> {
        self.registry.get_for(query)
    }

    /// Return the value for `query`, building it via `Q::build` if it is
    /// not already cached.
    pub fn get_or_build_for<Q: Query>(&self, query: Q) -> Result<Arc<Q::Value>, BuildError> {
        self.handle().get_or_build_for(query)
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
    registry: Weak<Registry>,
}

impl DependencyFactoryHandle {
    fn upgrade(&self) -> Result<Arc<Registry>, BuildError> {
        self.registry
            .upgrade()
            .ok_or_else(BuildError::factory_dropped)
    }

    /// The registry behind this handle, or [`BuildError::factory_dropped`]
    /// if the owning factory is gone.
    pub fn registry(&self) -> Result<Arc<Registry>, BuildError> {
        self.upgrade()
    }

    /// Return the cached singleton instance of `R`, if one is present.
    /// `Ok(None)` means the factory is alive but no instance is cached;
    /// `Err(_)` means the factory itself has been dropped.
    pub fn get<R: Send + Sync + 'static>(&self) -> Result<Option<Arc<R>>, BuildError> {
        Ok(self.upgrade()?.get::<R>())
    }

    /// Return the singleton instance of `R`, building it via `R::build`
    /// if it is not already cached. Returns
    /// [`BuildError::factory_dropped`] if the owning factory is gone.
    pub fn get_or_build<R: Singleton>(&self) -> Result<Arc<R>, BuildError> {
        let registry = self.upgrade()?;
        if let Some(arc) = registry.get::<R>() {
            return Ok(arc);
        }
        let _guard = CycleGuard::enter(Frame::singleton::<R>(&registry))?;
        let value = R::build(self).map_err(|e| e.push_frame(std::any::type_name::<R>()))?;
        Ok(registry.get_or_insert_with(|| value))
    }

    /// Return the cached value for `query`, if one is present.
    /// `Ok(None)` means the factory is alive but no value is cached for
    /// this key; `Err(_)` means the factory itself has been dropped.
    pub fn get_for<Q: Query>(&self, query: &Q) -> Result<Option<Arc<Q::Value>>, BuildError> {
        Ok(self.upgrade()?.get_for(query))
    }

    /// Return the value for `query`, building it via `Q::build` if it is
    /// not already cached. Returns [`BuildError::factory_dropped`] if the
    /// owning factory is gone.
    pub fn get_or_build_for<Q: Query>(&self, query: Q) -> Result<Arc<Q::Value>, BuildError> {
        let registry = self.upgrade()?;
        if let Some(arc) = registry.get_for(&query) {
            return Ok(arc);
        }
        let _guard = CycleGuard::enter(Frame::query(&registry, &query))?;
        let value = query
            .build(self)
            .map_err(|e| e.push_frame(std::any::type_name::<Q::Value>()))?;
        Ok(registry.get_or_insert_with_for(query, || value))
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
pub trait Singleton: Send + Sync + Sized + 'static {
    fn build(factory: &DependencyFactoryHandle) -> Result<Self, BuildError>;
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
    fn build(&self, factory: &DependencyFactoryHandle) -> Result<Self::Value, BuildError>;
}
