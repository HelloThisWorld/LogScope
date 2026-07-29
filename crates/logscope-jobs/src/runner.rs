//! Job execution on worker threads with progress events and panic isolation.

use std::panic::AssertUnwindSafe;
use std::thread::JoinHandle;

use chrono::Utc;
use crossbeam_channel::Sender;
use serde::{Deserialize, Serialize};

use crate::control::JobControl;
use crate::error::JobError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Pending,
    Running,
    Paused,
    Cancelled,
    Failed,
    Completed,
}

/// Progress counters common to all LogScope jobs. Fields that do not apply
/// to a job kind stay zero.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct JobProgress {
    pub stage: String,
    /// The source/file currently being processed, for display.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_item: Option<String>,
    pub records_accepted: u64,
    pub records_rejected: u64,
    pub records_unparsed: u64,
    pub records_duplicate: u64,
    pub bytes_processed: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes_total: Option<u64>,
}

/// Lifecycle and progress events emitted by a running job.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum JobEvent {
    Started {
        job_id: String,
        kind: String,
        at_unix_ms: i64,
    },
    Progress {
        job_id: String,
        progress: JobProgress,
    },
    Finished {
        job_id: String,
        status: JobStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<JobError>,
        at_unix_ms: i64,
    },
}

/// Everything a job body needs: identity, control, and progress reporting.
pub struct JobContext {
    pub job_id: String,
    pub control: JobControl,
    events: Sender<JobEvent>,
}

impl JobContext {
    /// Foreground context for running a job body inline (synchronous
    /// maintenance work, tests). Progress events go to the returned
    /// receiver; cancellation goes through the returned control.
    pub fn detached(
        job_id: impl Into<String>,
    ) -> (
        JobContext,
        JobControl,
        crossbeam_channel::Receiver<JobEvent>,
    ) {
        let (tx, rx) = crossbeam_channel::unbounded();
        let control = JobControl::new();
        (
            JobContext {
                job_id: job_id.into(),
                control: control.clone(),
                events: tx,
            },
            control,
            rx,
        )
    }

    pub fn report(&self, progress: JobProgress) {
        // Progress delivery is best effort: a full/disconnected listener
        // must never stall or fail the job itself.
        let _ = self.events.try_send(JobEvent::Progress {
            job_id: self.job_id.clone(),
            progress,
        });
    }
}

/// A spawned job. Join to obtain the outcome; control to pause/cancel.
pub struct JobHandle<T> {
    pub job_id: String,
    pub kind: String,
    pub control: JobControl,
    join: JoinHandle<Result<T, JobError>>,
}

impl<T> JobHandle<T> {
    /// Waits for completion and returns the job body's result. A panic in
    /// the body surfaces as a structured `job/panic` error, not a poison.
    pub fn join(self) -> Result<T, JobError> {
        match self.join.join() {
            Ok(result) => result,
            Err(_) => Err(JobError::new(
                "job/panic",
                "job thread panicked; see application log for details",
            )),
        }
    }

    pub fn is_finished(&self) -> bool {
        self.join.is_finished()
    }
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// Spawns `work` on a dedicated thread with panic isolation and lifecycle
/// events. The worker decides its own safe points via `ctx.control`.
pub fn spawn_job<T, F>(
    job_id: impl Into<String>,
    kind: impl Into<String>,
    events: Sender<JobEvent>,
    work: F,
) -> JobHandle<T>
where
    T: Send + 'static,
    F: FnOnce(&JobContext) -> Result<T, JobError> + Send + 'static,
{
    let job_id = job_id.into();
    let kind = kind.into();
    let control = JobControl::new();

    let ctx = JobContext {
        job_id: job_id.clone(),
        control: control.clone(),
        events: events.clone(),
    };
    let thread_job_id = job_id.clone();
    let thread_kind = kind.clone();

    let join = std::thread::Builder::new()
        .name(format!("job-{job_id}"))
        .spawn(move || {
            let _ = events.try_send(JobEvent::Started {
                job_id: thread_job_id.clone(),
                kind: thread_kind,
                at_unix_ms: now_ms(),
            });

            let outcome = std::panic::catch_unwind(AssertUnwindSafe(|| work(&ctx)));

            let result: Result<T, JobError> = match outcome {
                Ok(r) => r,
                Err(panic) => {
                    let msg = panic
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| panic.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "unknown panic payload".to_string());
                    tracing::error!(job_id = %thread_job_id, panic = %msg, "job panicked");
                    Err(JobError::new("job/panic", format!("job panicked: {msg}")))
                }
            };

            let (status, error) = match &result {
                Ok(_) => (JobStatus::Completed, None),
                Err(e) if e.code == "job/cancelled" => (JobStatus::Cancelled, Some(e.clone())),
                Err(e) => (JobStatus::Failed, Some(e.clone())),
            };
            let _ = events.try_send(JobEvent::Finished {
                job_id: thread_job_id,
                status,
                error,
                at_unix_ms: now_ms(),
            });
            result
        })
        .expect("spawning a job thread cannot fail under normal conditions");

