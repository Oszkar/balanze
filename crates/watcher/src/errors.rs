use state_coordinator::Source;

/// Errors that a watcher task can return via its `JoinHandle`.
///
/// Task 5b will add `TaskPanicked { affected, message }` for the supervisor
/// restart logic — do not pre-add it here.
#[derive(Debug, thiserror::Error)]
pub enum WatcherError {
    /// Kernel inotify (Linux) / Win32 directory-change handle (Windows) /
    /// FSEvents (macOS) exhausted. The affected task should fall back to
    /// polling — in 5a this is just reported via the JoinHandle; 5b's
    /// supervisor decides the restart-or-poll-fallback policy.
    ///
    /// The field is named `affected` (not `source`) to avoid `thiserror`'s
    /// special handling of a field named `source`, which requires the field
    /// type to implement `std::error::Error`. `state_coordinator::Source` is
    /// a plain enum tag, not an error type.
    #[error("notify watcher exhausted for {affected:?}; supervisor should fall back to polling")]
    NotifyExhausted { affected: Source },

    /// I/O surfaced from the underlying notify subscription or the JSONL
    /// re-walk.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}
