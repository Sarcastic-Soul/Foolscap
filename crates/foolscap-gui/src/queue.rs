//! The batch queue: what is running and what is waiting.
//!
//! The worker is a single thread — MuPDF's context is thread-local and two
//! concurrent LibreOffice profiles still contend — so jobs run one at a time
//! in the order they were queued. This type only tracks labels and progress;
//! it never touches GTK or the filesystem, which keeps it unit-testable and
//! keeps IO on the worker thread.

/// A job's lifecycle, as shown in the queue panel.
#[derive(Debug, Clone, PartialEq)]
pub enum JobStatus {
    Queued,
    Running {
        fraction: Option<f32>,
    },
    Done(String),
    Failed(String),
    /// Dropped by the user before it started.
    Cancelled,
}

/// One queued operation.
#[derive(Debug, Clone)]
pub struct Job {
    pub id: u64,
    pub label: String,
    /// What the job works on, shown as the row's tooltip.
    pub detail: String,
    pub status: JobStatus,
}

impl Job {
    /// One line for the queue panel.
    pub fn summary(&self) -> String {
        match &self.status {
            JobStatus::Queued => format!("Queued — {}", self.label),
            JobStatus::Running { fraction } => match fraction {
                Some(done) => format!("Working — {} ({:.0}%)", self.label, done * 100.0),
                None => format!("Working — {}", self.label),
            },
            JobStatus::Done(message) => format!("Done — {}", message),
            JobStatus::Failed(error) => format!("Failed — {}: {error}", self.label),
            JobStatus::Cancelled => format!("Cancelled — {}", self.label),
        }
    }

    /// True once the job has stopped for good, whichever way it ended.
    pub fn is_finished(&self) -> bool {
        matches!(
            self.status,
            JobStatus::Done(_) | JobStatus::Failed(_) | JobStatus::Cancelled
        )
    }
}

/// Labels and progress for the batch queue, backed by a plain vector.
#[derive(Debug, Default)]
pub struct Queue {
    jobs: Vec<Job>,
    next_id: u64,
}

