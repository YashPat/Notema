//! Background fetch of the local Ollama model list, over the shared [`Worker`].

use crate::tui::{app::AppModel, runtime::worker::Worker};

/// The background tag-list worker, spawned on first use.
pub(crate) type OllamaTagsWorker = Worker<OllamaTagsRequest, OllamaTagsResult>;

/// A tag-list fetch handed to the worker, tagged with the request id the
/// caller assigned.
#[derive(Debug, PartialEq)]
pub(crate) struct OllamaTagsRequest {
    pub(crate) id: u64,
}

/// A finished tag list. `names` is the parsed model names, or a human-readable
/// failure.
pub(crate) struct OllamaTagsResult {
    pub(crate) id: u64,
    pub(crate) names: Result<Vec<String>, String>,
}

/// Resolve one tag-list request. Runs on the worker thread.
pub(crate) fn resolve(request: OllamaTagsRequest) -> OllamaTagsResult {
    OllamaTagsResult {
        id: request.id,
        names: notema_model::fetch_ollama_tags().map_err(|error| error.to_string()),
    }
}

impl AppModel {
    /// Fold finished tag replies into the open Local screen.
    ///
    /// A reply whose id is not the one Local is waiting on, or that arrives
    /// after Local has been left, is ignored. A worker that died with a request
    /// still pending is a failed fetch.
    pub(crate) fn apply_ollama_tags(&mut self) -> bool {
        let results = self.ollama_tags.drain();
        let lost = self.ollama_tags.take_lost();
        let mut changed = false;
        for result in results {
            changed |= self.accept_ollama_tags(result.id, result.names);
        }
        if lost
            && let Some(id) = self
                .model_setup_state()
                .filter(|state| state.on_local())
                .and_then(|state| state.pending_tags_id)
        {
            changed |=
                self.accept_ollama_tags(id, Err("the model list stopped unexpectedly".into()));
        }
        changed
    }
}
