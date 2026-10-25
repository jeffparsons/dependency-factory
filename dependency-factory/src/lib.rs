mod builder;
mod error;
mod factory;
mod registry;
mod store;

pub use builder::Builder;
pub use error::{BuildError, CycleError, Frame, OccupiedError};
pub use factory::{DependencyFactory, DependencyFactoryHandle, Query, Singleton};
pub use registry::{Key, Registry, Transaction};

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
        factory.begin().insert(Arc::new(42_u32)).unwrap();
        assert_eq!(*factory.get::<u32>().unwrap(), 42);
    }

    #[test]
    fn factory_over_a_prepopulated_registry() {
        let registry = Arc::new(Registry::new());
        registry.begin().insert(Arc::new(42_u32)).unwrap();
        let factory = DependencyFactory::with_registry(Arc::clone(&registry));
        assert!(Arc::ptr_eq(factory.registry(), &registry));
        assert_eq!(*factory.get::<u32>().unwrap(), 42);
    }

    #[test]
    fn a_handle_upgrades_to_the_factory() {
        let factory = DependencyFactory::new();
        factory.begin().insert(Arc::new(42_u32)).unwrap();
        let handle = factory.handle();
        let upgraded = handle.upgrade().unwrap();
        assert!(Arc::ptr_eq(upgraded.registry(), factory.registry()));
        assert_eq!(*upgraded.get::<u32>().unwrap(), 42);
    }

    #[test]
    fn a_handle_does_not_upgrade_after_the_last_factory_is_dropped() {
        let factory = DependencyFactory::new();
        let handle = factory.handle();
        let clone = factory.clone();
        drop(factory);
        assert!(handle.upgrade().is_some(), "a clone still holds it");
        drop(clone);
        assert!(handle.upgrade().is_none());
    }

    #[test]
    fn an_upgraded_factory_holds_the_registry_alive() {
        let factory = DependencyFactory::new();
        let handle = factory.handle();
        let upgraded = handle.upgrade().unwrap();
        drop(factory);
        upgraded.begin().insert(Arc::new(1_u32)).unwrap();
        assert_eq!(*handle.upgrade().unwrap().get::<u32>().unwrap(), 1);
        drop(upgraded);
        assert!(handle.upgrade().is_none());
    }

    #[test]
    fn handle_clone_is_cheap_and_independent() {
        let factory = DependencyFactory::new();
        let h1 = factory.handle();
        let h2 = h1.clone();
        h1.upgrade()
            .unwrap()
            .begin()
            .insert(Arc::new(1_u32))
            .unwrap();
        assert_eq!(*h2.upgrade().unwrap().get::<u32>().unwrap(), 1);
    }

    #[derive(Debug, Hash, Eq, PartialEq, Clone)]
    struct StringKey(u32);

    impl Key for StringKey {
        type Value = String;
    }

    impl Query for StringKey {
        fn build(&self, _builder: &mut Builder) -> Result<Arc<String>, BuildError> {
            Ok(Arc::new(format!("built-{}", self.0)))
        }
    }

    #[test]
    fn get_for_reads_the_registry() {
        let factory = DependencyFactory::new();
        assert!(factory.get_for(&StringKey(7)).is_none());
        let inserted = Arc::new("seven".to_string());
        factory
            .begin()
            .insert_for(StringKey(7), Arc::clone(&inserted))
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
        let inserted = Arc::new("by hand".to_string());
        factory
            .begin()
            .insert_for(StringKey(3), Arc::clone(&inserted))
            .unwrap();
        let built = factory.get_or_build_for(StringKey(3)).unwrap();
        assert!(Arc::ptr_eq(&inserted, &built));
    }
}
