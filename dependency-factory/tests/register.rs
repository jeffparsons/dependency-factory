//! `Singleton::register` and `Query::register`, and the indexes they add
//! resources to.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use dependency_factory::{BuildError, Builder, DependencyFactory, Key, Query, Singleton};

trait Lifecycle: Send + Sync {
    fn name(&self) -> String;
}

trait Shutdown: Send + Sync {
    fn shutdown(&self);
    fn shutdown_name(&self) -> &'static str {
        "built"
    }
}

fn names(index: Vec<Arc<dyn Lifecycle>>) -> Vec<String> {
    index.iter().map(|r| r.name()).collect()
}

// --- A hand-written chain: Service -> Db -> Config ---------------------------

struct Config;

impl Singleton for Config {
    fn build(_: &mut Builder<'_>) -> Result<Arc<Self>, BuildError> {
        Ok(Arc::new(Config))
    }
}

struct Db {
    _config: Arc<Config>,
    registered: AtomicUsize,
}

impl Lifecycle for Db {
    fn name(&self) -> String {
        "Db".into()
    }
}

impl Shutdown for Db {
    fn shutdown(&self) {}
    fn shutdown_name(&self) -> &'static str {
        if self.registered.load(Ordering::SeqCst) == 0 {
            "hand-made"
        } else {
            "built"
        }
    }
}

impl Singleton for Db {
    fn build(builder: &mut Builder<'_>) -> Result<Arc<Self>, BuildError> {
        Ok(Arc::new(Db {
            _config: builder.get_or_build()?,
            registered: AtomicUsize::new(0),
        }))
    }

    fn register(this: &Arc<Self>, builder: &mut Builder<'_>) -> Result<(), BuildError> {
        this.registered.fetch_add(1, Ordering::SeqCst);
        // The value is already cached when `register` runs.
        assert!(Arc::ptr_eq(this, &builder.get::<Db>().unwrap()));
        builder.index::<dyn Lifecycle>(this.clone());
        builder.index::<dyn Shutdown>(this.clone());
        Ok(())
    }
}

struct Service {
    _db: Arc<Db>,
}

impl Lifecycle for Service {
    fn name(&self) -> String {
        "Service".into()
    }
}

impl Singleton for Service {
    fn build(builder: &mut Builder<'_>) -> Result<Arc<Self>, BuildError> {
        Ok(Arc::new(Service {
            _db: builder.get_or_build()?,
        }))
    }

    fn register(this: &Arc<Self>, builder: &mut Builder<'_>) -> Result<(), BuildError> {
        builder.index::<dyn Lifecycle>(this.clone());
        Ok(())
    }
}

#[test]
fn register_runs_once_after_the_build_and_not_on_a_cache_hit() {
    let factory = DependencyFactory::new();
    let db = factory.get_or_build::<Db>().unwrap();
    assert_eq!(db.registered.load(Ordering::SeqCst), 1);
    factory.get_or_build::<Db>().unwrap();
    factory.get_or_build::<Service>().unwrap();
    assert_eq!(db.registered.load(Ordering::SeqCst), 1);
    assert_eq!(names(factory.indexed::<dyn Lifecycle>()), ["Db", "Service"]);
    assert_eq!(factory.indexed::<dyn Shutdown>().len(), 1);
}

#[test]
fn a_resource_follows_its_dependencies_in_an_index() {
    let factory = DependencyFactory::new();
    factory.get_or_build::<Service>().unwrap();
    assert_eq!(names(factory.indexed::<dyn Lifecycle>()), ["Db", "Service"]);
    // Shut down in reverse.
    let order: Vec<String> = factory
        .indexed::<dyn Lifecycle>()
        .iter()
        .rev()
        .map(|r| r.name())
        .collect();
    assert_eq!(order, ["Service", "Db"]);
    for resource in factory.indexed::<dyn Shutdown>().iter().rev() {
        resource.shutdown();
    }
}

#[test]
fn the_index_entry_is_the_cached_arc() {
    let factory = DependencyFactory::new();
    let db = factory.get_or_build::<Db>().unwrap();
    let indexed = factory.indexed::<dyn Shutdown>();
    assert_eq!(
        Arc::as_ptr(&indexed[0]) as *const (),
        Arc::as_ptr(&db) as *const ()
    );
}

