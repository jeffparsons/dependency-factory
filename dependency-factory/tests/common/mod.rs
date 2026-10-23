use std::error::Error;
use std::iter::successors;

/// `err` and every error in its `source` chain, one per line, the way an
/// error reporter such as anyhow's shows them.
pub fn report(err: &(dyn Error + 'static)) -> String {
    successors(Some(err), |&err| err.source())
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n")
}
