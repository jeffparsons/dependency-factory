# dependency-factory

A type-safe dependency-injection container for Rust. Resources are
auto-built on first request, cached as `Arc<T>`, and resolved recursively
from their declared dependencies.

Two kinds of resources are supported:

- **Singletons**: one instance per type. Implement `Singleton`, or
  `#[derive(Singleton)]` and let the macro write the dependency wiring for you.
- **Queries**: many instances per type, parameterised by a key value.
  Implement `Query` to define the key type and how the resource is built
  from it.

A resource is built at most once per key. If several threads ask for the
same key at once, one builds it and the others wait for the result.

Cycles in the dependency graph are detected at runtime and surfaced as
structured errors with the full path through the cycle. Detection is
best-effort: it is reliable for a graph resolved on one thread, which is
the normal shape at startup, and it also catches cycles whose halves are
being built on different threads, as long as every edge is a `build`
call. A `build` that blocks on a lock, channel or thread which in turn
needs the key being built is a cycle the factory cannot see, and it will
deadlock. A sufficiently motivated caller can always manufacture one.

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
```

## License

Dual-licensed under either of:

- [Apache License, Version 2.0](LICENSE-APACHE)
- [MIT license](LICENSE-MIT)

at your option.
