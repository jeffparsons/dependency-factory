mod builder;
mod error;
mod factory;
mod registry;
mod store;

pub use builder::Builder;
pub use error::{BuildError, CycleError, Frame, OccupiedError};
pub use factory::{DependencyFactory, DependencyFactoryHandle, Query, Singleton};
pub use registry::{Key, Registry, Transaction};

/// Derive [`Singleton`](trait@Singleton) for a struct whose fields are
/// built from the factory.
///
/// Each field is filled in the order it is written, so dependencies are
/// built, and added to indexes, in that order. A field with no attribute
/// is an `Arc<R>` of another singleton, from
/// [`Builder::get_or_build`]. A `#[factory(...)]` attribute on a field
/// says otherwise, with one of:
///
/// - **`query = key_fn`**: the value for the key `key_fn(builder)`
///   returns, from [`Builder::get_or_build_for`].
/// - **`default`**: `Default::default()`, for state that is not a
///   dependency.
/// - **`build = build_fn`**: whatever `build_fn(builder)` returns, for
///   anything else.
///
/// `key_fn` and `build_fn` take `&mut Builder` and return a `Result`
/// whose error `?` converts into a [`BuildError`]; an error ends the
/// build, attributed to the struct.
///
/// On the struct, `#[factory(index(dyn Trait, ...))]` writes a
/// [`register`](Singleton::register) that adds the value to the index for
/// each trait named, in order.
///
/// ```
/// use std::sync::{Arc, OnceLock};
/// use dependency_factory::{BuildError, Builder, DependencyFactory, Singleton};
///
/// trait WarmUp: Send + Sync {
///     fn warm_up(&self);
/// }
///
/// struct Config {
///     region: String,
/// }
///
/// impl Singleton for Config {
///     fn build(_: &mut Builder) -> Result<Self, BuildError> {
///         Ok(Config { region: "north".into() })
///     }
/// }
///
/// fn database_file(builder: &mut Builder) -> Result<String, BuildError> {
///     let config = builder.get_or_build::<Config>()?;
///     Ok(format!("geoip-{}.mmdb", config.region))
/// }
///
/// #[derive(Singleton)]
/// #[factory(index(dyn WarmUp))]
/// struct GeoIp {
///     config: Arc<Config>,
///     #[factory(build = database_file)]
///     file: String,
///     #[factory(default)]
///     warmed: OnceLock<()>,
/// }
///
/// impl WarmUp for GeoIp {
///     fn warm_up(&self) {
///         self.warmed.get_or_init(|| { /* load self.file */ });
///     }
/// }
///
/// let factory = DependencyFactory::new();
/// let geo_ip = factory.get_or_build::<GeoIp>().unwrap();
/// assert_eq!(geo_ip.file, "geoip-north.mmdb");
/// for resource in factory.indexed::<dyn WarmUp>() {
///     resource.warm_up();
/// }
/// ```
///
/// A field takes only one of `query`, `default` and `build`:
///
/// ```compile_fail
/// # use dependency_factory::{BuildError, Builder, Singleton};
/// # fn zero(_: &mut Builder) -> Result<u32, BuildError> { Ok(0) }
/// #[derive(Singleton)]
/// struct Counter {
///     #[factory(default, build = zero)]
///     count: u32,
/// }
/// ```
///
/// and `default` takes no value; a function goes in `build`:
///
/// ```compile_fail
/// # use dependency_factory::Singleton;
/// # fn zero() -> u32 { 0 }
/// #[derive(Singleton)]
/// struct Counter {
///     #[factory(default = zero)]
///     count: u32,
/// }
/// ```
#[cfg(feature = "derive")]
pub use dependency_factory_derive::Singleton;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn factory_reads_its_registry() {
        let factory = DependencyFactory::new();
        assert!(factory.get::<u32>().is_none());
        factory.registry().begin().insert(42_u32).unwrap();
        assert_eq!(*factory.get::<u32>().unwrap(), 42);
    }

    #[test]
    fn factory_over_a_prepopulated_registry() {
        let registry = Arc::new(Registry::new());
        registry.begin().insert(42_u32).unwrap();
        let factory = DependencyFactory::with_registry(Arc::clone(&registry));
        assert!(Arc::ptr_eq(factory.registry(), &registry));
        assert_eq!(*factory.get::<u32>().unwrap(), 42);
    }

    #[test]
    fn handle_can_resolve() {
        let factory = DependencyFactory::new();
        factory.registry().begin().insert(42_u32).unwrap();
        let handle = factory.handle();
        assert_eq!(*handle.get::<u32>().unwrap().unwrap(), 42);
    }

    #[test]
    fn handle_exposes_the_registry() {
        let factory = DependencyFactory::new();
        let handle = factory.handle();
        handle.registry().unwrap().begin().insert(42_u32).unwrap();
        assert_eq!(*factory.get::<u32>().unwrap(), 42);
    }

    #[test]
    fn handle_after_factory_dropped_errors() {
        let factory = DependencyFactory::new();
        let handle = factory.handle();
        drop(factory);
        assert!(handle.get::<u32>().is_err());
        assert!(handle.registry().is_err());
    }

    #[test]
    fn handle_clone_is_cheap_and_independent() {
        let factory = DependencyFactory::new();
        let h1 = factory.handle();
        let h2 = h1.clone();
        h1.registry().unwrap().begin().insert(1_u32).unwrap();
        assert_eq!(*h2.get::<u32>().unwrap().unwrap(), 1);
    }

    #[derive(Debug, Hash, Eq, PartialEq, Clone)]
    struct StringKey(u32);

    impl Key for StringKey {
        type Value = String;
    }

    impl Query for StringKey {
        fn build(&self, _builder: &mut Builder) -> Result<String, BuildError> {
            Ok(format!("built-{}", self.0))
        }
    }

    #[test]
    fn get_for_reads_the_registry() {
        let factory = DependencyFactory::new();
        assert!(factory.get_for(&StringKey(7)).is_none());
        let inserted = factory
            .registry()
            .begin()
            .insert_for(StringKey(7), "seven".to_string())
            .unwrap();
        let got = factory.get_for(&StringKey(7)).unwrap();
        assert!(Arc::ptr_eq(&inserted, &got));
    }

    #[test]
    fn build_for_uses_query_build_when_absent() {
        let factory = DependencyFactory::new();
        let built = factory.get_or_build_for(StringKey(3)).unwrap();
        assert_eq!(*built, "built-3");
    }

    #[test]
    fn build_for_caches_results() {
        let factory = DependencyFactory::new();
        let a = factory.get_or_build_for(StringKey(3)).unwrap();
        let b = factory.get_or_build_for(StringKey(3)).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn inserted_value_short_circuits_build_for() {
        let factory = DependencyFactory::new();
        let inserted = factory
            .registry()
            .begin()
            .insert_for(StringKey(3), "by hand".into())
            .unwrap();
        let built = factory.get_or_build_for(StringKey(3)).unwrap();
        assert!(Arc::ptr_eq(&inserted, &built));
    }

    #[test]
    fn handle_can_resolve_queries() {
        let factory = DependencyFactory::new();
        factory
            .registry()
            .begin()
            .insert_for(StringKey(1), "one".into())
            .unwrap();
        let handle = factory.handle();
        assert_eq!(*handle.get_for(&StringKey(1)).unwrap().unwrap(), "one");
        let built = handle.get_or_build_for(StringKey(2)).unwrap();
        assert_eq!(*built, "built-2");
    }

    #[test]
    fn handle_after_factory_dropped_errors_for_queries() {
        let factory = DependencyFactory::new();
        let handle = factory.handle();
        drop(factory);
        assert!(handle.get_for(&StringKey(1)).is_err());
        assert!(handle.get_or_build_for(StringKey(1)).is_err());
    }
}
