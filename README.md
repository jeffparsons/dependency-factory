# dependency-factory

[![Crate](https://img.shields.io/crates/v/dependency-factory.svg)](https://crates.io/crates/dependency-factory)
[![Docs](https://docs.rs/dependency-factory/badge.svg)](https://docs.rs/dependency-factory)
[![Build status](https://github.com/jeffparsons/dependency-factory/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/jeffparsons/dependency-factory/actions/workflows/ci.yml)
[![Rust](https://img.shields.io/badge/rust-1.91%2B-blue.svg)](https://github.com/jeffparsons/dependency-factory)

A type-safe dependency-injection container for Rust. Resources are
auto-built on first request, cached as `Arc<T>`, and resolved recursively
from their declared dependencies.

Two kinds of resources are supported:

- **Singletons**: one instance per type. Implement `Singleton`, or
  `#[derive(Singleton)]` and let the macro write the dependency wiring for you.
- **Keyed resources**: many instances per type, parameterised by a key
  value. Implement `Key` to pair a key type with its value type, and
  `Query` on top of it to say how the value is built from the key.

An error names every resource that was being built when it happened,
and a keyed resource by its key, through the key's `Debug` impl. Cycles
in the dependency graph are detected at runtime and surfaced as
structured errors with the full path through the cycle.

Inside `build`, errors are `BuildError`s, which `?` makes from any
standard error. The factory and its handles return a `ResolveError`
instead, which is a standard error itself, so `?` carries it on into an
`anyhow::Error`, a `Box<dyn Error>` or your own error type, with each
resource that was being built as its own link in the `source` chain.

Resources live in a `Registry`, a thread-safe type-keyed map that knows
nothing about building. The factory is one client of its registry: it
reads it to short-circuit and commits what it builds with
`get_or_insert_with`, so a value already in the registry always wins.
Anything else can populate the same registry by hand through
`factory.registry()`.

Every write goes through a transaction: `registry.begin()` returns a
`Transaction` carrying `insert` (fails if the slot is taken), `replace`
(overwrites) and `get_or_insert_with`. Its writes are published as one
batch when it commits, which dropping it does, so
`registry.begin().insert(value)` is a single write and a held
transaction is a batch; `commit()` says so explicitly, and `discard()`
throws the batch away. Only one transaction is open at a time. Reads
live on the registry and never wait for one, not even a build in
progress; a committing transaction holds readers off only for the
moment it takes to swap its batch in.

Building goes through a `Builder`, which `Singleton::build` and
`Query::build` receive and pass down to resolve their own dependencies.
A builder is a registry transaction plus the record of builds in
progress, so each resource is built exactly once even when several
threads ask for it, and every dependency cycle is seen by one builder
and reported. `get_or_build` takes a builder for the duration of the
call; `factory.builder()` hands one out to hold across several. Lookups
that never build (`get`, `get_for`) take no builder and never wait.

Resources that take part in a lifecycle event the application runs, such
as warming up or shutting down, make themselves findable through an
*index*: the registry's list of `Arc<dyn Trait>` for any trait you like.
A resource adds itself in `Singleton::register` (or `Query::register`),
a hook that runs right after the built value is cached, with the
builder still open. That is where `Arc<Resource>` becomes
`Arc<dyn Trait>`, so the registry never needs to know which traits a
value implements. `#[derive(Singleton)]` writes the hook from a
`#[factory(index(dyn Trait, ...))]` attribute. Whoever runs the event
asks for `registry.indexed::<dyn Trait>()`. The index is published in
the same batch as the value, a resource always follows its dependencies
in it, and a replaced value leaves its indexes. A value inserted by hand
is not indexed unless the caller adds it with `Transaction::index`. The
crate holds no graph between resources and runs no hooks itself; what a
lifecycle event is, and when it runs, is the application's business.

## Usage

```toml
[dependencies]
dependency-factory = "0.1"
```

The `derive` feature is on by default and re-exports
`#[derive(Singleton)]`. To opt out, depend with
`default-features = false`.

## Example

```rust
use std::sync::Arc;
use dependency_factory::{BuildError, Builder, DependencyFactory, Singleton};

struct Config {
    greeting: String,
}

impl Singleton for Config {
    fn build(_: &mut Builder) -> Result<Self, BuildError> {
        Ok(Config { greeting: "hello".into() })
    }
}

#[derive(Singleton)]
struct Greeter {
    config: Arc<Config>,
}

let factory = DependencyFactory::new();
let greeter: Arc<Greeter> = factory.get_or_build().unwrap();
assert_eq!(greeter.config.greeting, "hello");

// A value placed in the registry by hand is used instead of building one.
let factory = DependencyFactory::new();
factory.registry().begin().insert(Config { greeting: "g'day".into() }).unwrap();
let greeter: Arc<Greeter> = factory.get_or_build().unwrap();
assert_eq!(greeter.config.greeting, "g'day");
```

A lifecycle event, run by the application over an index:

```rust
use std::sync::Arc;
use dependency_factory::{BuildError, Builder, DependencyFactory, Singleton};

trait WarmUp: Send + Sync {
    fn warm_up(&self);
}

struct Cache;

impl WarmUp for Cache {
    fn warm_up(&self) { /* fill the cache */ }
}

impl Singleton for Cache {
    fn build(_: &mut Builder) -> Result<Self, BuildError> {
        Ok(Cache)
    }

    fn register(this: &Arc<Self>, builder: &mut Builder) -> Result<(), BuildError> {
        builder.index::<dyn WarmUp>(this.clone());
        Ok(())
    }
}

#[derive(Singleton)]
#[factory(index(dyn WarmUp))]
struct Service {
    cache: Arc<Cache>,
}

impl WarmUp for Service {
    fn warm_up(&self) { /* prime the service */ }
}

let factory = DependencyFactory::new();
factory.get_or_build::<Service>().unwrap();
// Dependencies first: Cache, then Service.
for resource in factory.indexed::<dyn WarmUp>() {
    resource.warm_up();
}
```

## License

Dual-licensed under either of:

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT license](LICENSE-MIT)

at your option.
