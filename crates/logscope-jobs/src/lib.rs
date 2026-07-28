//! Background job abstraction for LogScope.
//!
//! Jobs run on worker threads with progress reporting, cooperative pause and
//! cancellation, and structured failure. No async runtime is required; the
//! desktop shell bridges these primitives onto its own event loop.

pub mod control;
pub mod error;
pub mod runner;

pub use control::JobControl;
pub use error::{Cancelled, JobError};
pub use runner::{spawn_job, JobContext, JobEvent, JobHandle, JobProgress, JobStatus};
