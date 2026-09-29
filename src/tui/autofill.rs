//! Background autofill over the shared [`Worker`]. One completion runs at a
//! time. Further entries are tracked immediately and posted when it returns.

use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use notema_model::{ChatMessage, ChatModel, ModelError};
use serde_json::Value;

use crate::tui::runtime::worker::Worker;

/// The background autofill worker, spawned on first use.
pub(crate) type AutofillWorker = Worker<AutofillRequest, AutofillResult>;

/// One completion, including everything `resolve` needs. The worker thread
/// captures nothing.
pub(crate) struct AutofillRequest {
    pub(crate) id: String,
    pub(crate) cancel: Arc<AtomicBool>,
    pub(crate) model: ChatModel,
    pub(crate) messages: Vec<ChatMessage>,
    pub(crate) schema: Value,
}

/// A finished autofill call. `assistant` is `None` when the cancel flag was
/// set before `complete` ran.
pub(crate) struct AutofillResult {
    pub(crate) id: String,
    pub(crate) assistant: Option<Result<String, ModelError>>,
}

/// Resolve one autofill request. Runs on the worker thread.
pub(crate) fn resolve(request: AutofillRequest) -> AutofillResult {
    if request.cancel.load(Ordering::Acquire) {
        return AutofillResult {
            id: request.id,
            assistant: None,
        };
    }
    let assistant = notema_model::complete(&request.model, &request.messages, &request.schema);
    AutofillResult {
        id: request.id,
        assistant: Some(assistant),
    }
}

/// An entry queued or running. The button reads this set. [`Autofill::has_pending`]
/// is only the loop's wake-up.
pub(crate) struct Job {
    pub(crate) id: String,
    pub(crate) journal: String,
    pub(crate) path: PathBuf,
    pub(crate) body_hash: [u8; 32],
    pub(crate) prefix: String,
    pub(crate) cancel: Arc<AtomicBool>,
    /// Taken when the request is posted. A second entry keeps its request here
    /// until the running resolve returns.
    pub(crate) request: Option<AutofillRequest>,
}

/// The worker plus the entries tracked for it.
#[derive(Default)]
pub(crate) struct Autofill {
    worker: AutofillWorker,
    jobs: VecDeque<Job>,
}

impl Autofill {
    pub(crate) fn has_pending(&self) -> bool {
        self.worker.has_pending()
    }

    pub(crate) fn is_tracked(&self, id: &str) -> bool {
        self.jobs.iter().any(|job| job.id == id)
    }

    /// A body save sets the flag. The job toasts when it ends.
    pub(crate) fn cancel(&mut self, id: &str) {
        if let Some(job) = self.jobs.iter().find(|job| job.id == id) {
            job.cancel.store(true, Ordering::Release);
        }
    }

    pub(crate) fn enqueue(&mut self, job: Job) {
        self.jobs.push_back(job);
        self.submit_next();
    }

    /// Post the next tracked entry once the running resolve has returned.
    pub(crate) fn submit_next(&mut self) {
        if self.worker.has_pending() {
            return;
        }
        let Some(request) = self.jobs.iter_mut().find_map(|job| job.request.take()) else {
            return;
        };
        self.worker.request(request, resolve);
    }

    pub(crate) fn drain(&mut self) -> Vec<AutofillResult> {
        self.worker.drain()
    }

    pub(crate) fn take_lost(&mut self) -> bool {
        self.worker.take_lost()
    }

    pub(crate) fn take_job(&mut self, id: &str) -> Option<Job> {
        let index = self.jobs.iter().position(|job| job.id == id)?;
        self.jobs.remove(index)
    }

    pub(crate) fn take_jobs(&mut self) -> VecDeque<Job> {
        std::mem::take(&mut self.jobs)
    }

    #[cfg(test)]
    pub(crate) fn is_cancelled(&self, id: &str) -> bool {
        self.jobs
            .iter()
            .find(|job| job.id == id)
            .is_some_and(|job| job.cancel.load(Ordering::Acquire))
    }

    #[cfg(test)]
    pub(crate) fn park(
        &mut self,
        id: String,
        journal: String,
        path: PathBuf,
        body_hash: [u8; 32],
        prefix: String,
    ) {
        self.jobs.push_back(Job {
            id,
            journal,
            path,
            body_hash,
            prefix,
            cancel: Arc::new(AtomicBool::new(false)),
            request: None,
        });
    }
}
