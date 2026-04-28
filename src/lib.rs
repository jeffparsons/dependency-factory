mod error;
mod factory;

pub use error::BuildError;
pub use factory::{DependencyFactory, DependencyFactoryHandle};

#[cfg(test)]
mod tests {
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
}
