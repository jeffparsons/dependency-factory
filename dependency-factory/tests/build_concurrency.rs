//! The `Builder` holds a registry transaction plus the build machinery:
//! at most one transaction per registry, held across user `build` code. These
//! tests pin down what that buys (builds run once, never overlap, cycles
//! are always same-builder cycles), that lookups never wait for it, and
//! how it behaves across threads and panics.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::Duration;

use dependency_factory::{
    BuildError, Builder, CycleError, DependencyFactory, DependencyFactoryHandle, Key, Query,
    Registry, Singleton,
};

/// Aborts the process if the test has not finished within ten seconds, so
/// a regression into a deadlock fails CI instead of hanging it.
struct Watchdog(#[allow(dead_code)] mpsc::Sender<()>);

fn watchdog() -> Watchdog {
    let (tx, rx) = mpsc::channel::<()>();
    thread::spawn(move || {
        if let Err(RecvTimeoutError::Timeout) = rx.recv_timeout(Duration::from_secs(10)) {
            eprintln!("watchdog: test did not finish in time");
            std::process::abort();
        }
    });
    Watchdog(tx)
}

fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn factory_types_are_send_and_sync() {
    assert_send_sync::<Registry>();
    assert_send_sync::<DependencyFactory>();
    assert_send_sync::<DependencyFactoryHandle>();
}

// --- One build per key, however many threads ask ---------------------------

static SLOW_BUILDS: AtomicUsize = AtomicUsize::new(0);

struct Slow;

impl Singleton for Slow {
    fn build(_builder: &mut Builder) -> Result<Arc<Self>, BuildError> {
        SLOW_BUILDS.fetch_add(1, Ordering::SeqCst);
        // Sleep so the other threads' requests arrive while this build is
        // in progress.
        thread::sleep(Duration::from_millis(50));
        Ok(Arc::new(Slow))
    }
}

#[test]
fn concurrent_requests_for_one_key_build_it_once() {
    let _watchdog = watchdog();
    let factory = Arc::new(DependencyFactory::new());
    let barrier = Arc::new(Barrier::new(4));

    let workers: Vec<_> = (0..4)
        .map(|_| {
            let factory = Arc::clone(&factory);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                factory.get_or_build::<Slow>().unwrap()
            })
        })
        .collect();
    let arcs: Vec<Arc<Slow>> = workers.into_iter().map(|w| w.join().unwrap()).collect();

    assert_eq!(SLOW_BUILDS.load(Ordering::SeqCst), 1);
    for arc in &arcs[1..] {
        assert!(Arc::ptr_eq(&arcs[0], arc));
    }
}

// --- Builds of different keys never overlap --------------------------------

static IN_BUILD: AtomicBool = AtomicBool::new(false);
static OVERLAPPED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Hash, Eq, PartialEq, Clone)]
struct ExclusiveKey(u8);

impl Key for ExclusiveKey {
    type Value = ();
}

impl Query for ExclusiveKey {
    fn build(&self, _builder: &mut Builder) -> Result<Arc<()>, BuildError> {
        if IN_BUILD.swap(true, Ordering::SeqCst) {
            OVERLAPPED.store(true, Ordering::SeqCst);
        }
        thread::sleep(Duration::from_millis(20));
        IN_BUILD.store(false, Ordering::SeqCst);
        Ok(Arc::new(()))
    }
}

#[test]
fn builds_of_different_keys_are_serialised() {
    let _watchdog = watchdog();
    let factory = Arc::new(DependencyFactory::new());
    let barrier = Arc::new(Barrier::new(2));

    let workers: Vec<_> = (0..2)
        .map(|i| {
            let factory = Arc::clone(&factory);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                factory.get_or_build_for(ExclusiveKey(i)).unwrap();
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }

    assert!(!OVERLAPPED.load(Ordering::SeqCst));
}

// --- A cycle split across threads is still reported ------------------------

#[derive(Debug)]
struct Ping;

#[derive(Debug)]
struct Pong;

impl Singleton for Ping {
    fn build(builder: &mut Builder) -> Result<Arc<Self>, BuildError> {
        thread::sleep(Duration::from_millis(10));
        builder.get_or_build::<Pong>()?;
        Ok(Arc::new(Ping))
    }
}

impl Singleton for Pong {
    fn build(builder: &mut Builder) -> Result<Arc<Self>, BuildError> {
        thread::sleep(Duration::from_millis(10));
        builder.get_or_build::<Ping>()?;
        Ok(Arc::new(Pong))
    }
}

#[test]
fn cycle_split_across_threads_is_reported_on_both() {
    let _watchdog = watchdog();
    let factory = Arc::new(DependencyFactory::new());
    let barrier = Arc::new(Barrier::new(2));

    let ping = {
        let factory = Arc::clone(&factory);
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || {
            barrier.wait();
            factory.get_or_build::<Ping>().map(|_| ())
        })
    };
    let pong = {
        let factory = Arc::clone(&factory);
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || {
            barrier.wait();
            factory.get_or_build::<Pong>().map(|_| ())
        })
    };

    for outcome in [ping.join().unwrap(), pong.join().unwrap()] {
        let err = outcome.unwrap_err();
        assert!(
            err.source().downcast_ref::<CycleError>().is_some(),
            "expected a CycleError, got: {err}"
        );
    }
}