#[test]
fn a_held_builder_sees_its_own_index_additions_before_they_are_published() {
    let factory = DependencyFactory::new();
    let mut builder = factory.builder();
    builder.get_or_build::<Service>().unwrap();
    assert_eq!(names(builder.indexed::<dyn Lifecycle>()), ["Db", "Service"]);
    assert!(factory.indexed::<dyn Lifecycle>().is_empty());
    drop(builder);
    assert_eq!(names(factory.indexed::<dyn Lifecycle>()), ["Db", "Service"]);
}

fn hand_made_db() -> Arc<Db> {
    Arc::new(Db {
        _config: Arc::new(Config),
        registered: AtomicUsize::new(0),
    })
}

#[test]
fn a_hand_inserted_value_is_not_indexed_unless_the_caller_indexes_it() {
    let factory = DependencyFactory::new();
    let db = hand_made_db();
    factory.begin().insert(Arc::clone(&db)).unwrap();
    factory.get_or_build::<Service>().unwrap();
    assert_eq!(db.registered.load(Ordering::SeqCst), 0);
    assert_eq!(names(factory.indexed::<dyn Lifecycle>()), ["Service"]);

    factory.begin().index::<dyn Lifecycle>(db);
    assert_eq!(names(factory.indexed::<dyn Lifecycle>()), ["Service", "Db"]);
}

#[test]
fn a_replaced_value_stays_in_its_indexes_until_the_caller_unindexes_it() {
    let factory = DependencyFactory::new();
    factory.get_or_build::<Service>().unwrap();
    let fresh = hand_made_db();
    let mut tx = factory.begin();
    let stale = tx.replace(Arc::clone(&fresh)).expect("Db was built");
    // Replacing is a slot write; the indexes still list the old value.
    assert_eq!(names(tx.indexed::<dyn Lifecycle>()), ["Db", "Service"]);
    // The caller knows which indexes it was in, and swaps it out of each.
    tx.unindex::<dyn Lifecycle>(Arc::clone(&stale) as Arc<dyn Lifecycle>);
    tx.unindex::<dyn Shutdown>(stale);
    tx.index::<dyn Lifecycle>(Arc::clone(&fresh) as Arc<dyn Lifecycle>);
    tx.index::<dyn Shutdown>(fresh);
    drop(tx);
    assert_eq!(names(factory.indexed::<dyn Lifecycle>()), ["Service", "Db"]);
    assert_eq!(factory.indexed::<dyn Shutdown>().len(), 1);
    assert_eq!(
        factory.indexed::<dyn Shutdown>()[0].shutdown_name(),
        "hand-made"
    );
}

#[test]
fn indexed_through_a_handle() {
    let factory = DependencyFactory::new();
    factory.get_or_build::<Db>().unwrap();
    let handle = factory.handle();
    let upgraded = handle.upgrade().unwrap();
    assert_eq!(names(upgraded.indexed::<dyn Lifecycle>()), ["Db"]);
    drop(factory);
    drop(upgraded);
    assert!(handle.upgrade().is_none());
}

// --- register may resolve other resources ------------------------------------

#[derive(Default)]
struct Listeners(Mutex<Vec<String>>);

impl Singleton for Listeners {
    fn build(_: &mut Builder<'_>) -> Result<Arc<Self>, BuildError> {
        Ok(Arc::new(Listeners::default()))
    }
}

struct Announcer;

impl Singleton for Announcer {
    fn build(_: &mut Builder<'_>) -> Result<Arc<Self>, BuildError> {
        Ok(Arc::new(Announcer))
    }

    fn register(_: &Arc<Self>, builder: &mut Builder<'_>) -> Result<(), BuildError> {
        let listeners = builder.get_or_build::<Listeners>()?;
        listeners.0.lock().unwrap().push("Announcer".into());
        Ok(())
    }
}

#[test]
fn register_can_build_and_use_other_resources() {
    let factory = DependencyFactory::new();
    factory.get_or_build::<Announcer>().unwrap();
    let listeners = factory.get::<Listeners>().unwrap();
    assert_eq!(*listeners.0.lock().unwrap(), ["Announcer"]);
}

