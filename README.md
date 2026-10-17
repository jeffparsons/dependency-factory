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

Cycles in the dependency graph are detected at runtime and surfaced as
structured errors with the full path through the cycle.

Resources live in a `Registry`, a thread-safe type-keyed map that knows
nothing about building. The factory is one client of its registry: it
reads it to short-circuit and commits what it builds with
`get_or_insert_with`, so a value already in the registry always wins.
Anything else can populate the same registry by hand through
`factory.registry()`, with `insert` (fails if the slot is taken) or
`replace` (overwrites).

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
use dependency_factory::{
    BuildError, DependencyFactory, DependencyFactoryHandle, Singleton,
};

struct Config {
    greeting: String,
}

impl Singleton for Config {
    fn build(_: &DependencyFactoryHandle) -> Result<Self, BuildError> {
        Ok(Config { greeting: "hello".into() })
    }
}

#[derive(Singleton)]
struct Greeter {
    config: Arc<Config>,
}

let factory = DependencyFactory::new();
let greeter: Arc<Greeter> = factory.build().unwrap();
assert_eq!(greeter.config.greeting, "hello");

// A value placed in the registry by hand is used instead of building one.
let factory = DependencyFactory::new();
factory.registry().insert(Config { greeting: "g'day".into() }).unwrap();
let greeter: Arc<Greeter> = factory.build().unwrap();
assert_eq!(greeter.config.greeting, "g'day");
```

## License

Dual-licensed under either of:

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT license](LICENSE-MIT)

at your option.