// --- Holding a builder across calls -----------------------------------------

#[derive(Debug)]
struct Plain;

impl Singleton for Plain {
    fn build(_builder: &mut Builder) -> Result<Arc<Self>, BuildError> {
        Ok(Arc::new(Plain))
    }
}

struct Other;

impl Singleton for Other {
    fn build(_builder: &mut Builder) -> Result<Arc<Self>, BuildError> {
        Ok(Arc::new(Other))
    }
}

#[test]
fn builder_can_be_held_across_calls() {
    let factory = DependencyFactory::new();
    let mut builder = factory.builder();
    let plain = builder.get_or_build::<Plain>().unwrap();
    builder.get_or_build::<Other>().unwrap();
    // A second request through the same builder is a cache hit.
    assert!(Arc::ptr_eq(
        &plain,
        &builder.get_or_build::<Plain>().unwrap()
    ));
    drop(builder);
    factory.builder().get_or_build::<Plain>().unwrap();
}

#[test]
fn builder_can_be_taken_through_a_handle() {
    let factory = DependencyFactory::new();
    let handle = factory.handle();
    let upgraded = handle.upgrade().unwrap();
    let mut builder = upgraded.builder();
    builder.get_or_build::<Plain>().unwrap();
    assert!(Arc::ptr_eq(builder.registry(), factory.registry()));
}

#[test]
fn what_a_builder_commits_is_published_when_it_drops() {
    let factory = DependencyFactory::new();
    let mut builder = factory.builder();
    let built = builder.get_or_build::<Plain>().unwrap();
    // The builder sees its own commit; nobody else does yet.
    assert!(builder.get::<Plain>().is_some());
    assert!(factory.get::<Plain>().is_none());
    assert!(factory.handle().upgrade().unwrap().get::<Plain>().is_none());
    drop(builder);
    assert!(Arc::ptr_eq(&built, &factory.get::<Plain>().unwrap()));
}

#[test]
fn a_registry_transaction_holds_off_builders_on_other_threads() {
    let _watchdog = watchdog();
    let factory = Arc::new(DependencyFactory::new());
    let released = Arc::new(AtomicBool::new(false));
    let (held_tx, held_rx) = mpsc::channel();

    let holder = {
        let factory = Arc::clone(&factory);
        let released = Arc::clone(&released);
        thread::spawn(move || {
            let tx = factory.registry().begin();
            held_tx.send(()).unwrap();
            thread::sleep(Duration::from_millis(50));
            released.store(true, Ordering::SeqCst);
            drop(tx);
        })
    };

    held_rx.recv().unwrap();
    // A plain read goes through while the transaction is open elsewhere...
    assert!(factory.get::<Plain>().is_none());
    // ...but a build waits for it.
    factory.get_or_build::<Plain>().unwrap();
    assert!(released.load(Ordering::SeqCst));
    holder.join().unwrap();
}

#[test]
fn taking_a_builder_waits_for_the_other_thread_to_drop_its_builder() {
    let _watchdog = watchdog();
    let factory = Arc::new(DependencyFactory::new());
    let released = Arc::new(AtomicBool::new(false));
    let (held_tx, held_rx) = mpsc::channel();

    let holder = {
        let factory = Arc::clone(&factory);
        let released = Arc::clone(&released);
        thread::spawn(move || {
            let builder = factory.builder();
            held_tx.send(()).unwrap();
            thread::sleep(Duration::from_millis(50));
            released.store(true, Ordering::SeqCst);
            drop(builder);
        })
    };

    held_rx.recv().unwrap();
    let _builder = factory.builder();
    assert!(released.load(Ordering::SeqCst));
    holder.join().unwrap();
}

