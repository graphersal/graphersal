//! The channel and the "technical" mark of what the current thread runs: a front end opens a
//! scope ([`scope`]) around the work it does for a request, and every sample recorded on the
//! thread meanwhile (an execution, a lock acquisition) is counted under that channel, or not at
//! all for technical work (queries the front end made up itself: schema inference, drawing,
//! panels, the advisor's reads).

use std::cell::{Cell, RefCell};
use std::sync::Arc;

use super::{Channel, WorkloadStats};

/// What the current thread runs: its channel and whether it is technical.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Origin {
    pub(crate) channel: Channel,
    pub(crate) technical: bool,
}

thread_local! {
    static CURRENT: Cell<Option<Origin>> = const { Cell::new(None) };
    /// The origin outside every scope ([`set_thread_default`]).
    static DEFAULT: Cell<Option<Origin>> = const { Cell::new(None) };
    /// The statistics of the graph whose lock guard this thread holds (the innermost one).
    static ATTACHED: RefCell<Option<Arc<WorkloadStats>>> = const { RefCell::new(None) };
}

/// While alive, makes `stats` the statistics executions on this thread record into (a lock
/// guard of a graph with statistics holds one); dropping restores the previous ones.
pub(crate) struct Attached(Option<Arc<WorkloadStats>>);

impl Attached {
    /// Attaches `stats` to the current thread.
    pub(crate) fn new(stats: &Arc<WorkloadStats>) -> Self {
        let previous = ATTACHED
            .try_with(|current| {
                current
                    .try_borrow_mut()
                    .ok()
                    .and_then(|mut current| current.replace(Arc::clone(stats)))
            })
            .ok()
            .flatten();
        Attached(previous)
    }
}

impl Drop for Attached {
    fn drop(&mut self) {
        let previous = self.0.take();
        let _ = ATTACHED.try_with(|current| {
            if let Ok(mut current) = current.try_borrow_mut() {
                *current = previous;
            }
        });
    }
}

/// The statistics executions on this thread record into: those of the graph whose lock guard
/// the thread holds, when they are on and the work is not technical.
pub(crate) fn attached() -> Option<(Arc<WorkloadStats>, Origin)> {
    let origin = current();
    if origin.technical {
        return None;
    }
    let stats = ATTACHED
        .try_with(|current| current.try_borrow().ok().and_then(|c| c.clone()))
        .ok()
        .flatten()?;
    (stats.level() != super::WorkloadLevel::Off).then_some((stats, origin))
}

/// Runs `run` counting its samples under `channel` (`technical`: not counted at all). Scopes
/// nest; the innermost applies and the previous one is restored afterwards (also on a panic).
pub fn scope<R>(channel: Channel, technical: bool, run: impl FnOnce() -> R) -> R {
    struct Restore(Option<Origin>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let previous = self.0.take();
            let _ = CURRENT.try_with(|current| current.set(previous));
        }
    }
    let origin = Origin { channel, technical };
    let previous = CURRENT
        .try_with(|current| current.replace(Some(origin)))
        .ok()
        .flatten();
    let _restore = Restore(previous);
    run()
}

/// The origin of the current thread's work: the innermost scope's, else the thread's default
/// ([`set_thread_default`]), else [`Channel::Host`], not technical.
pub(crate) fn current() -> Origin {
    CURRENT
        .try_with(Cell::get)
        .ok()
        .flatten()
        .or_else(|| DEFAULT.try_with(Cell::get).ok().flatten())
        .unwrap_or_default()
}

/// Sets what the current thread's work counts as outside every [`scope`]: a thread that only
/// serves one front end (the playground's worker, the CLI's script thread, a Python script
/// worker) declares it once instead of around every call.
pub fn set_thread_default(channel: Channel, technical: bool) {
    let _ = DEFAULT.try_with(|default| default.set(Some(Origin { channel, technical })));
}

/// The channel the current thread's work counts under. A front end that marks its own work as
/// technical runs a user's query in `scope(current_channel(), false, ..)`: same channel, counted.
pub fn current_channel() -> Channel {
    current().channel
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_nest_and_restore() {
        assert_eq!(current(), Origin::default());
        scope(Channel::Mcp, false, || {
            assert_eq!(current().channel, Channel::Mcp);
            scope(Channel::Playground, true, || {
                assert_eq!(
                    current(),
                    Origin {
                        channel: Channel::Playground,
                        technical: true
                    }
                );
            });
            assert_eq!(current().channel, Channel::Mcp);
        });
        assert_eq!(current(), Origin::default());
    }

    #[test]
    fn a_thread_default_applies_outside_scopes() {
        let thread = std::thread::spawn(|| {
            set_thread_default(Channel::Python, true);
            assert_eq!(current_channel(), Channel::Python);
            assert!(current().technical);
            scope(current_channel(), false, || {
                assert_eq!(
                    current(),
                    Origin {
                        channel: Channel::Python,
                        technical: false
                    }
                );
            });
            assert!(current().technical);
        });
        assert!(thread.join().is_ok());
    }
}
