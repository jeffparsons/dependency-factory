use std::sync::Arc;

use dependency_factory::{BuildError, Builder, DependencyFactory, Key, Query, Singleton};

struct Config {
    greeting: String,
}

impl Singleton for Config {
    fn build(_builder: &mut Builder) -> Result<Self, BuildError> {
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
    let s = factory.get_or_build::<Service>().unwrap();
    let g = factory.get_or_build::<Greeter>().unwrap();
    let c = factory.get_or_build::<Config>().unwrap();
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
    fn build(_builder: &mut Builder) -> Result<Self, BuildError> {
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
    let err = factory.get_or_build::<DependsOnFailing>().unwrap_err();
    let chain = err.chain();
    assert_eq!(chain.len(), 2, "chain: {chain:?}");
    assert!(
        chain[0].type_name().ends_with("Failing"),
        "innermost: {}",
        chain[0]
    );
    assert!(
        chain[1].type_name().ends_with("DependsOnFailing"),
        "outermost: {}",
        chain[1],
    );
    let rendered = format!("{err}");
    assert!(rendered.contains("DependsOnFailing"));
    assert!(rendered.contains("Failing"));
    assert!(rendered.contains("upstream is unavailable"));
}

#[derive(Debug, Hash, Eq, PartialEq, Clone)]
struct DbKey(&'static str);

impl Key for DbKey {
    type Value = String;
}

impl Query for DbKey {
    fn build(&self, _builder: &mut Builder) -> Result<String, BuildError> {
        Ok(format!("db-{}", self.0))
    }
}

fn users_db_key(_builder: &mut Builder) -> Result<DbKey, BuildError> {
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
    let s = factory.get_or_build::<UsersService>().unwrap();
    assert_eq!(*s.db, "db-users");
    let direct = factory.get_or_build_for(DbKey("users")).unwrap();
    assert!(Arc::ptr_eq(&s.db, &direct));
}

// Key fn that itself uses the factory handle: it resolves a singleton, then
// derives the key from it. This demonstrates that `query =` functions are not
// limited to constants.
fn config_named_key(builder: &mut Builder) -> Result<DbKey, BuildError> {
    let cfg = builder.get_or_build::<Config>()?;
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
    let s = factory.get_or_build::<ConfiguredService>().unwrap();
    assert_eq!(*s.db, "db-hello-db");
}

#[derive(Singleton)]
struct GreeterWrap(Arc<Greeter>);

#[test]
fn derive_supports_tuple_struct() {
    let factory = DependencyFactory::new();
    let w = factory.get_or_build::<GreeterWrap>().unwrap();
    let g = factory.get_or_build::<Greeter>().unwrap();
    assert!(Arc::ptr_eq(&w.0, &g));
}

#[derive(Singleton)]
struct Marker;

#[test]
fn derive_supports_unit_struct() {
    let factory = DependencyFactory::new();
    // Builds without panic; Arc identity confirms caching works the same way.
    let a = factory.get_or_build::<Marker>().unwrap();
    let b = factory.get_or_build::<Marker>().unwrap();
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
    let p = factory.get_or_build::<Pair<Config, Greeter>>().unwrap();
    assert_eq!(p.a.greeting, "hello");
    assert_eq!(p.b.config.greeting, "hello");
}
