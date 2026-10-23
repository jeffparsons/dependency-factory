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

// --- Fields that are not dependencies ----------------------------------------

trait WarmUp: Send + Sync {
    fn warm_up(&self);
}

// Plain state beside a dependency, as in a resource that dedupes
// concurrent warm-ups, together with an index.
#[derive(Singleton)]
#[factory(index(dyn WarmUp))]
struct Catalog {
    config: Arc<Config>,
    #[factory(default)]
    warmed: std::sync::OnceLock<()>,
    #[factory(default)]
    warm_ups: std::sync::atomic::AtomicUsize,
}

impl WarmUp for Catalog {
    fn warm_up(&self) {
        self.warmed.get_or_init(|| {
            self.warm_ups
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        });
    }
}

#[test]
fn derive_fills_a_default_field_with_its_default() {
    let factory = DependencyFactory::new();
    let catalog = factory.get_or_build::<Catalog>().unwrap();
    assert!(Arc::ptr_eq(
        &catalog.config,
        &factory.get_or_build::<Config>().unwrap()
    ));
    assert!(catalog.warmed.get().is_none());
    for resource in factory.indexed::<dyn WarmUp>() {
        resource.warm_up();
        resource.warm_up();
    }
    assert_eq!(
        catalog.warm_ups.load(std::sync::atomic::Ordering::Relaxed),
        1
    );
}

#[derive(Singleton)]
struct Counters(#[factory(default)] Vec<u32>, Arc<Config>);

#[test]
fn derive_fills_a_default_field_in_a_tuple_struct() {
    let factory = DependencyFactory::new();
    let counters = factory.get_or_build::<Counters>().unwrap();
    assert!(counters.0.is_empty());
    assert_eq!(counters.1.greeting, "hello");
}

fn shout(builder: &mut Builder) -> Result<String, BuildError> {
    let config = builder.get_or_build::<Config>()?;
    Ok(config.greeting.to_uppercase())
}

#[derive(Singleton)]
struct Shouter {
    #[factory(build = shout)]
    line: String,
}

#[test]
fn derive_fills_a_build_field_from_its_function() {
    let factory = DependencyFactory::new();
    let shouter = factory.get_or_build::<Shouter>().unwrap();
    assert_eq!(shouter.line, "HELLO");
    // The function's dependency was built and cached along the way.
    assert!(factory.get::<Config>().is_some());
}

// Any error `?` turns into a `BuildError` will do.
fn read_port(_builder: &mut Builder) -> Result<u16, std::io::Error> {
    Err(std::io::Error::other("port file is missing"))
}

#[derive(Debug, Singleton)]
#[allow(dead_code)]
struct Listener {
    #[factory(build = read_port)]
    port: u16,
}

#[test]
fn derive_attributes_a_build_function_error_to_the_struct() {
    let factory = DependencyFactory::new();
    let err = factory.get_or_build::<Listener>().unwrap_err();
    let chain = err.chain();
    assert_eq!(chain.len(), 1, "chain: {chain:?}");
    assert!(chain[0].type_name().ends_with("Listener"));
    assert!(
        format!("{err:?}").contains("port file is missing"),
        "{err:?}"
    );
}

// Fields are filled in the order they are written, so dependencies are
// built, and indexed, in that order.
trait Named: Send + Sync {
    fn name(&self) -> &'static str;
}

#[derive(Singleton)]
#[factory(index(dyn Named))]
struct First;

impl Named for First {
    fn name(&self) -> &'static str {
        "first"
    }
}

#[derive(Singleton)]
#[factory(index(dyn Named))]
struct Third;

impl Named for Third {
    fn name(&self) -> &'static str {
        "third"
    }
}

fn indexed_so_far(builder: &mut Builder) -> Result<Vec<&'static str>, BuildError> {
    Ok(builder
        .indexed::<dyn Named>()
        .iter()
        .map(|named| named.name())
        .collect())
}

#[derive(Singleton)]
struct InOrder {
    _first: Arc<First>,
    #[factory(build = indexed_so_far)]
    seen: Vec<&'static str>,
    _third: Arc<Third>,
}

#[test]
fn derive_fills_fields_in_the_order_written() {
    let factory = DependencyFactory::new();
    let in_order = factory.get_or_build::<InOrder>().unwrap();
    assert_eq!(in_order.seen, ["first"]);
    let names: Vec<_> = factory
        .indexed::<dyn Named>()
        .iter()
        .map(|named| named.name())
        .collect();
    assert_eq!(names, ["first", "third"]);
}
