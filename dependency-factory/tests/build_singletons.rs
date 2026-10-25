use std::sync::Arc;

use dependency_factory::{BuildError, Builder, DependencyFactory, Singleton};

struct Config {
    greeting: String,
}

impl Singleton for Config {
    fn build(_builder: &mut Builder) -> Result<Arc<Self>, BuildError> {
        Ok(Arc::new(Config {
            greeting: "hello".into(),
        }))
    }
}

struct Greeter {
    config: Arc<Config>,
}

impl Singleton for Greeter {
    fn build(builder: &mut Builder) -> Result<Arc<Self>, BuildError> {
        Ok(Arc::new(Greeter {
            config: builder.get_or_build::<Config>()?,
        }))
    }
}

struct Service {
    greeter: Arc<Greeter>,
    name: String,
}

impl Singleton for Service {
    fn build(builder: &mut Builder) -> Result<Arc<Self>, BuildError> {
        Ok(Arc::new(Service {
            greeter: builder.get_or_build::<Greeter>()?,
            name: "world".into(),
        }))
    }
}

#[test]
fn auto_builds_a_dependency_chain() {
    let factory = DependencyFactory::new();
    let service = factory.get_or_build::<Service>().unwrap();
    assert_eq!(service.greeter.config.greeting, "hello");
    assert_eq!(service.name, "world");
}

#[test]
fn build_caches_each_resource() {
    let factory = DependencyFactory::new();
    let s1 = factory.get_or_build::<Service>().unwrap();
    let s2 = factory.get_or_build::<Service>().unwrap();
    assert!(Arc::ptr_eq(&s1, &s2));
    let g1 = factory.get_or_build::<Greeter>().unwrap();
    assert!(Arc::ptr_eq(&s1.greeter, &g1));
}

#[test]
fn pre_inserted_resource_short_circuits_build() {
    let factory = DependencyFactory::new();
    let custom = Arc::new(Config {
        greeting: "g'day".into(),
    });
    factory
        .registry()
        .begin()
        .insert(Arc::clone(&custom))
        .unwrap();
    let greeter = factory.get_or_build::<Greeter>().unwrap();
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
    fn build(_builder: &mut Builder) -> Result<Arc<Self>, BuildError> {
        Err(Upstream.into())
    }
}

#[derive(Debug)]
#[allow(dead_code)]
struct DependsOnFailing {
    f: Arc<Failing>,
}

impl Singleton for DependsOnFailing {
    fn build(builder: &mut Builder) -> Result<Arc<Self>, BuildError> {
        Ok(Arc::new(DependsOnFailing {
            f: builder.get_or_build::<Failing>()?,
        }))
    }
}

#[test]
fn errors_carry_a_chain_of_frames() {
    let factory = DependencyFactory::new();
    let err = factory.get_or_build::<DependsOnFailing>().unwrap_err();
    let chain = err.chain();
    assert_eq!(chain.len(), 2);
    assert!(
        chain[0].type_name().ends_with("Failing"),
        "innermost frame: {}",
        chain[0]
    );
    assert!(
        chain[1].type_name().ends_with("DependsOnFailing"),
        "outermost frame: {}",
        chain[1],
    );
    // A singleton has no key to show.
    assert!(chain.iter().all(|frame| frame.key().is_none()));
    let rendered = format!("{err}");
    assert!(rendered.contains("DependsOnFailing"));
    assert!(rendered.contains("Failing"));
    assert!(rendered.contains("upstream is unavailable"));
    assert!(!rendered.contains(" for "), "{rendered}");
}
