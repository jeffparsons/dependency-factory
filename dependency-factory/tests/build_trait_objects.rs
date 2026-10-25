//! `Singleton` for a trait object, binding the trait to an implementation,
//! and the other things a `build` that returns an `Arc` can do.

use std::sync::{Arc, Weak};

use dependency_factory::{BuildError, Builder, DependencyFactory, Singleton};

/// Whether two `Arc`s, of any types, share one allocation. `Arc::ptr_eq`
/// is for one type; this compares an `Arc<Smtp>` with the
/// `Arc<dyn Mailer>` coerced from it.
fn same_allocation<A: ?Sized, B: ?Sized>(a: &Arc<A>, b: &Arc<B>) -> bool {
    std::ptr::addr_eq(Arc::as_ptr(a), Arc::as_ptr(b))
}

trait Mailer: Send + Sync {
    fn transport(&self) -> &'static str;
}

trait WarmUp: Send + Sync {
    fn warm_up(&self);
}

struct Config {
    dry_run: bool,
}

impl Singleton for Config {
    fn build(_: &mut Builder) -> Result<Arc<Self>, BuildError> {
        Ok(Arc::new(Config { dry_run: false }))
    }
}

#[derive(Singleton)]
#[factory(index(dyn WarmUp))]
struct Smtp {
    _config: Arc<Config>,
}

impl Mailer for Smtp {
    fn transport(&self) -> &'static str {
        "smtp"
    }
}

impl WarmUp for Smtp {
    fn warm_up(&self) {}
}

struct DryRun;

impl Mailer for DryRun {
    fn transport(&self) -> &'static str {
        "none"
    }
}

// The binding: which implementation stands in for the trait. Resolving
// Smtp's own singleton fills both slots with one instance; the choice
// can depend on anything the builder can resolve.
impl Singleton for dyn Mailer {
    fn build(builder: &mut Builder) -> Result<Arc<Self>, BuildError> {
        if builder.get_or_build::<Config>()?.dry_run {
            Ok(Arc::new(DryRun))
        } else {
            Ok(builder.get_or_build::<Smtp>()?)
        }
    }
}

#[derive(Singleton)]
struct Notifier {
    mailer: Arc<dyn Mailer>,
}

#[test]
fn a_trait_object_slot_is_filled_by_its_binding() {
    let factory = DependencyFactory::new();
    let mailer = factory.get_or_build::<dyn Mailer>().unwrap();
    assert_eq!(mailer.transport(), "smtp");
    // Both slots hold the one Smtp, and it registered itself.
    let smtp = factory
        .get::<Smtp>()
        .expect("the implementation's own slot is filled too");
    assert!(same_allocation(&mailer, &smtp));
    let warm_ups = factory.indexed::<dyn WarmUp>();
    assert_eq!(warm_ups.len(), 1);
    assert!(same_allocation(&warm_ups[0], &mailer));
    for resource in &warm_ups {
        resource.warm_up();
    }
    // Cached like any singleton.
    let again = factory.get_or_build::<dyn Mailer>().unwrap();
    assert!(same_allocation(&again, &mailer));
}

#[test]
fn the_derive_resolves_a_trait_object_field() {
    let factory = DependencyFactory::new();
    let notifier = factory.get_or_build::<Notifier>().unwrap();
    assert_eq!(notifier.mailer.transport(), "smtp");
    let smtp = factory.get::<Smtp>().unwrap();
    assert!(same_allocation(&notifier.mailer, &smtp));
}

struct FakeMailer;

impl Mailer for FakeMailer {
    fn transport(&self) -> &'static str {
        "fake"
    }
}

#[test]
fn a_fake_in_the_slot_stands_in_for_the_binding() {
    let factory = DependencyFactory::new();
    factory
        .begin()
        .insert::<dyn Mailer>(Arc::new(FakeMailer))
        .unwrap();
    let notifier = factory.get_or_build::<Notifier>().unwrap();
    assert_eq!(notifier.mailer.transport(), "fake");
    assert!(
        factory.get::<Smtp>().is_none(),
        "the real implementation was never built"
    );
    assert!(factory.indexed::<dyn WarmUp>().is_empty());
}

#[test]
fn a_binding_may_choose_the_implementation_at_run_time() {
    let factory = DependencyFactory::new();
    factory
        .begin()
        .insert(Arc::new(Config { dry_run: true }))
        .unwrap();
    let mailer = factory.get_or_build::<dyn Mailer>().unwrap();
    assert_eq!(mailer.transport(), "none");
    assert!(factory.get::<Smtp>().is_none());
}

