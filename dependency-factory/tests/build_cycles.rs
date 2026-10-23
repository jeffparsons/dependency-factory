use std::sync::Arc;

use dependency_factory::{
    BuildError, Builder, CycleError, DependencyFactory, DependencyFactoryHandle, Key, Query,
    ResolveError, Singleton,
};

mod common;

fn cycle_of(err: &ResolveError) -> &CycleError {
    err.underlying()
        .downcast_ref::<CycleError>()
        .expect("expected the underlying cause to be a CycleError")
}

// --- Direct singleton self-cycle: A -> A -----------------------------------

#[derive(Debug)]
struct Selfish;

impl Singleton for Selfish {
    fn build(builder: &mut Builder) -> Result<Self, BuildError> {
        let _self = builder.get_or_build::<Selfish>()?;
        Ok(Selfish)
    }
}

#[test]
fn singleton_self_cycle_is_detected() {
    let factory = DependencyFactory::new();
    let err = factory.get_or_build::<Selfish>().unwrap_err();
    let cycle = cycle_of(&err);
    assert_eq!(cycle.path().len(), 2);
    assert!(cycle.path()[0].type_name().ends_with("Selfish"));
    assert!(cycle.path()[1].type_name().ends_with("Selfish"));
    assert!(cycle.path().iter().all(|frame| frame.key().is_none()));
}

// --- Two-step singleton cycle: A -> B -> A ---------------------------------

#[derive(Debug)]
#[allow(dead_code)]
struct Alpha {
    b: Arc<Beta>,
}

#[derive(Debug)]
#[allow(dead_code)]
struct Beta {
    a: Arc<Alpha>,
}

impl Singleton for Alpha {
    fn build(builder: &mut Builder) -> Result<Self, BuildError> {
        Ok(Alpha {
            b: builder.get_or_build::<Beta>()?,
        })
    }
}

impl Singleton for Beta {
    fn build(builder: &mut Builder) -> Result<Self, BuildError> {
        Ok(Beta {
            a: builder.get_or_build::<Alpha>()?,
        })
    }
}

#[test]
fn two_step_singleton_cycle_is_detected() {
    let factory = DependencyFactory::new();
    let err = factory.get_or_build::<Alpha>().unwrap_err();
    let cycle = cycle_of(&err);
    let path = cycle.path();
    assert_eq!(path.len(), 3);
    assert!(path[0].type_name().ends_with("Alpha"));
    assert!(path[1].type_name().ends_with("Beta"));
    assert!(path[2].type_name().ends_with("Alpha"));
    let rendered = common::report(&err);
    assert!(rendered.contains("dependency cycle detected"));
}

// --- Query cycle on different keys: Tree(1) -> Tree(2) -> Tree(1) ----------

#[derive(Debug)]
struct Tree;

#[derive(Debug, Hash, Eq, PartialEq, Clone, Copy)]
struct TreeKey(u32);

impl Key for TreeKey {
    type Value = Tree;
}

impl Query for TreeKey {
    fn build(&self, builder: &mut Builder) -> Result<Tree, BuildError> {
        match self.0 {
            1 => {
                let _ = builder.get_or_build_for(TreeKey(2))?;
            }
            2 => {
                let _ = builder.get_or_build_for(TreeKey(1))?;
            }
            _ => {}
        }
        Ok(Tree)
    }
}

#[test]
fn query_cycle_on_different_keys_is_detected() {
    let factory = DependencyFactory::new();
    let err = factory.get_or_build_for(TreeKey(1)).unwrap_err();
    let keys: Vec<Option<&str>> = cycle_of(&err).path().iter().map(|f| f.key()).collect();
    assert_eq!(
        keys,
        [Some("TreeKey(1)"), Some("TreeKey(2)"), Some("TreeKey(1)")]
    );
    let rendered = common::report(&err);
    assert!(
        rendered.contains("Tree for TreeKey(1) -> build_cycles::Tree for TreeKey(2) -> "),
        "{rendered}"
    );
}

// --- Keys that hash alike but differ: Tenant(1, 0) -> Tenant(1, 1) --------

/// A key whose `Hash` covers only part of what `Eq` compares. That is
/// allowed: equal keys must hash alike, but unequal keys may too.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TenantKey {
    id: u32,
    region: u8,
}

impl std::hash::Hash for TenantKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl Key for TenantKey {
    type Value = String;
}

impl Query for TenantKey {
    fn build(&self, builder: &mut Builder) -> Result<String, BuildError> {
        match self.region {
            // A different key of the same tenant, with the same hash.
            0 => {
                builder.get_or_build_for(TenantKey {
                    id: self.id,
                    region: 1,
                })?;
            }
            // Itself.
            2 => {
                builder.get_or_build_for(TenantKey {
                    id: self.id,
                    region: 2,
                })?;
            }
            _ => {}
        }
        Ok(format!("{}-{}", self.id, self.region))
    }
}

#[test]
fn keys_that_hash_alike_but_differ_are_not_a_cycle() {
    let factory = DependencyFactory::new();
    let value = factory
        .get_or_build_for(TenantKey { id: 1, region: 0 })
        .expect("distinct keys with equal hashes must not look like a cycle");
    assert_eq!(*value, "1-0");
    let dependency = factory
        .get_or_build_for(TenantKey { id: 1, region: 1 })
        .unwrap();
    assert_eq!(*dependency, "1-1");
}

