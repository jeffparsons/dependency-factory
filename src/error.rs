use std::error::Error;
use std::fmt;

/// Error returned when a resource cannot be obtained from the factory.
///
/// Carries a chain of `while building <type>` frames pushed by the factory
/// as it descends into transitive `build` calls, plus an underlying source
/// error. The chain is empty for errors that did not originate inside a
/// `build` call (for example, [`BuildError::factory_dropped`]).
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
        for frame in &self.chain {
            writeln!(f, "while building {frame}")?;
        }
        write!(f, "caused by: {}", self.source)
    }
}

impl Error for BuildError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&*self.source)
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