    JobHandle {
        job_id,
        kind,
        control,
        join,
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn unbounded() -> (Sender<JobEvent>, crossbeam_channel::Receiver<JobEvent>) {
        crossbeam_channel::unbounded()
    }

    #[test]
    fn completes_and_reports_lifecycle() {
        let (tx, rx) = unbounded();
        let handle = spawn_job("j1", "test", tx, |ctx| {
            ctx.report(JobProgress {
                stage: "work".into(),
                records_accepted: 10,
                ..Default::default()
            });
            Ok::<_, JobError>(42)
        });
        assert_eq!(handle.join().unwrap(), 42);
        let events: Vec<JobEvent> = rx.try_iter().collect();
        assert!(matches!(events.first(), Some(JobEvent::Started { .. })));
        assert!(matches!(
            events.last(),
            Some(JobEvent::Finished {
                status: JobStatus::Completed,
                ..
            })
        ));
    }

    #[test]
    fn cancellation_stops_the_loop() {
        let (tx, rx) = unbounded();
        let handle = spawn_job("j2", "test", tx, |ctx| {
            for _ in 0..10_000 {
                ctx.control.checkpoint()?;
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok::<_, JobError>(())
        });
        std::thread::sleep(Duration::from_millis(30));
        handle.control.cancel();
        let err = handle.join().unwrap_err();
        assert_eq!(err.code, "job/cancelled");
        let last = rx.try_iter().last().unwrap();
        assert!(matches!(
            last,
            JobEvent::Finished {
                status: JobStatus::Cancelled,
                ..
            }
        ));
    }

    #[test]
    fn pause_blocks_and_resume_unblocks() {
        let (tx, _rx) = unbounded();
        let handle = spawn_job("j3", "test", tx, |ctx| {
            let mut iterations: u64 = 0;
            for _ in 0..50 {
                ctx.control.checkpoint()?;
                iterations += 1;
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok::<_, JobError>(iterations)
        });
        std::thread::sleep(Duration::from_millis(10));
        handle.control.pause();
        std::thread::sleep(Duration::from_millis(100));
        assert!(!handle.is_finished(), "paused job must not finish");
        handle.control.resume();
        assert_eq!(handle.join().unwrap(), 50);
    }

    #[test]
    fn cancel_while_paused_exits_cleanly() {
        let (tx, _rx) = unbounded();
        let handle = spawn_job("j4", "test", tx, |ctx| {
            loop {
                ctx.control.checkpoint()?;
                std::thread::sleep(Duration::from_millis(1));
            }
            #[allow(unreachable_code)]
            Ok::<_, JobError>(())
        });
        std::thread::sleep(Duration::from_millis(10));
        handle.control.pause();
        std::thread::sleep(Duration::from_millis(30));
        handle.control.cancel();
        let err = handle.join().unwrap_err();
        assert_eq!(err.code, "job/cancelled");
    }

    #[test]
    fn panic_becomes_structured_failure() {
        let (tx, rx) = unbounded();
        let handle = spawn_job("j5", "test", tx, |_ctx| -> Result<(), JobError> {
            panic!("boom: simulated defect");
        });
        let err = handle.join().unwrap_err();
        assert_eq!(err.code, "job/panic");
        assert!(err.message.contains("boom"));
        let last = rx.try_iter().last().unwrap();
        assert!(matches!(
            last,
            JobEvent::Finished {
                status: JobStatus::Failed,
                ..
            }
        ));
    }
}
