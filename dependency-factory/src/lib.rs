mod error;
mod factory;

pub use error::{BuildError, CycleError};
pub use factory::{DependencyFactory, DependencyFactoryHandle, FrozenFactory, Query, Singleton};

#[cfg(feature = "derive")]
pub use dependency_factory_derive::Singleton;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn insert_and_get_singleton() {
        let mut factory = DependencyFactory::new();
        let inserted = factory.insert(42_u32);
        assert_eq!(*inserted, 42);
        let got = factory.get::<u32>().unwrap();
        assert_eq!(*got, 42);
    }

    #[test]
    fn get_returns_none_when_absent() {
        let factory = DependencyFactory::new();
        assert!(factory.get::<u32>().is_none());
    }

    #[test]
    fn singletons_are_keyed_by_type() {
        let mut factory = DependencyFactory::new();
        factory.insert(1_u32);
        factory.insert(2_u64);
        assert_eq!(*factory.get::<u32>().unwrap(), 1);
        assert_eq!(*factory.get::<u64>().unwrap(), 2);
    }

    #[test]
    fn insert_overwrites_existing_entry() {
        let mut factory = DependencyFactory::new();
        factory.insert(1_u32);
        factory.insert(2_u32);
        assert_eq!(*factory.get::<u32>().unwrap(), 2);
    }

    #[test]
    fn frozen_factory_can_get() {
        let mut factory = DependencyFactory::new();
        factory.insert(42_u32);
        let frozen = factory.freeze();
        assert_eq!(*frozen.get::<u32>().unwrap(), 42);
        assert!(frozen.get::<u64>().is_none());
    }

    #[test]
    fn handle_can_resolve_after_freeze() {
        let mut factory = DependencyFactory::new();
        factory.insert(42_u32);
        let handle = factory.handle();
        let frozen = factory.freeze();
        assert_eq!(*handle.get::<u32>().unwrap().unwrap(), 42);
        assert_eq!(*frozen.handle().get::<u32>().unwrap().unwrap(), 42);
    }

    #[test]
    fn handle_before_freeze_errors() {
        let mut factory = DependencyFactory::new();
        factory.insert(42_u32);
        let handle = factory.handle();
        let err = handle.get::<u32>().unwrap_err();
        assert!(format!("{err}").contains("not been frozen"));
    }

    #[test]
    fn handle_after_unfrozen_factory_dropped_errors() {
        let factory = DependencyFactory::new();
        let handle = factory.handle();
        drop(factory);
        let err = handle.get::<u32>().unwrap_err();
        assert!(format!("{err}").contains("has been dropped"));
    }

    #[test]
    fn handle_after_frozen_factory_dropped_errors() {
        let factory = DependencyFactory::new();
        let frozen = factory.freeze();
        let handle = frozen.handle();
        drop(frozen);
        let err = handle.get::<u32>().unwrap_err();
        assert!(format!("{err}").contains("has been dropped"));
    }

    #[test]
    fn handle_clone_is_cheap_and_independent() {
        let mut factory = DependencyFactory::new();
        let h1 = factory.handle();
        let h2 = h1.clone();
        factory.insert(1_u32);
        let _frozen = factory.freeze();
        assert_eq!(*h1.get::<u32>().unwrap().unwrap(), 1);
        assert_eq!(*h2.get::<u32>().unwrap().unwrap(), 1);
    }

    #[derive(Hash, Eq, PartialEq, Clone)]
    struct StringKey(u32);

    impl Query for StringKey {
        type Output = String;

        fn build(&self, _factory: &mut DependencyFactory) -> Result<String, BuildError> {
            Ok(format!("built-{}", self.0))
        }
    }

    #[test]
    fn insert_for_and_get_for_round_trip() {
        let mut factory = DependencyFactory::new();
        let inserted = factory.insert_for(StringKey(7), "seven".to_string());
        assert_eq!(*inserted, "seven");
        let got = factory.get_for(StringKey(7)).unwrap();
        assert_eq!(*got, "seven");
    }

    #[test]
    fn get_for_returns_none_when_absent() {
        let factory = DependencyFactory::new();
        assert!(factory.get_for(StringKey(7)).is_none());
    }

    #[test]
    fn queries_with_different_keys_are_independent() {
        let mut factory = DependencyFactory::new();
        factory.insert_for(StringKey(1), "one".into());
        factory.insert_for(StringKey(2), "two".into());
        assert_eq!(*factory.get_for(StringKey(1)).unwrap(), "one");
        assert_eq!(*factory.get_for(StringKey(2)).unwrap(), "two");
    }

    #[test]
    fn insert_for_overwrites_existing_entry() {
        let mut factory = DependencyFactory::new();
        factory.insert_for(StringKey(1), "first".into());
        factory.insert_for(StringKey(1), "second".into());
        assert_eq!(*factory.get_for(StringKey(1)).unwrap(), "second");
    }

    #[test]
    fn build_for_uses_query_build_when_absent() {
        let mut factory = DependencyFactory::new();
        let built = factory.build_for(StringKey(3)).unwrap();
        assert_eq!(*built, "built-3");
    }

    #[test]
    fn build_for_caches_results() {
        let mut factory = DependencyFactory::new();
        let a = factory.build_for(StringKey(3)).unwrap();
        let b = factory.build_for(StringKey(3)).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn frozen_factory_and_handle_can_resolve_queries() {
        let mut factory = DependencyFactory::new();
        factory.insert_for(StringKey(1), "one".into());
        let handle = factory.handle();
        let frozen = factory.freeze();
        assert_eq!(*frozen.get_for(StringKey(1)).unwrap(), "one");
        assert!(frozen.get_for(StringKey(2)).is_none());
        assert_eq!(*handle.get_for(StringKey(1)).unwrap().unwrap(), "one");
        assert!(handle.get_for(StringKey(2)).unwrap().is_none());
    }

    #[test]
    fn handle_errors_for_queries_before_freeze_and_after_drop() {
        let factory = DependencyFactory::new();
        let handle = factory.handle();
        assert!(handle.get_for(StringKey(1)).is_err());
        drop(factory);
        assert!(handle.get_for(StringKey(1)).is_err());
    }
}