#[test]
fn a_key_that_hashes_like_others_still_detects_its_own_cycle() {
    let factory = DependencyFactory::new();
    let err = factory
        .get_or_build_for(TenantKey { id: 1, region: 2 })
        .unwrap_err();
    assert_eq!(cycle_of(&err).path().len(), 2);
}

// --- Query self-cycle on the same key --------------------------------------

#[derive(Debug, Hash, Eq, PartialEq, Clone, Copy)]
struct SelfishKey;

impl Key for SelfishKey {
    type Value = ();
}

impl Query for SelfishKey {
    fn build(&self, builder: &mut Builder) -> Result<(), BuildError> {
        builder.get_or_build_for(SelfishKey)?;
        Ok(())
    }
}

#[test]
fn query_self_cycle_is_detected() {
    let factory = DependencyFactory::new();
    let err = factory.get_or_build_for(SelfishKey).unwrap_err();
    cycle_of(&err);
}

// --- Tree-shaped non-cycle: Linear(1) -> Linear(2) -> Linear(3) ------------

#[derive(Debug)]
struct Linear;

#[derive(Debug, Hash, Eq, PartialEq, Clone, Copy)]
struct LinearKey(u32);

impl Key for LinearKey {
    type Value = Linear;
}

impl Query for LinearKey {
    fn build(&self, builder: &mut Builder) -> Result<Linear, BuildError> {
        if self.0 < 3 {
            builder.get_or_build_for(LinearKey(self.0 + 1))?;
        }
        Ok(Linear)
    }
}

#[test]
fn linear_chain_of_distinct_keys_is_not_a_false_positive() {
    let factory = DependencyFactory::new();
    factory
        .get_or_build_for(LinearKey(1))
        .expect("a linear chain of distinct keys must build successfully");
}

// --- Mixed singleton + query cycle -----------------------------------------

#[derive(Debug)]
struct Hub;

impl Singleton for Hub {
    fn build(builder: &mut Builder) -> Result<Self, BuildError> {
        builder.get_or_build_for(SpokeKey("only"))?;
        Ok(Hub)
    }
}

#[derive(Debug, Hash, Eq, PartialEq, Clone)]
struct SpokeKey(&'static str);

#[derive(Debug)]
struct Spoke;

impl Key for SpokeKey {
    type Value = Spoke;
}

impl Query for SpokeKey {
    fn build(&self, builder: &mut Builder) -> Result<Spoke, BuildError> {
        builder.get_or_build::<Hub>()?;
        Ok(Spoke)
    }
}

#[test]
fn mixed_singleton_and_query_cycle_is_detected() {
    let factory = DependencyFactory::new();
    let err = factory.get_or_build::<Hub>().unwrap_err();
    cycle_of(&err);
}

// --- Frame separation across factories -------------------------------------

struct Bridged {
    local: Arc<Bridgeable>,
    remote: Arc<Bridgeable>,
}

struct Bridgeable;

impl Singleton for Bridgeable {
    fn build(_builder: &mut Builder) -> Result<Self, BuildError> {
        Ok(Bridgeable)
    }
}

impl Singleton for Bridged {
    fn build(builder: &mut Builder) -> Result<Self, BuildError> {
        let local = builder.get_or_build::<Bridgeable>()?;
        // The test pre-inserts a handle to the *other* factory into this
        // one, so this build can recurse into a different factory while
        // resolving the same Rust type.
        let other = builder
            .get::<DependencyFactoryHandle>()
            .expect("test setup must insert the other factory's handle");
        // A different registry has its own lock, so this nested builder
        // does not wait on the one we hold.
        let remote = other.get_or_build::<Bridgeable>()?;
        Ok(Bridged { local, remote })
    }
}

#[test]
fn frames_in_distinct_factories_do_not_collide() {
    let factory_a = DependencyFactory::new();
    let factory_b = DependencyFactory::new();
    factory_a
        .registry()
        .begin()
        .insert(factory_b.handle())
        .unwrap();

    let bridged = factory_a
        .get_or_build::<Bridged>()
        .expect("recursing into a different factory for the same type must not look like a cycle");
    assert!(!Arc::ptr_eq(&bridged.local, &bridged.remote));
}

// --- Breadcrumb is balanced after a non-cycle error ------------------------

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
    fn build(_builder: &mut Builder) -> Result<Self, BuildError> {
        Err(Upstream.into())
    }
}

#[derive(Debug)]
struct OkResource;

impl Singleton for OkResource {
    fn build(_builder: &mut Builder) -> Result<Self, BuildError> {
        Ok(OkResource)
    }
}

#[test]
fn breadcrumb_is_balanced_after_failed_build() {
    let factory = DependencyFactory::new();
    // First, a build that fails for a reason other than a cycle.
    let _ = factory.get_or_build::<Failing>().unwrap_err();
    // Then, a completely unrelated build on the same thread must succeed
    // without a stale breadcrumb entry causing a spurious cycle error.
    factory.get_or_build::<OkResource>().unwrap();
}