// --- Errors in register ---------------------------------------------------------

#[derive(Debug)]
struct Refused;

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("refused")
    }
}

impl std::error::Error for Refused {}

struct Unregisterable;

impl Singleton for Unregisterable {
    fn build(_: &mut Builder<'_>) -> Result<Arc<Self>, BuildError> {
        Ok(Arc::new(Unregisterable))
    }

    fn register(_: &Arc<Self>, _: &mut Builder<'_>) -> Result<(), BuildError> {
        Err(Refused.into())
    }
}

struct NeedsUnregisterable {
    _inner: Arc<Unregisterable>,
}

impl Singleton for NeedsUnregisterable {
    fn build(builder: &mut Builder<'_>) -> Result<Arc<Self>, BuildError> {
        Ok(Arc::new(NeedsUnregisterable {
            _inner: builder.get_or_build()?,
        }))
    }
}

#[test]
fn a_register_error_is_attributed_like_a_build_error_and_the_value_stays_cached() {
    let factory = DependencyFactory::new();
    let err = factory
        .get_or_build::<NeedsUnregisterable>()
        .err()
        .expect("register fails");
    assert!(err.source().is::<Refused>());
    let chain: Vec<&str> = err
        .chain()
        .iter()
        .map(|f| f.type_name().rsplit("::").next().unwrap())
        .collect();
    assert_eq!(chain, ["Unregisterable", "NeedsUnregisterable"]);
    assert!(factory.get::<Unregisterable>().is_some());
    assert!(factory.get::<NeedsUnregisterable>().is_none());
}

// --- Query::register ---------------------------------------------------------

struct Conn {
    name: &'static str,
}

impl Lifecycle for Conn {
    fn name(&self) -> String {
        format!("Conn({})", self.name)
    }
}

#[derive(Debug, Hash, Eq, PartialEq, Clone)]
struct ConnKey(&'static str);

impl Key for ConnKey {
    type Value = Conn;
}

impl Query for ConnKey {
    fn build(&self, _: &mut Builder<'_>) -> Result<Arc<Conn>, BuildError> {
        Ok(Arc::new(Conn { name: self.0 }))
    }

    fn register(&self, value: &Arc<Conn>, builder: &mut Builder<'_>) -> Result<(), BuildError> {
        assert_eq!(value.name, self.0);
        builder.index::<dyn Lifecycle>(value.clone());
        Ok(())
    }
}

#[test]
fn query_register_runs_per_key_with_the_key_to_hand() {
    let factory = DependencyFactory::new();
    factory.get_or_build_for(ConnKey("a")).unwrap();
    factory.get_or_build_for(ConnKey("b")).unwrap();
    factory.get_or_build_for(ConnKey("a")).unwrap();
    assert_eq!(
        names(factory.indexed::<dyn Lifecycle>()),
        ["Conn(a)", "Conn(b)"]
    );
}

// --- The derive --------------------------------------------------------------

#[derive(Singleton)]
#[factory(index(dyn Lifecycle, dyn Shutdown))]
struct Derived {
    _db: Arc<Db>,
}

impl Lifecycle for Derived {
    fn name(&self) -> String {
        "Derived".into()
    }
}

impl Shutdown for Derived {
    fn shutdown(&self) {}
}

#[derive(Singleton)]
#[factory(index(dyn Lifecycle))]
#[factory(index(dyn Shutdown))]
struct DerivedTwice;

impl Lifecycle for DerivedTwice {
    fn name(&self) -> String {
        "DerivedTwice".into()
    }
}

impl Shutdown for DerivedTwice {
    fn shutdown(&self) {}
}

#[test]
fn the_derive_indexes_from_a_struct_attribute() {
    let factory = DependencyFactory::new();
    factory.get_or_build::<Derived>().unwrap();
    factory.get_or_build::<DerivedTwice>().unwrap();
    assert_eq!(
        names(factory.indexed::<dyn Lifecycle>()),
        ["Db", "Derived", "DerivedTwice"]
    );
    assert_eq!(factory.indexed::<dyn Shutdown>().len(), 3);
}