#[test]
fn get_never_waits_for_a_builder() {
    let _watchdog = watchdog();
    let factory = DependencyFactory::new();
    factory.registry().begin().insert(Arc::new(7_u32)).unwrap();
    let _builder = factory.builder();
    // Same thread, builder held: a lookup must neither wait nor error.
    assert_eq!(*factory.get::<u32>().unwrap(), 7);
    assert_eq!(
        *factory.handle().upgrade().unwrap().get::<u32>().unwrap(),
        7
    );
}

// --- Reading through a handle inside a build -------------------------------

struct ReadsViaHandle {
    seen: Option<Arc<u32>>,
}

impl Singleton for ReadsViaHandle {
    fn build(builder: &mut Builder) -> Result<Arc<Self>, BuildError> {
        // A read through an upgraded handle never takes the lock this
        // builder holds, so it is safe; it sees what was last published.
        let seen = builder
            .handle()
            .upgrade()
            .expect("the factory is alive while it builds")
            .get::<u32>();
        Ok(Arc::new(ReadsViaHandle { seen }))
    }
}

#[test]
fn reading_through_a_handle_inside_build_does_not_wait() {
    let _watchdog = watchdog();
    let factory = DependencyFactory::new();
    factory.registry().begin().insert(Arc::new(7_u32)).unwrap();
    let built = factory.get_or_build::<ReadsViaHandle>().unwrap();
    assert_eq!(built.seen.as_deref(), Some(&7));
}

// --- Panics ------------------------------------------------------------------

#[derive(Debug)]
struct Panicking;

impl Singleton for Panicking {
    fn build(_builder: &mut Builder) -> Result<Arc<Self>, BuildError> {
        panic!("build blew up");
    }
}

#[test]
fn builder_is_released_when_a_build_panics() {
    let _watchdog = watchdog();
    let factory = Arc::new(DependencyFactory::new());
    let panicker = {
        let factory = Arc::clone(&factory);
        thread::spawn(move || factory.get_or_build::<Panicking>().map(|_| ()))
    };
    assert!(panicker.join().is_err(), "the panic must propagate");
    // Would hang here if the panicking thread's builder were still held.
    factory.get_or_build::<Plain>().unwrap();
}

struct PanicsAfterADependency;

impl Singleton for PanicsAfterADependency {
    fn build(builder: &mut Builder) -> Result<Arc<Self>, BuildError> {
        builder.get_or_build::<Plain>()?;
        panic!("after building Plain");
    }
}

#[test]
fn dependencies_built_before_a_panic_are_discarded() {
    let factory = DependencyFactory::new();
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        factory.get_or_build::<PanicsAfterADependency>()
    }));
    assert!(outcome.is_err());
    // The panic unwound through the builder, so its transaction was
    // discarded, Plain included.
    assert!(factory.get::<Plain>().is_none());
    assert!(factory.get::<PanicsAfterADependency>().is_none());
}

struct FailsAfterADependency;

impl Singleton for FailsAfterADependency {
    fn build(builder: &mut Builder) -> Result<Arc<Self>, BuildError> {
        builder.get_or_build::<Plain>()?;
        Err(std::io::Error::other("after building Plain").into())
    }
}

#[test]
fn dependencies_built_before_an_error_are_published() {
    let factory = DependencyFactory::new();
    assert!(factory.get_or_build::<FailsAfterADependency>().is_err());
    // An error ends one build, not the transaction.
    assert!(factory.get::<Plain>().is_some());
    assert!(factory.get::<FailsAfterADependency>().is_none());
}

#[test]
fn breadcrumb_is_balanced_after_a_panic_inside_a_held_builder() {
    let factory = DependencyFactory::new();
    let mut builder = factory.builder();
    let outcome = catch_unwind(AssertUnwindSafe(|| builder.get_or_build::<Panicking>()));
    assert!(outcome.is_err());
    // Retrying must panic again rather than report a cycle against the
    // frame the first attempt left behind.
    let outcome = catch_unwind(AssertUnwindSafe(|| builder.get_or_build::<Panicking>()));
    let payload = outcome.unwrap_err();
    assert_eq!(payload.downcast_ref::<&str>(), Some(&"build blew up"));
    builder.get_or_build::<Plain>().unwrap();
}

// --- Handles after teardown -------------------------------------------------

#[test]
fn a_handle_cannot_upgrade_after_the_factory_is_dropped() {
    let factory = DependencyFactory::new();
    let handle = factory.handle();
    assert!(handle.upgrade().is_some());
    drop(factory);
    assert!(handle.upgrade().is_none());
}
