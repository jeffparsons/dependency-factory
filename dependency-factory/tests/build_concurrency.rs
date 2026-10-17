//! Concurrent resolution of the same key: `build` runs at most once, other
//! threads wait for the result, failures let a waiter retry, and cycles
//! that span threads are reported rather than deadlocking.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use dependency_factory::{
    BuildError, CycleError, DependencyFactory, DependencyFactoryHandle, Query, Singleton,
};

/// Run a test body on its own thread and fail if it has not finished in
/// time. A regression in the waiting protocol shows up as a hang, and
/// without this `cargo test` would hang with it.
fn with_watchdog(body: impl FnOnce() + Send + 'static) {
    let (done_tx, done_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        body();
        done_tx
            .send(())
            .expect("watchdog receiver is alive until the body finishes");
    });
    match done_rx.recv_timeout(Duration::from_secs(10)) {
        // On a panic in the body, the sender is dropped and `join`
        // re-raises the panic with its original message.
        Ok(()) | Err(RecvTimeoutError::Disconnected) => worker.join().unwrap(),
        Err(RecvTimeoutError::Timeout) => {
            panic!("test did not finish within 10s; a build is probably deadlocked")
        }
    }
}

/// Long enough for a thread that is about to park to have parked, short
/// enough not to slow the suite down. Tests never depend on this for
/// correctness, only to make the overlap they are exercising likely.
fn settle() {
    thread::sleep(Duration::from_millis(50));
}

/// A one-shot gate: `wait` blocks until `open` has been called.
#[derive(Default)]
struct Latch {
    open: Mutex<bool>,
    changed: Condvar,
}

impl Latch {
    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.changed.notify_all();
    }

    fn wait(&self) {
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.changed.wait(open).unwrap();
        }
    }
}

/// Inserted into the factory so `build` bodies can coordinate with the
/// test without statics.
#[derive(Default)]
struct Control {
    started: Latch,
    release: Latch,
    builds: AtomicUsize,
}

fn control(factory: &DependencyFactoryHandle) -> Arc<Control> {
    factory
        .get::<Control>()
        .unwrap()
        .expect("test inserts Control before building")
}

#[derive(Debug)]
struct Boom;

impl std::fmt::Display for Boom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("boom")
    }
}

impl std::error::Error for Boom {}

fn cycle_of(err: &BuildError) -> &CycleError {
    err.source()
        .downcast_ref::<CycleError>()
        .expect("expected the underlying cause to be a CycleError")
}

// --- Same key, two threads: one build, one Arc ------------------------------

#[derive(Debug)]
struct Slow;

impl Singleton for Slow {
    fn build(factory: &DependencyFactoryHandle) -> Result<Self, BuildError> {
        let control = control(factory);
        control.builds.fetch_add(1, Ordering::SeqCst);
        control.started.open();
        control.release.wait();
        Ok(Slow)
    }
}

#[test]
fn concurrent_builds_of_same_key_build_once() {
    with_watchdog(|| {
        let factory = DependencyFactory::new();
        let control = factory.insert(Control::default());
        thread::scope(|s| {
            let first = s.spawn(|| factory.build::<Slow>());
            control.started.wait();
            let second = s.spawn(|| factory.build::<Slow>());
            settle();
            // The first build is still running: nothing is cached yet and
            // the second thread has not started a build of its own.
            assert!(factory.get::<Slow>().is_none());
            assert_eq!(control.builds.load(Ordering::SeqCst), 1);

            control.release.open();
            let a = first.join().unwrap().unwrap();
            let b = second.join().unwrap().unwrap();
            assert!(Arc::ptr_eq(&a, &b));
        });
        assert_eq!(control.builds.load(Ordering::SeqCst), 1);
    });
}

// --- Same key via `build_for` ----------------------------------------------

#[derive(Hash, PartialEq, Eq, Clone, Debug)]
struct SlowKey(u32);

impl Query for SlowKey {
    type Output = Slow;

    fn build(&self, factory: &DependencyFactoryHandle) -> Result<Slow, BuildError> {
        Slow::build(factory)
    }
}

#[test]
fn concurrent_build_for_of_same_key_builds_once() {
    with_watchdog(|| {
        let factory = DependencyFactory::new();
        let control = factory.insert(Control::default());
        thread::scope(|s| {
            let first = s.spawn(|| factory.build_for(SlowKey(1)));
            control.started.wait();
            let second = s.spawn(|| factory.build_for(SlowKey(1)));
            settle();
            assert!(factory.get_for(SlowKey(1)).is_none());
            control.release.open();
            let a = first.join().unwrap().unwrap();
            let b = second.join().unwrap().unwrap();
            assert!(Arc::ptr_eq(&a, &b));
        });
        assert_eq!(control.builds.load(Ordering::SeqCst), 1);
    });
}

// --- Different keys are not serialised --------------------------------------

/// Each key's build waits until the other key's build has started. If the
/// factory serialised builds across keys, this would deadlock.
#[derive(Default)]
struct Pair {
    started: [Latch; 2],
}

#[derive(Hash, PartialEq, Eq, Clone, Debug)]
struct PairKey(usize);

impl Query for PairKey {
    type Output = ();

    fn build(&self, factory: &DependencyFactoryHandle) -> Result<(), BuildError> {
        let pair = factory.get::<Pair>().unwrap().unwrap();
        pair.started[self.0].open();
        pair.started[1 - self.0].wait();
        Ok(())
    }
}

#[test]
fn builds_of_different_keys_proceed_in_parallel() {
    with_watchdog(|| {
        let factory = DependencyFactory::new();
        factory.insert(Pair::default());
        thread::scope(|s| {
            let zero = s.spawn(|| factory.build_for(PairKey(0)));
            let one = s.spawn(|| factory.build_for(PairKey(1)));
            zero.join().unwrap().unwrap();
            one.join().unwrap().unwrap();
        });
    });
}

