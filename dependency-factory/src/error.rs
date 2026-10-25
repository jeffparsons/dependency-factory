use std::error::Error;
use std::fmt;
use std::sync::Arc;

/// One resource on the way to an error: the type of the value that was
/// being built and, for a keyed resource, the key it was being built
/// for. [`BuildError::chain`], [`ResolveError::chain`] and
/// [`CycleError::path`] are lists of these. `Display` renders one as
/// `Db for DbKey("users")`, or as just `Config` for a singleton.
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

/// The error a [`Singleton::build`](crate::Singleton::build),
/// [`Query::build`](crate::Query::build) or `register` returns, and what
/// the [`Builder`](crate::Builder) methods that build return.
///
/// Carries a chain of `while building <frame>` [`Frame`]s pushed by the
/// factory as it descends into transitive `build` calls, plus a boxed
/// source error. The chain is empty for an error that has not yet passed
/// through a `build`, such as one just converted with `?` inside one.
///
/// `?` converts any standard error that is `Send + Sync + 'static` into
/// a `BuildError`. For anything else that converts into a
/// `Box<dyn Error + Send + Sync>`, such as an `anyhow::Error` or a
/// message string, use [`BuildError::new`].
///
/// `BuildError` deliberately does not implement [`std::error::Error`]:
/// doing so would conflict with the blanket `From` impl that lets `?`
/// work cleanly inside `build` methods. Outside a `build`,
/// [`DependencyFactory`](crate::DependencyFactory) returns a
/// [`ResolveError`] instead, which does implement it. A `BuildError` from
/// a [`Builder`](crate::Builder) used outside a `build` converts into one
/// with [`ResolveError::from`].
pub struct BuildError {
    chain: Vec<Frame>,
    cause: Box<dyn Error + Send + Sync + 'static>,
}

impl BuildError {
    /// A build error caused by `cause`, with an empty chain. Converting a
    /// [`ResolveError`] keeps its chain and cause rather than nesting it.
    ///
    /// ```
    /// # use dependency_factory::BuildError;
    /// let err = BuildError::new("FOO is not set");
    /// assert_eq!(err.underlying().to_string(), "FOO is not set");
    /// ```
    pub fn new(cause: impl Into<Box<dyn Error + Send + Sync + 'static>>) -> Self {
        match cause.into().downcast::<ResolveError>() {
            Ok(resolve) => resolve.into_build_error(),
            Err(cause) => Self {
                chain: Vec::new(),
                cause,
            },
        }
    }

    /// The frames recorded as the error propagated up the build stack.
    /// The first element is the innermost (originating) resource; the
    /// last is the outermost.
    pub fn chain(&self) -> &[Frame] {
        &self.chain
    }

    /// The underlying error that caused the build to fail.
    pub fn underlying(&self) -> &(dyn Error + Send + Sync + 'static) {
        &*self.cause
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
            .field("cause", &self.cause)
            .finish()
    }
}

impl fmt::Display for BuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for frame in self.chain.iter().rev() {
            writeln!(f, "while building {frame}")?;
        }
        write!(f, "caused by: {}", self.cause)
    }
}

impl<E> From<E> for BuildError
where
    E: Error + Send + Sync + 'static,
{
    fn from(cause: E) -> Self {
        Self::new(cause)
    }
}

/// Lets `?` turn a [`BuildError`] into a `Box<dyn Error + Send + Sync>`,
/// by way of a [`ResolveError`].
impl From<BuildError> for Box<dyn Error + Send + Sync + 'static> {
    fn from(err: BuildError) -> Self {
        Box::new(ResolveError::from(err))
    }
}

/// Error returned when a resource cannot be obtained from a
/// [`DependencyFactory`](crate::DependencyFactory).
///
/// The same chain of [`Frame`]s and cause as the [`BuildError`] it came
/// from, but as a standard [`Error`], so `?` converts it into an
/// `anyhow::Error`, a `Box<dyn Error>` or an error enum with a `#[from]`
/// variant. Each frame is one link in the chain this error starts, and
/// that [`source`](Error::source) continues, outermost first; the cause
/// is the last link:
///
/// ```text
/// failed to build app::App
/// failed to build app::Db for DbKey("users")
/// connection refused
/// ```
///
/// So `Display` shows only the outermost frame; render the whole chain
/// with your error reporter (anyhow's `{:#}` or `{:?}`, for example), and
/// reach the cause with [`underlying`](Self::underlying). The factory
/// never returns one with an empty chain; one converted from a
/// [`BuildError`] that has none is transparent, with the cause's own
/// `Display` and `source`.
pub struct ResolveError {
    chain: Vec<Frame>,
    /// The next link: a [`FrameLink`] for the next frame in, or the cause
    /// itself.
    next: Box<dyn Error + Send + Sync + 'static>,
}

impl ResolveError {
    /// The frames recorded as the error propagated up the build stack.
    /// The first element is the innermost (originating) resource; the
    /// last is the outermost.
    pub fn chain(&self) -> &[Frame] {
        &self.chain
    }

    /// The underlying error that caused the build to fail.
    pub fn underlying(&self) -> &(dyn Error + Send + Sync + 'static) {
        let mut cause = &*self.next;
        while let Some(link) = cause.downcast_ref::<FrameLink>() {
            cause = &*link.next;
        }
        cause
    }

    fn into_build_error(self) -> BuildError {
        let mut cause = self.next;
        loop {
            match cause.downcast::<FrameLink>() {
                Ok(link) => cause = link.next,
                Err(unlinked) => {
                    break BuildError {
                        chain: self.chain,
                        cause: unlinked,
                    };
                }
            }
        }
    }
}

impl From<BuildError> for ResolveError {
    fn from(err: BuildError) -> Self {
        let BuildError { chain, cause } = err;
        // Every frame but the outermost becomes a link, innermost
        // wrapped first; the outermost is this error's own `Display`.
        let inner = chain.len().saturating_sub(1);
        let next = chain[..inner].iter().fold(cause, |next, frame| {
            Box::new(FrameLink {
                frame: frame.clone(),
                next,
            })
        });
        Self { chain, next }
    }
}

impl fmt::Debug for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ResolveError")
            .field("chain", &self.chain)
            .field("cause", &self.underlying())
            .finish()
    }
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.chain.last() {
            Some(frame) => write!(f, "failed to build {frame}"),
            None => fmt::Display::fmt(&self.next, f),
        }
    }
}

impl Error for ResolveError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        if self.chain.is_empty() {
            self.next.source()
        } else {
            Some(&*self.next)
        }
    }
}

/// One inner frame of a [`ResolveError`], as a link in its `source`
/// chain.
#[derive(Debug)]
struct FrameLink {
    frame: Frame,
    next: Box<dyn Error + Send + Sync + 'static>,
}

impl fmt::Display for FrameLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "failed to build {}", self.frame)
    }
}

impl Error for FrameLink {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&*self.next)
    }
}

/// A dependency cycle detected during recursive resolution. Carried as
/// the cause of a [`BuildError`] or [`ResolveError`] when a cycle is the
/// root cause; recognise it with `err.underlying().downcast_ref::<CycleError>()`.
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
