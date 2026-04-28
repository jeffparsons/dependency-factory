mod error;
mod factory;

pub use error::BuildError;
pub use factory::{DependencyFactory, DependencyFactoryHandle, Query, Singleton};

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    #[test]
    fn insert_and_get_singleton() {
        let factory = DependencyFactory::new();
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
        let factory = DependencyFactory::new();
        factory.insert(1_u32);
        factory.insert(2_u64);
        assert_eq!(*factory.get::<u32>().unwrap(), 1);
        assert_eq!(*factory.get::<u64>().unwrap(), 2);
    }

    #[test]
    fn insert_overwrites_existing_entry() {
        let factory = DependencyFactory::new();
        factory.insert(1_u32);
        factory.insert(2_u32);
        assert_eq!(*factory.get::<u32>().unwrap(), 2);
    }

    #[test]
    fn handle_can_resolve() {
        let factory = DependencyFactory::new();
        factory.insert(42_u32);
        let handle = factory.handle();
        assert_eq!(*handle.get::<u32>().unwrap().unwrap(), 42);
    }

    #[test]
    fn handle_can_insert() {
        let factory = DependencyFactory::new();
        let handle = factory.handle();
        handle.insert(42_u32).unwrap();
        assert_eq!(*factory.get::<u32>().unwrap(), 42);
    }

    #[test]
    fn handle_after_factory_dropped_errors() {
        let factory = DependencyFactory::new();
        let handle = factory.handle();
        drop(factory);
        assert!(handle.get::<u32>().is_err());
        assert!(handle.insert(1_u32).is_err());
    }

    #[test]
    fn handle_clone_is_cheap_and_independent() {
        let factory = DependencyFactory::new();
        let h1 = factory.handle();
        let h2 = h1.clone();
        h1.insert(1_u32).unwrap();
        assert_eq!(*h2.get::<u32>().unwrap().unwrap(), 1);
    }

    #[derive(Hash, Eq, PartialEq, Clone)]
    struct StringKey(u32);

    impl Query for StringKey {
        type Output = String;

        fn build(
            &self,
            _factory: &DependencyFactoryHandle,
        ) -> Result<String, BuildError> {
            Ok(format!("built-{}", self.0))
        }
    }

    #[test]
    fn insert_for_and_get_for_round_trip() {
        let factory = DependencyFactory::new();
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
        let factory = DependencyFactory::new();
        factory.insert_for(StringKey(1), "one".into());
        factory.insert_for(StringKey(2), "two".into());
        assert_eq!(*factory.get_for(StringKey(1)).unwrap(), "one");
        assert_eq!(*factory.get_for(StringKey(2)).unwrap(), "two");
    }

    #[test]
    fn insert_for_overwrites_existing_entry() {
        let factory = DependencyFactory::new();
        factory.insert_for(StringKey(1), "first".into());
        factory.insert_for(StringKey(1), "second".into());
        assert_eq!(*factory.get_for(StringKey(1)).unwrap(), "second");
    }

    #[test]
    fn build_for_uses_query_build_when_absent() {
        let factory = DependencyFactory::new();
        let built = factory.build_for(StringKey(3)).unwrap();
        assert_eq!(*built, "built-3");
    }

    #[test]
    fn build_for_caches_results() {
        let factory = DependencyFactory::new();
        let a = factory.build_for(StringKey(3)).unwrap();
        let b = factory.build_for(StringKey(3)).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn handle_can_resolve_queries() {
        let factory = DependencyFactory::new();
        factory.insert_for(StringKey(1), "one".into());
        let handle = factory.handle();
        assert_eq!(*handle.get_for(StringKey(1)).unwrap().unwrap(), "one");
        let built = handle.build_for(StringKey(2)).unwrap();
        assert_eq!(*built, "built-2");
    }

    #[test]
    fn handle_after_factory_dropped_errors_for_queries() {
        let factory = DependencyFactory::new();
        let handle = factory.handle();
        drop(factory);
        assert!(handle.get_for(StringKey(1)).is_err());
        assert!(handle.insert_for(StringKey(1), "x".into()).is_err());
        assert!(handle.build_for(StringKey(1)).is_err());
    }
}