#[test]
fn trait_object_slots_through_a_handle_and_a_held_builder() {
    let factory = DependencyFactory::new();
    let handle = factory.handle().upgrade().unwrap();
    assert!(handle.get::<dyn Mailer>().is_none());
    let mailer = handle.get_or_build::<dyn Mailer>().unwrap();
    let seen = handle.get::<dyn Mailer>().unwrap();
    assert!(same_allocation(&seen, &mailer));
    let mut builder = factory.builder();
    assert!(same_allocation(
        &builder.get::<dyn Mailer>().unwrap(),
        &mailer
    ));
    let built = builder.get_or_build::<dyn Mailer>().unwrap();
    assert!(same_allocation(&built, &mailer));
}

// --- A build need not allocate ---------------------------------------------

/// Obtained as an `Arc` from elsewhere, say a pool, rather than made on
/// the spot.
struct Pool {
    id: u32,
}

static SHARED_POOL: std::sync::LazyLock<Arc<Pool>> =
    std::sync::LazyLock::new(|| Arc::new(Pool { id: 7 }));

impl Singleton for Pool {
    fn build(_: &mut Builder) -> Result<Arc<Self>, BuildError> {
        Ok(Arc::clone(&SHARED_POOL))
    }
}

#[derive(Singleton)]
struct UsesPool {
    pool: Arc<Pool>,
}

#[test]
fn a_build_may_hand_over_an_arc_it_already_holds() {
    let factory = DependencyFactory::new();
    let pool = factory.get_or_build::<Pool>().unwrap();
    assert_eq!(pool.id, 7);
    assert!(Arc::ptr_eq(&pool, &SHARED_POOL));
    let uses_pool = factory.get_or_build::<UsesPool>().unwrap();
    assert!(Arc::ptr_eq(&uses_pool.pool, &SHARED_POOL));
}

/// Keeps a `Weak` to itself, for callbacks it hands out later.
struct SelfAware {
    me: Weak<SelfAware>,
}

impl Singleton for SelfAware {
    fn build(_: &mut Builder) -> Result<Arc<Self>, BuildError> {
        Ok(Arc::new_cyclic(|me| SelfAware { me: me.clone() }))
    }
}

#[test]
fn a_build_may_give_the_resource_a_weak_to_itself() {
    let factory = DependencyFactory::new();
    let resource = factory.get_or_build::<SelfAware>().unwrap();
    let upgraded = resource.me.upgrade().expect("the registry keeps it alive");
    assert!(Arc::ptr_eq(&upgraded, &resource));
}

// --- Errors -----------------------------------------------------------------

#[derive(Debug)]
struct Upstream;

impl std::fmt::Display for Upstream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("upstream is unavailable")
    }
}

impl std::error::Error for Upstream {}

trait Weather: Send + Sync {}

#[derive(Debug)]
struct Station;

impl Singleton for Station {
    fn build(_: &mut Builder) -> Result<Arc<Self>, BuildError> {
        Err(Upstream.into())
    }
}

impl Weather for Station {}

impl Singleton for dyn Weather {
    fn build(builder: &mut Builder) -> Result<Arc<Self>, BuildError> {
        Ok(builder.get_or_build::<Station>()?)
    }
}

#[test]
fn an_error_names_the_trait_object_and_the_implementation() {
    let factory = DependencyFactory::new();
    let err = factory
        .get_or_build::<dyn Weather>()
        .err()
        .expect("Station fails to build");
    let chain: Vec<&str> = err.chain().iter().map(|f| f.type_name()).collect();
    assert_eq!(chain.len(), 2, "{chain:?}");
    assert!(chain[0].ends_with("Station"), "{chain:?}");
    assert_eq!(chain[1], "dyn build_trait_objects::Weather");
    assert!(err.chain().iter().all(|f| f.key().is_none()));
    assert!(format!("{err:?}").contains("Upstream"), "{err:?}");
}

trait Clock: Send + Sync {}

struct SystemClock {
    _scheduler: Arc<Scheduler>,
}

impl Singleton for SystemClock {
    fn build(builder: &mut Builder) -> Result<Arc<Self>, BuildError> {
        Ok(Arc::new(SystemClock {
            _scheduler: builder.get_or_build()?,
        }))
    }
}

impl Clock for SystemClock {}

impl Singleton for dyn Clock {
    fn build(builder: &mut Builder) -> Result<Arc<Self>, BuildError> {
        Ok(builder.get_or_build::<SystemClock>()?)
    }
}

#[derive(Singleton)]
struct Scheduler {
    _clock: Arc<dyn Clock>,
}

#[test]
fn a_cycle_through_a_trait_object_is_detected() {
    let factory = DependencyFactory::new();
    let err = factory.get_or_build::<Scheduler>().err().expect("a cycle");
    // Scheduler -> dyn Clock -> SystemClock -> Scheduler, as the
    // CycleError's path, which Debug renders.
    let rendered = format!("{err:?}");
    assert!(rendered.contains("CycleError"), "{rendered}");
    assert!(
        rendered.contains("dyn build_trait_objects::Clock"),
        "{rendered}"
    );
    assert!(rendered.contains("SystemClock"), "{rendered}");
}
