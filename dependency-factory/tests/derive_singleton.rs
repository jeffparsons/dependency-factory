use std::sync::Arc;

use dependency_factory::{
    BuildError, DependencyFactory, DependencyFactoryHandle, Query, Singleton,
};

struct Config {
    greeting: String,
}

impl Singleton for Config {
    fn build(_factory: &DependencyFactoryHandle) -> Result<Self, BuildError> {
        Ok(Config {
            greeting: "hello".into(),
        })
    }
}

#[derive(Singleton)]
struct Greeter {
    config: Arc<Config>,
}

#[derive(Singleton)]
struct Service {
    greeter: Arc<Greeter>,
    config: Arc<Config>,
}

#[test]
fn derive_chain_builds_and_caches() {
    let factory = DependencyFactory::new();
    let s = factory.build::<Service>().unwrap();
    let g = factory.build::<Greeter>().unwrap();
    let c = factory.build::<Config>().unwrap();
    assert!(Arc::ptr_eq(&s.greeter, &g));
    assert!(Arc::ptr_eq(&s.config, &c));
    assert!(Arc::ptr_eq(&s.greeter.config, &c));
    assert_eq!(c.greeting, "hello");
}

#[derive(Debug)]
struct Upstream;

impl std::fmt::Display for Upstream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("upstream is unavailable")
    }
}

impl std::error::Error for Upstream {}

struct Failing;

impl Singleton for Failing {
    fn build(_factory: &DependencyFactoryHandle) -> Result<Self, BuildError> {
        Err(Upstream.into())
    }
}

#[derive(Debug, Singleton)]
#[allow(dead_code)]
struct DependsOnFailing {
    f: Arc<Failing>,
}

impl std::fmt::Debug for Failing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Failing")
    }
}

#[test]
fn derive_propagates_error_with_frame() {
    let factory = DependencyFactory::new();
    let err = factory.build::<DependsOnFailing>().unwrap_err();
    let chain = err.chain();
    assert_eq!(chain.len(), 2, "chain: {chain:?}");
    assert!(chain[0].ends_with("Failing"), "innermost: {}", chain[0]);
    assert!(
        chain[1].ends_with("DependsOnFailing"),
        "outermost: {}",
        chain[1],
    );
    let rendered = format!("{err}");
    assert!(rendered.contains("DependsOnFailing"));
    assert!(rendered.contains("Failing"));
    assert!(rendered.contains("upstream is unavailable"));
}

#[derive(Hash, Eq, PartialEq, Clone)]
struct DbKey(&'static str);

impl Query for DbKey {
    type Output = String;

    fn build(&self, _factory: &DependencyFactoryHandle) -> Result<String, BuildError> {
        Ok(format!("db-{}", self.0))
    }
}

fn users_db_key(_factory: &DependencyFactoryHandle) -> Result<DbKey, BuildError> {
    Ok(DbKey("users"))
}

#[derive(Singleton)]
struct UsersService {
    #[factory(query = users_db_key)]
    db: Arc<String>,
}

#[test]
fn derive_uses_query_for_keyed_field() {
    let factory = DependencyFactory::new();
    let s = factory.build::<UsersService>().unwrap();
    assert_eq!(*s.db, "db-users");
    let direct = factory.build_for(DbKey("users")).unwrap();
    assert!(Arc::ptr_eq(&s.db, &direct));
}

// Key fn that itself uses the factory handle: it resolves a singleton, then
// derives the key from it. This demonstrates that `query =` functions are not
// limited to constants.
fn config_named_key(factory: &DependencyFactoryHandle) -> Result<DbKey, BuildError> {
    let cfg = factory.build::<Config>()?;
    let name: &'static str = if cfg.greeting == "hello" {
        "hello-db"
    } else {
        "other-db"
    };
    Ok(DbKey(name))
}

#[derive(Singleton)]
struct ConfiguredService {
    #[factory(query = config_named_key)]
    db: Arc<String>,
}

#[test]
fn derive_query_key_can_resolve_from_factory() {
    let factory = DependencyFactory::new();
    let s = factory.build::<ConfiguredService>().unwrap();
    assert_eq!(*s.db, "db-hello-db");
}

#[derive(Singleton)]
struct GreeterWrap(Arc<Greeter>);

#[test]
fn derive_supports_tuple_struct() {
    let factory = DependencyFactory::new();
    let w = factory.build::<GreeterWrap>().unwrap();
    let g = factory.build::<Greeter>().unwrap();
    assert!(Arc::ptr_eq(&w.0, &g));
}

#[derive(Singleton)]
struct Marker;

#[test]
fn derive_supports_unit_struct() {
    let factory = DependencyFactory::new();
    // Builds without panic; Arc identity confirms caching works the same way.
    let a = factory.build::<Marker>().unwrap();
    let b = factory.build::<Marker>().unwrap();
    assert!(Arc::ptr_eq(&a, &b));
}

#[derive(Singleton)]
struct Pair<A: Singleton, B: Singleton> {
    a: Arc<A>,
    b: Arc<B>,
}

#[test]
fn derive_supports_generic_struct() {
    let factory = DependencyFactory::new();
    let p = factory.build::<Pair<Config, Greeter>>().unwrap();
    assert_eq!(p.a.greeting, "hello");
    assert_eq!(p.b.config.greeting, "hello");
}
