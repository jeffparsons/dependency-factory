use std::sync::Arc;

use dependency_factory::{
    BuildError, DependencyFactory, DependencyFactoryHandle, Query, Singleton,
};

#[derive(Debug)]
#[allow(dead_code)]
struct Db {
    name: String,
}

#[derive(Hash, Eq, PartialEq, Clone)]
struct DbKey(&'static str);

impl Query for DbKey {
    type Output = Db;

    fn build(&self, _factory: &DependencyFactoryHandle) -> Result<Db, BuildError> {
        Ok(Db {
            name: self.0.into(),
        })
    }
}

#[derive(Debug)]
#[allow(dead_code)]
struct Service {
    db: Arc<Db>,
    label: &'static str,
}

#[derive(Hash, Eq, PartialEq, Clone)]
struct ServiceKey(&'static str);

impl Query for ServiceKey {
    type Output = Service;

    fn build(&self, factory: &DependencyFactoryHandle) -> Result<Service, BuildError> {
        Ok(Service {
            db: factory.build_for(DbKey(self.0))?,
            label: self.0,
        })
    }
}

#[test]
fn distinct_keys_produce_distinct_outputs() {
    let factory = DependencyFactory::new();
    let a = factory.build_for(ServiceKey("a")).unwrap();
    let b = factory.build_for(ServiceKey("b")).unwrap();
    assert!(!Arc::ptr_eq(&a, &b));
    assert_eq!(a.label, "a");
    assert_eq!(b.label, "b");
    assert!(!Arc::ptr_eq(&a.db, &b.db));
}

#[test]
fn shared_dependency_key_produces_shared_arc() {
    let factory = DependencyFactory::new();
    let svc = factory.build_for(ServiceKey("shared")).unwrap();
    let db = factory.build_for(DbKey("shared")).unwrap();
    assert!(Arc::ptr_eq(&svc.db, &db));
}

#[test]
fn build_for_caches_per_key() {
    let factory = DependencyFactory::new();
    let s1 = factory.build_for(ServiceKey("one")).unwrap();
    let s2 = factory.build_for(ServiceKey("one")).unwrap();
    assert!(Arc::ptr_eq(&s1, &s2));
}

#[test]
fn pre_inserted_query_output_short_circuits_build() {
    let factory = DependencyFactory::new();
    let custom = factory.insert_for(
        DbKey("override"),
        Db {
            name: "fake".into(),
        },
    );
    let service = factory.build_for(ServiceKey("override")).unwrap();
    assert!(Arc::ptr_eq(&service.db, &custom));
    assert_eq!(service.db.name, "fake");
}

#[derive(Debug)]
struct Upstream;

impl std::fmt::Display for Upstream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("upstream is unavailable")
    }
}

impl std::error::Error for Upstream {}

#[derive(Debug)]
struct Failing;

#[derive(Hash, Eq, PartialEq, Clone)]
struct FailingKey;

impl Query for FailingKey {
    type Output = Failing;

    fn build(&self, _factory: &DependencyFactoryHandle) -> Result<Failing, BuildError> {
        Err(Upstream.into())
    }
}

#[derive(Debug)]
#[allow(dead_code)]
struct DependsOnFailing {
    f: Arc<Failing>,
}

#[derive(Hash, Eq, PartialEq, Clone)]
struct DependsOnFailingKey;

impl Query for DependsOnFailingKey {
    type Output = DependsOnFailing;

    fn build(&self, factory: &DependencyFactoryHandle) -> Result<DependsOnFailing, BuildError> {
        Ok(DependsOnFailing {
            f: factory.build_for(FailingKey)?,
        })
    }
}

#[test]
fn errors_carry_chain_of_output_type_names() {
    let factory = DependencyFactory::new();
    let err = factory.build_for(DependsOnFailingKey).unwrap_err();
    let chain = err.chain();
    assert_eq!(chain.len(), 2);
    assert!(
        chain[0].ends_with("Failing"),
        "innermost frame: {}",
        chain[0]
    );
    assert!(
        chain[1].ends_with("DependsOnFailing"),
        "outermost frame: {}",
        chain[1],
    );
    let rendered = format!("{err}");
    assert!(rendered.contains("DependsOnFailing"));
    assert!(rendered.contains("Failing"));
    assert!(rendered.contains("upstream is unavailable"));
}

struct Clock;

impl Singleton for Clock {
    fn build(_factory: &DependencyFactoryHandle) -> Result<Self, BuildError> {
        Ok(Clock)
    }
}

struct TimedDb {
    _clock: Arc<Clock>,
    name: String,
}

#[derive(Hash, Eq, PartialEq, Clone)]
struct TimedDbKey(&'static str);

impl Query for TimedDbKey {
    type Output = TimedDb;

    fn build(&self, factory: &DependencyFactoryHandle) -> Result<TimedDb, BuildError> {
        Ok(TimedDb {
            _clock: factory.build::<Clock>()?,
            name: self.0.into(),
        })
    }
}

#[test]
fn query_can_depend_on_singleton() {
    let factory = DependencyFactory::new();
    let db = factory.build_for(TimedDbKey("primary")).unwrap();
    assert_eq!(db.name, "primary");
    let clock = factory.build::<Clock>().unwrap();
    assert!(Arc::ptr_eq(&db._clock, &clock));
}
