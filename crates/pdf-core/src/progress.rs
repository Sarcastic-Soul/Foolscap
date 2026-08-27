/// A progress report emitted during a long-running operation.
///
/// Operations never render progress themselves; they hand these to a caller
/// supplied callback. The CLI prints them, the GTK front end forwards them to
/// the main loop over a channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    /// Units completed so far.
    pub current: usize,
    /// Total units, if known ahead of time.
    pub total: Option<usize>,
    /// Human-readable description of the current step.
    pub message: String,
}

impl Progress {
    pub fn new(current: usize, total: Option<usize>, message: impl Into<String>) -> Self {
        Self {
            current,
            total,
            message: message.into(),
        }
    }

    /// Completion in the range `0.0..=1.0`, or `None` when the total is unknown.
    pub fn fraction(&self) -> Option<f32> {
        match self.total {
            Some(0) | None => None,
            Some(total) => Some((self.current as f32 / total as f32).clamp(0.0, 1.0)),
        }
    }
}

/// Callback type accepted by operations that can report progress.
pub type ProgressFn<'a> = &'a mut dyn FnMut(Progress);
