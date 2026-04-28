use std::error::Error;
use std::fmt;

/// Error returned when a resource cannot be obtained from the factory.
///
/// Carries a chain of `while building <type>` frames pushed by the factory
/// as it descends into transitive `build` calls, plus a boxed source error.
/// The chain is empty for errors that did not originate inside a `build`
/// call (for example, [`BuildError::factory_dropped`]).
///
/// `BuildError` deliberately does not implement [`std::error::Error`]: doing
/// so would conflict with the blanket `From<E: Error + Send + Sync +
/// 'static>` impl that lets `?` work cleanly inside `build` methods. Use
/// [`BuildError::source`] to obtain a `&dyn Error` view of the underlying
/// cause if you need one.
pub struct BuildError {
    chain: Vec<&'static str>,
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

    /// The chain of resource type names recorded as the error propagated
    /// up the build stack. The first element is the innermost (originating)
    /// resource; the last is the outermost.
    pub fn chain(&self) -> &[&'static str] {
        &self.chain
    }

    /// The underlying error that caused the build to fail.
    pub fn source(&self) -> &(dyn Error + Send + Sync + 'static) {
        &*self.source
    }

    pub(crate) fn push_frame(mut self, frame: &'static str) -> Self {
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
    path: Vec<&'static str>,
}

impl CycleError {
    pub(crate) fn new(path: Vec<&'static str>) -> Self {
        Self { path }
    }

    /// The resource type names along the detected cycle, starting at the
    /// resource where the cycle begins and ending with the same resource
    /// where the cycle closes.
    pub fn path(&self) -> &[&'static str] {
        &self.path
    }
}

impl fmt::Display for CycleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("dependency cycle detected: ")?;
        for (i, name) in self.path.iter().enumerate() {
            if i > 0 {
                f.write_str(" -> ")?;
            }
            f.write_str(name)?;
        }
        Ok(())
    }
}

impl Error for CycleError {}