impl Queue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue a job, returning its id for the worker round-trip.
    pub fn push(&mut self, label: impl Into<String>, detail: impl Into<String>) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.jobs.push(Job {
            id,
            label: label.into(),
            detail: detail.into(),
            status: JobStatus::Queued,
        });
        id
    }

    pub fn get(&self, id: u64) -> Option<&Job> {
        self.jobs.iter().find(|job| job.id == id)
    }

    fn get_mut(&mut self, id: u64) -> Option<&mut Job> {
        self.jobs.iter_mut().find(|job| job.id == id)
    }

    pub fn mark_running(&mut self, id: u64) {
        if let Some(job) = self.get_mut(id) {
            job.status = JobStatus::Running { fraction: None };
        }
    }

    pub fn mark_progress(&mut self, id: u64, fraction: Option<f32>) {
        if let Some(job) = self.get_mut(id) {
            job.status = JobStatus::Running { fraction };
        }
    }

    pub fn mark_done(&mut self, id: u64, message: impl Into<String>) {
        if let Some(job) = self.get_mut(id) {
            job.status = JobStatus::Done(message.into());
        }
    }

    pub fn mark_failed(&mut self, id: u64, error: impl Into<String>) {
        if let Some(job) = self.get_mut(id) {
            job.status = JobStatus::Failed(error.into());
        }
    }

    /// Jobs still waiting for the worker, oldest first.
    pub fn queued(&self) -> Vec<u64> {
        self.jobs
            .iter()
            .filter(|job| job.status == JobStatus::Queued)
            .map(|job| job.id)
            .collect()
    }

    /// Drop one job that has not started. Returns false if it already has, or
    /// does not exist.
    pub fn cancel(&mut self, id: u64) -> bool {
        match self.get_mut(id) {
            Some(job) if job.status == JobStatus::Queued => {
                job.status = JobStatus::Cancelled;
                true
            }
            _ => false,
        }
    }

    /// Drop every job that has not started, returning their ids.
    pub fn cancel_waiting(&mut self) -> Vec<u64> {
        let waiting = self.queued();
        for id in &waiting {
            self.cancel(*id);
        }
        waiting
    }

    /// How many jobs are waiting behind the running one.
    pub fn queued_count(&self) -> usize {
        self.jobs
            .iter()
            .filter(|job| job.status == JobStatus::Queued)
            .count()
    }

    /// The running job, if any. There is at most one: the worker is serial.
    pub fn running(&self) -> Option<u64> {
        self.jobs.iter().find_map(|job| match job.status {
            JobStatus::Running { .. } => Some(job.id),
            _ => None,
        })
    }

    pub fn jobs(&self) -> &[Job] {
        &self.jobs
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }

    /// Drop finished, failed and cancelled jobs, keeping whatever is queued or
    /// running.
    pub fn clear_finished(&mut self) {
        self.jobs.retain(|job| !job.is_finished());
    }

    /// True when at least one job has stopped and could be cleared.
    pub fn has_finished(&self) -> bool {
        self.jobs.iter().any(Job::is_finished)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_monotonic() {
        let mut queue = Queue::new();
        let first = queue.push("compress", "a.pdf");
        let second = queue.push("ocr", "b.pdf");
        assert_ne!(first, second);
        assert!(second > first);
    }

    #[test]
    fn progress_tracks_one_running_job() {
        let mut queue = Queue::new();
        let id = queue.push("compress", "a.pdf");
        assert_eq!(queue.queued_count(), 1);
        assert_eq!(queue.running(), None);

        queue.mark_running(id);
        assert_eq!(queue.queued_count(), 0);
        assert_eq!(queue.running(), Some(id));

        queue.mark_progress(id, Some(0.5));
        assert!(queue.get(id).unwrap().summary().contains("50%"));

        queue.mark_done(id, "compressed");
        assert_eq!(queue.running(), None);
        assert_eq!(queue.queued_count(), 0);
    }

    #[test]
    fn failures_keep_their_message() {
        let mut queue = Queue::new();
        let id = queue.push("ocr", "a.pdf");
        queue.mark_running(id);
        queue.mark_failed(id, "tesseract missing");
        assert!(queue.get(id).unwrap().summary().contains("tesseract"));
    }

    #[test]
    fn clearing_keeps_what_is_still_pending() {
        let mut queue = Queue::new();
        let done = queue.push("a", "");
        let waiting = queue.push("b", "");
        queue.mark_running(done);
        queue.mark_done(done, "ok");
        queue.clear_finished();
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.jobs()[0].id, waiting);
    }

    #[test]
    fn unknown_ids_are_ignored() {
        let mut queue = Queue::new();
        queue.mark_running(99);
        queue.mark_done(99, "x");
        assert!(queue.is_empty());
    }

    #[test]
    fn cancelling_only_touches_jobs_that_have_not_started() {
        let mut queue = Queue::new();
        let running = queue.push("a", "");
        let waiting = queue.push("b", "");
        let later = queue.push("c", "");
        queue.mark_running(running);

        assert!(!queue.cancel(running));
        assert!(queue.cancel(waiting));
        assert!(!queue.cancel(waiting));
        assert_eq!(queue.cancel_waiting(), vec![later]);
        assert_eq!(queue.queued_count(), 0);
        assert_eq!(queue.running(), Some(running));
        assert!(queue
            .get(waiting)
            .unwrap()
            .summary()
            .starts_with("Cancelled"));
    }

    #[test]
    fn clearing_drops_cancelled_jobs_too() {
        let mut queue = Queue::new();
        let id = queue.push("a", "");
        queue.cancel(id);
        assert!(queue.has_finished());
        queue.clear_finished();
        assert!(queue.is_empty());
    }
}
