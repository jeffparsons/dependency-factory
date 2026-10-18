mod error;
mod factory;
mod registry;

pub use error::{BuildError, CycleError, OccupiedError};
pub use factory::{DependencyFactory, DependencyFactoryHandle, Query, Singleton};
pub use registry::{Key, Registry};

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
        factory.registry().insert(42_u32).unwrap();
        assert_eq!(*factory.get::<u32>().unwrap(), 42);
    }

    #[test]
    fn factory_over_a_prepopulated_registry() {
        let registry = Arc::new(Registry::new());
        registry.insert(42_u32).unwrap();
        let factory = DependencyFactory::with_registry(Arc::clone(&registry));
        assert!(Arc::ptr_eq(factory.registry(), &registry));
        assert_eq!(*factory.get::<u32>().unwrap(), 42);
    }

    #[test]
    fn handle_can_resolve() {
        let factory = DependencyFactory::new();
        factory.registry().insert(42_u32).unwrap();
        let handle = factory.handle();
        assert_eq!(*handle.get::<u32>().unwrap().unwrap(), 42);
    }

    #[test]
    fn handle_exposes_the_registry() {
        let factory = DependencyFactory::new();
        let handle = factory.handle();
        handle.registry().unwrap().insert(42_u32).unwrap();
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
        h1.registry().unwrap().insert(1_u32).unwrap();
        assert_eq!(*h2.get::<u32>().unwrap().unwrap(), 1);
    }

    #[derive(Hash, Eq, PartialEq, Clone)]
    struct StringKey(u32);

    impl Key for StringKey {
        type Value = String;
    }

    impl Query for StringKey {
        fn build(&self, _factory: &DependencyFactoryHandle) -> Result<String, BuildError> {
            Ok(format!("built-{}", self.0))
        }
    }

    #[test]
    fn get_for_reads_the_registry() {
        let factory = DependencyFactory::new();
        assert!(factory.get_for(&StringKey(7)).is_none());
        let inserted = factory
            .registry()
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