// --- A failed build lets a waiter retry ------------------------------------

#[derive(Debug)]
struct FailsOnce;

impl Singleton for FailsOnce {
    fn build(factory: &DependencyFactoryHandle) -> Result<Self, BuildError> {
        let control = control(factory);
        if control.builds.fetch_add(1, Ordering::SeqCst) == 0 {
            control.started.open();
            control.release.wait();
            return Err(Boom.into());
        }
        Ok(FailsOnce)
    }
}

#[test]
fn waiter_retries_after_the_first_build_fails() {
    with_watchdog(|| {
        let factory = DependencyFactory::new();
        let control = factory.insert(Control::default());
        thread::scope(|s| {
            let first = s.spawn(|| factory.build::<FailsOnce>());
            control.started.wait();
            let second = s.spawn(|| factory.build::<FailsOnce>());
            settle();
            control.release.open();

            let err = first.join().unwrap().unwrap_err();
            assert!(err.source().downcast_ref::<Boom>().is_some());
            // The waiter is not handed the first thread's error. It
            // claims the slot and builds for itself.
            second.join().unwrap().unwrap();
        });
        assert_eq!(control.builds.load(Ordering::SeqCst), 2);
        // The failure left nothing behind: a later build succeeds.
        factory.build::<FailsOnce>().unwrap();
        assert_eq!(control.builds.load(Ordering::SeqCst), 2);
    });
}

// --- A panicking build does not strand waiters -----------------------------

#[derive(Debug)]
struct PanicsOnce;

impl Singleton for PanicsOnce {
    fn build(factory: &DependencyFactoryHandle) -> Result<Self, BuildError> {
        let control = control(factory);
        if control.builds.fetch_add(1, Ordering::SeqCst) == 0 {
            control.started.open();
            control.release.wait();
            panic!("build panicked on purpose");
        }
        Ok(PanicsOnce)
    }
}

#[test]
fn waiter_retries_after_the_first_build_panics() {
    with_watchdog(|| {
        let factory = DependencyFactory::new();
        let control = factory.insert(Control::default());
        thread::scope(|s| {
            let first = s.spawn(|| factory.build::<PanicsOnce>());
            control.started.wait();
            let second = s.spawn(|| factory.build::<PanicsOnce>());
            settle();
            control.release.open();

            assert!(
                first.join().is_err(),
                "the first thread should have panicked"
            );
            second.join().unwrap().unwrap();
        });
        assert_eq!(control.builds.load(Ordering::SeqCst), 2);
    });
}

// --- An insert during a build wins -----------------------------------------

#[test]
fn insert_during_a_build_takes_precedence() {
    with_watchdog(|| {
        let factory = DependencyFactory::new();
        let control = factory.insert(Control::default());
        thread::scope(|s| {
            let builder = s.spawn(|| factory.build::<Slow>());
            control.started.wait();
            let waiter = s.spawn(|| factory.build::<Slow>());
            settle();

            let inserted = factory.insert(Slow);
            // The waiter is released by the insert, before the build ends.
            let from_waiter = waiter.join().unwrap().unwrap();
            assert!(Arc::ptr_eq(&from_waiter, &inserted));

            control.release.open();
            let from_builder = builder.join().unwrap().unwrap();
            assert!(Arc::ptr_eq(&from_builder, &inserted));
        });
        assert_eq!(control.builds.load(Ordering::SeqCst), 1);
    });
}

// --- A cycle split across two threads is an error, not a deadlock ----------

/// `Left` needs `Right` and `Right` needs `Left`. Each build waits until
/// the other has started before it asks for its partner, so the two halves
/// of the cycle are guaranteed to be owned by different threads.
#[derive(Default)]
struct Halves {
    left_started: Latch,
    right_started: Latch,
}

#[derive(Debug)]
struct Left;
#[derive(Debug)]
struct Right;

impl Singleton for Left {
    fn build(factory: &DependencyFactoryHandle) -> Result<Self, BuildError> {
        let halves = factory.get::<Halves>().unwrap().unwrap();
        halves.left_started.open();
        halves.right_started.wait();
        factory.build::<Right>()?;
        Ok(Left)
    }
}

impl Singleton for Right {
    fn build(factory: &DependencyFactoryHandle) -> Result<Self, BuildError> {
        let halves = factory.get::<Halves>().unwrap().unwrap();
        halves.right_started.open();
        halves.left_started.wait();
        factory.build::<Left>()?;
        Ok(Right)
    }
}

#[test]
fn cross_thread_cycle_is_reported_on_both_sides() {
    with_watchdog(|| {
        let factory = DependencyFactory::new();
        factory.insert(Halves::default());
        thread::scope(|s| {
            let left = s.spawn(|| factory.build::<Left>());
            let right = s.spawn(|| factory.build::<Right>());
            // Whichever thread would have closed the ring gets a cycle
            // error from the factory's wait-chain check. The other wakes
            // when that build is abandoned, builds the missing half
            // itself, and runs into the same cycle on its own breadcrumb.
            let left_err = left.join().unwrap().map(drop).unwrap_err();
            let right_err = right.join().unwrap().map(drop).unwrap_err();
            for err in [left_err, right_err] {
                let path = cycle_of(&err).path();
                assert_eq!(path.len(), 3, "unexpected cycle path {path:?}");
                assert_eq!(path[0], path[2]);
                assert_ne!(path[0], path[1]);
            }
        });
        // The abandoned slots are empty again, so a non-cyclic build works.
        factory.insert(Right);
        factory.build::<Left>().unwrap();
    });
}
