use std::error::Error;
use std::sync::Arc;

use dependency_factory::{BuildError, Builder, DependencyFactory, ResolveError, Singleton};

mod common;

#[derive(Debug)]
struct Upstream;

impl std::fmt::Display for Upstream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("upstream is unavailable")
    }
}

impl Error for Upstream {}

#[derive(Debug)]
struct Failing;

impl Singleton for Failing {
    fn build(_builder: &mut Builder) -> Result<Self, BuildError> {
        Err(Upstream.into())
    }
}

#[derive(Debug)]
#[allow(dead_code)]
struct Middle {
    failing: Arc<Failing>,
}

impl Singleton for Middle {
    fn build(builder: &mut Builder) -> Result<Self, BuildError> {
        Ok(Middle {
            failing: builder.get_or_build()?,
        })
    }
}

#[derive(Debug)]
#[allow(dead_code)]
struct Top {
    middle: Arc<Middle>,
}

impl Singleton for Top {
    fn build(builder: &mut Builder) -> Result<Self, BuildError> {
        Ok(Top {
            middle: builder.get_or_build()?,
        })
    }
}

// --- Each frame is a link in the source chain --------------------------------

#[test]
fn frames_are_links_in_the_source_chain_outermost_first() {
    let factory = DependencyFactory::new();
    let err = factory.get_or_build::<Top>().unwrap_err();
    assert_eq!(
        common::report(&err),
        "failed to build errors::Top\n\
         failed to build errors::Middle\n\
         failed to build errors::Failing\n\
         upstream is unavailable"
    );
    assert_eq!(err.chain().len(), 3);
    assert!(err.underlying().is::<Upstream>());
}

#[test]
fn an_error_without_frames_is_its_cause() {
    let factory = DependencyFactory::new();
    let handle = factory.handle();
    drop(factory);
    let err = handle.get::<Top>().unwrap_err();
    assert!(err.chain().is_empty());
    assert_eq!(err.to_string(), "the DependencyFactory has been dropped");
    assert!(err.source().is_none());
}

// --- `?` out of the crate ----------------------------------------------------

fn resolve_with_anyhow(factory: &DependencyFactory) -> anyhow::Result<Arc<Top>> {
    Ok(factory.get_or_build::<Top>()?)
}

#[test]
fn question_mark_converts_into_anyhow_keeping_the_chain() {
    let factory = DependencyFactory::new();
    let err = resolve_with_anyhow(&factory).unwrap_err();
    assert!(err.root_cause().is::<Upstream>());
    assert!(err.downcast_ref::<ResolveError>().is_some());
    assert_eq!(
        format!("{err:#}"),
        "failed to build errors::Top: failed to build errors::Middle: \
         failed to build errors::Failing: upstream is unavailable"
    );
}

fn build_with_box_dyn(factory: &DependencyFactory) -> Result<(), Box<dyn Error + Send + Sync>> {
    factory.builder().get_or_build::<Top>()?;
    Ok(())
}

#[test]
fn question_mark_converts_a_build_error_into_box_dyn_error() {
    let factory = DependencyFactory::new();
    let err = build_with_box_dyn(&factory).unwrap_err();
    let err = err.downcast::<ResolveError>().unwrap();
    assert_eq!(err.chain().len(), 3);
    assert!(err.underlying().is::<Upstream>());
}

#[test]
fn a_build_error_converts_into_a_resolve_error() {
    let factory = DependencyFactory::new();
    let err = ResolveError::from(factory.builder().get_or_build::<Top>().unwrap_err());
    assert_eq!(err.to_string(), "failed to build errors::Top");
    assert!(err.underlying().is::<Upstream>());
}

// --- Into a `BuildError` -----------------------------------------------------

#[derive(Debug)]
struct Unconfigured;

impl Singleton for Unconfigured {
    fn build(_builder: &mut Builder) -> Result<Self, BuildError> {
        Err(BuildError::new("FOO is not set"))
    }
}

#[test]
fn a_build_error_can_be_made_from_a_message() {
    let factory = DependencyFactory::new();
    let err = factory.get_or_build::<Unconfigured>().unwrap_err();
    assert_eq!(err.underlying().to_string(), "FOO is not set");
}

fn read_settings() -> anyhow::Result<()> {
    Err(anyhow::Error::new(Upstream).context("reading settings"))
}

#[derive(Debug)]
struct Settings;

impl Singleton for Settings {
    fn build(_builder: &mut Builder) -> Result<Self, BuildError> {
        read_settings().map_err(BuildError::new)?;
        Ok(Settings)
    }
}

#[test]
fn a_build_error_can_be_made_from_an_anyhow_error_keeping_its_chain() {
    let factory = DependencyFactory::new();
    let err = resolve_settings_with_anyhow(&factory).unwrap_err();
    assert!(err.root_cause().is::<Upstream>());
    assert_eq!(
        format!("{err:#}"),
        "failed to build errors::Settings: reading settings: upstream is unavailable"
    );
}

fn resolve_settings_with_anyhow(factory: &DependencyFactory) -> anyhow::Result<Arc<Settings>> {
    Ok(factory.get_or_build::<Settings>()?)
}

/// Resolves `Top` from a factory of its own, as code inside a `build`
/// might through some other component's API.
#[derive(Debug)]
struct Elsewhere;

impl Singleton for Elsewhere {
    fn build(_builder: &mut Builder) -> Result<Self, BuildError> {
        DependencyFactory::new().get_or_build::<Top>()?;
        Ok(Elsewhere)
    }
}

#[test]
fn a_resolve_error_inside_build_is_spliced_rather_than_nested() {
    let factory = DependencyFactory::new();
    let err = factory.get_or_build::<Elsewhere>().unwrap_err();
    let chain: Vec<&str> = err
        .chain()
        .iter()
        .map(|f| f.type_name().rsplit("::").next().unwrap())
        .collect();
    assert_eq!(chain, ["Failing", "Middle", "Top", "Elsewhere"]);
    assert!(err.underlying().is::<Upstream>());
}
