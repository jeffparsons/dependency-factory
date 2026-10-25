use std::error::Error;
use std::fmt;
use std::sync::Arc;

/// One resource on the way to an error: the type of the value that was
/// being built and, for a keyed resource, the key it was being built
/// for. [`BuildError::chain`] and [`CycleError::path`] are lists of
/// these. `Display` renders one as `Db for DbKey("users")`, or as just
/// `Config` for a singleton.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    type_name: &'static str,
    key: Option<String>,
}

impl Frame {
    pub(crate) fn new(type_name: &'static str, key: Option<String>) -> Self {
        Self { type_name, key }
    }

    /// The type of the value, as [`std::any::type_name`] spells it.
    pub fn type_name(&self) -> &'static str {
        self.type_name
    }

    /// The key the value was being built for, as its `Debug` impl
    /// renders it, or `None` for a singleton.
    pub fn key(&self) -> Option<&str> {
        self.key.as_deref()
    }
}

impl fmt::Display for Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.type_name)?;
        if let Some(key) = &self.key {
            write!(f, " for {key}")?;
        }
        Ok(())
    }
}

/// Error returned when a resource cannot be obtained from the factory.
///
/// Carries a chain of `while building <frame>` [`Frame`]s pushed by the
/// factory as it descends into transitive `build` calls, plus a boxed
/// source error. The chain is empty for errors that did not originate
/// inside a `build` call (for example, [`BuildError::factory_dropped`]).
///
/// `BuildError` deliberately does not implement [`std::error::Error`]: doing
/// so would conflict with the blanket `From<E: Error + Send + Sync +
/// 'static>` impl that lets `?` work cleanly inside `build` methods. Use
/// [`BuildError::source`] to obtain a `&dyn Error` view of the underlying
/// cause if you need one.
pub struct BuildError {
    chain: Vec<Frame>,
    source: Box<dyn Error + Send + Sync + 'static>,
}

impl BuildError {
    /// The owning [`crate::DependencyFactory`] has been dropped, so the
    /// handle cannot resolve resources.
    pub fn factory_dropped() -> Self {
        Self {
            chain: Vec::new(),
            source: Box::new(FactoryDropped),
        }
    }

    /// The frames recorded as the error propagated up the build stack.
    /// The first element is the innermost (originating) resource; the
    /// last is the outermost.
    pub fn chain(&self) -> &[Frame] {
        &self.chain
    }

    /// The underlying error that caused the build to fail.
    pub fn source(&self) -> &(dyn Error + Send + Sync + 'static) {
        &*self.source
    }

    pub(crate) fn push_frame(mut self, frame: Frame) -> Self {
        self.chain.push(frame);
        self
    }
}

impl fmt::Debug for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BuildError")
            .field("chain", &self.chain)
            .field("source", &self.source)
            .finish()
    }
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for frame in self.chain.iter().rev() {
            writeln!(f, "while building {frame}")?;
        }
        write!(f, "caused by: {}", self.source)
    }
}

impl<E> From<E> for BuildError
where
    E: Error + Send + Sync + 'static,
{
    fn from(source: E) -> Self {
        Self {
            chain: Vec::new(),
            source: Box::new(source),
        }
    }
}

#[derive(Debug)]
struct FactoryDropped;

impl fmt::Display for FactoryDropped {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("the DependencyFactory has been dropped")
    }
}

impl Error for FactoryDropped {}

/// A dependency cycle detected during recursive resolution. Carried as
/// the source of a [`BuildError`] when a cycle is the root cause; users
/// can recognise it via `err.source().downcast_ref::<CycleError>()`.
#[derive(Debug)]
pub struct CycleError {
    path: Vec<Frame>,
}

impl CycleError {
    pub(crate) fn new(path: Vec<Frame>) -> Self {
        Self { path }
    }

    /// The frames along the detected cycle, starting at the resource
    /// where the cycle begins and ending with the same resource where
    /// the cycle closes.
    pub fn path(&self) -> &[Frame] {
        &self.path
    }
}

impl fmt::Display for CycleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("dependency cycle detected: ")?;
        for (i, frame) in self.path.iter().enumerate() {
            if i > 0 {
                f.write_str(" -> ")?;
            }
            write!(f, "{frame}")?;
        }
        Ok(())
    }
}

impl Error for CycleError {}

/// Returned by [`Transaction::insert`](crate::Transaction::insert) and
/// [`Transaction::insert_for`](crate::Transaction::insert_for) when the
/// slot is already filled. Carries the rejected `Arc` so the caller can
/// recover it or fall back to
/// [`Transaction::replace`](crate::Transaction::replace).
///
/// Implements [`Debug`](fmt::Debug) and [`Error`] without requiring the
/// value to, so `unwrap` works for any registry value, and `?` converts it
/// into a [`BuildError`] inside `build` methods.
pub struct OccupiedError<V: ?Sized> {
    value: Arc<V>,
}

impl<V: ?Sized> OccupiedError<V> {
    pub(crate) fn new(value: Arc<V>) -> Self {
        Self { value }
    }

    /// The value that was not inserted.
    pub fn value(&self) -> &Arc<V> {
        &self.value
    }

    /// Recover the value that was not inserted.
    pub fn into_value(self) -> Arc<V> {
        self.value
    }
}

impl<V: ?Sized> fmt::Debug for OccupiedError<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OccupiedError")
            .field("value_type", &std::any::type_name::<V>())
            .finish()
    }
}

impl<V: ?Sized> fmt::Display for OccupiedError<V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the registry slot for this key is already occupied (value type {})",
            std::any::type_name::<V>()
        )
    }
}

impl<V: ?Sized> Error for OccupiedError<V> {}
