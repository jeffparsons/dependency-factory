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
- **Queries**: many instances per type, parameterised by a key value.
  Implement `Query` to define the key type and how the resource is built
  from it.

Cycles in the dependency graph are detected at runtime and surfaced as
structured errors with the full path through the cycle.

A factory has two phases. While assembling, every `build` and `insert`
takes `&mut DependencyFactory`, so each resource is built at most once and
there is no locking. `freeze` then turns the factory into a `FrozenFactory`
that is `Send + Sync` and read-only. Resources that need to look things up
later keep a `DependencyFactoryHandle`, a cheap, weak handle that starts
resolving once the factory is frozen.

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
use dependency_factory::{BuildError, DependencyFactory, Singleton};

struct Config {
    greeting: String,
}

impl Singleton for Config {
    fn build(_: &mut DependencyFactory) -> Result<Self, BuildError> {
        Ok(Config { greeting: "hello".into() })
    }
}

#[derive(Singleton)]
struct Greeter {
    config: Arc<Config>,
}

let mut factory = DependencyFactory::new();
let greeter: Arc<Greeter> = factory.build().unwrap();
assert_eq!(greeter.config.greeting, "hello");

// Done assembling: share the frozen factory with the rest of the program.
let frozen = factory.freeze();
let handle = frozen.handle();
assert!(Arc::ptr_eq(&handle.get::<Greeter>().unwrap().unwrap(), &greeter));
```

## License

Dual-licensed under either of:

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT license](LICENSE-MIT)

at your option.
