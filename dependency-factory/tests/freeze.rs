use std::sync::Arc;
use std::thread;

use dependency_factory::{
    BuildError, DependencyFactory, DependencyFactoryHandle, FrozenFactory, Query, Singleton,
};

fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn factory_types_are_send_and_sync() {
    assert_send_sync::<DependencyFactory>();
    assert_send_sync::<FrozenFactory>();
    assert_send_sync::<DependencyFactoryHandle>();
}

struct Config {
    greeting: String,
}

impl Singleton for Config {
    fn build(_factory: &mut DependencyFactory) -> Result<Self, BuildError> {
        Ok(Config {
            greeting: "hello".into(),
        })
    }
}

/// A resource that keeps a handle so it can look things up after assembly,
/// rather than capturing them at build time.
struct LateBound {
    factory: DependencyFactoryHandle,
}

impl Singleton for LateBound {
    fn build(factory: &mut DependencyFactory) -> Result<Self, BuildError> {
        Ok(LateBound {
            factory: factory.handle(),
        })
    }
}

impl LateBound {
    fn greeting(&self) -> Result<String, BuildError> {
        let config = self
            .factory
            .get::<Config>()?
            .expect("Config is built before the factory is frozen");
        Ok(config.greeting.clone())
    }
}

#[test]
fn handle_stored_during_build_resolves_after_freeze() {
    let mut factory = DependencyFactory::new();
    let late = factory.build::<LateBound>().unwrap();
    factory.build::<Config>().unwrap();

    let err = late.greeting().unwrap_err();
    assert!(format!("{err}").contains("not been frozen"), "{err}");

    let frozen = factory.freeze();
    assert_eq!(late.greeting().unwrap(), "hello");

    drop(frozen);
    let err = late.greeting().unwrap_err();
    assert!(format!("{err}").contains("has been dropped"), "{err}");
}

#[derive(Hash, Eq, PartialEq, Clone)]
struct DbKey(&'static str);

impl Query for DbKey {
    type Output = String;

    fn build(&self, _factory: &mut DependencyFactory) -> Result<String, BuildError> {
        Ok(format!("db-{}", self.0))
    }
}

#[test]
fn frozen_factory_is_shared_across_threads() {
    let mut factory = DependencyFactory::new();
    let config = factory.build::<Config>().unwrap();
    let db = factory.build_for(DbKey("users")).unwrap();
    let frozen = Arc::new(factory.freeze());

    let workers: Vec<_> = (0..4)
        .map(|_| {
            let frozen = Arc::clone(&frozen);
            let handle = frozen.handle();
            thread::spawn(move || {
                let via_owner = frozen.get::<Config>().unwrap();
                let via_handle = handle.get::<Config>().unwrap().unwrap();
                let db = handle.get_for(DbKey("users")).unwrap().unwrap();
                (via_owner, via_handle, db)
            })
        })
        .collect();

    for worker in workers {
        let (via_owner, via_handle, worker_db) = worker.join().unwrap();
        assert!(Arc::ptr_eq(&via_owner, &config));
        assert!(Arc::ptr_eq(&via_handle, &config));
        assert!(Arc::ptr_eq(&worker_db, &db));
    }
}

#[test]
fn frozen_factory_reports_absent_keys() {
    let frozen = DependencyFactory::new().freeze();
    assert!(frozen.get::<Config>().is_none());
    assert!(frozen.get_for(DbKey("missing")).is_none());
    assert!(frozen.handle().get::<Config>().unwrap().is_none());
}
