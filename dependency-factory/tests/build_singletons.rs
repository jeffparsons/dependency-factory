use std::sync::Arc;

use dependency_factory::{BuildError, DependencyFactory, DependencyFactoryHandle, Singleton};

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

struct Greeter {
    config: Arc<Config>,
}

impl Singleton for Greeter {
    fn build(factory: &DependencyFactoryHandle) -> Result<Self, BuildError> {
        Ok(Greeter {
            config: factory.build::<Config>()?,
        })
    }
}

struct Service {
    greeter: Arc<Greeter>,
    name: String,
}

impl Singleton for Service {
    fn build(factory: &DependencyFactoryHandle) -> Result<Self, BuildError> {
        Ok(Service {
            greeter: factory.build::<Greeter>()?,
            name: "world".into(),
        })
    }
}

#[test]
fn auto_builds_a_dependency_chain() {
    let factory = DependencyFactory::new();
    let service = factory.build::<Service>().unwrap();
    assert_eq!(service.greeter.config.greeting, "hello");
    assert_eq!(service.name, "world");
}

#[test]
fn build_caches_each_resource() {
    let factory = DependencyFactory::new();
    let s1 = factory.build::<Service>().unwrap();
    let s2 = factory.build::<Service>().unwrap();
    assert!(Arc::ptr_eq(&s1, &s2));
    let g1 = factory.build::<Greeter>().unwrap();
    assert!(Arc::ptr_eq(&s1.greeter, &g1));
}

#[test]
fn pre_inserted_resource_short_circuits_build() {
    let factory = DependencyFactory::new();
    let custom = factory.insert(Config {
        greeting: "g'day".into(),
    });
    let greeter = factory.build::<Greeter>().unwrap();
    assert!(Arc::ptr_eq(&greeter.config, &custom));
    assert_eq!(greeter.config.greeting, "g'day");
}

#[derive(Debug)]
struct Failing;

#[derive(Debug)]
struct Upstream;

impl std::fmt::Display for Upstream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("upstream is unavailable")
    }
}

impl std::error::Error for Upstream {}

impl Singleton for Failing {
    fn build(_factory: &DependencyFactoryHandle) -> Result<Self, BuildError> {
        Err(Upstream.into())
    }
}

#[derive(Debug)]
#[allow(dead_code)]
struct DependsOnFailing {
    f: Arc<Failing>,
}

impl Singleton for DependsOnFailing {
    fn build(factory: &DependencyFactoryHandle) -> Result<Self, BuildError> {
        Ok(DependsOnFailing {
            f: factory.build::<Failing>()?,
        })
    }
}

#[test]
fn errors_carry_a_chain_of_frames() {
    let factory = DependencyFactory::new();
    let err = factory.build::<DependsOnFailing>().unwrap_err();
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
