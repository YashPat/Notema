//! One Autofill press: read the entry on the event loop, ask the model on the
//! worker, then write empty fields back here.

use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use notema_autofill::{CurrentFields, autofill_prompt, decide_autofill};
use notema_domain::{Entry, MetadataField};
use notema_model::{ChatMessage, ChatModel, Role};
use notema_storage::EntryEdit;
use sha2::{Digest, Sha256};

use crate::AppResult;
use crate::tui::{
    app::AppModel,
    autofill::{AutofillRequest, AutofillResult, Job},
    state::{MetadataKind, ToastVariant},
    surface::{EntryMetadataValues, autofill_button_shown},
};

impl AppModel {
    /// Show the Reader button while a field is empty, or while this entry is
    /// still queued or running. `reading` is the tracked state.
    pub(crate) fn reader_autofill_state(&self, values: EntryMetadataValues<'_>) -> (bool, bool) {
        let reading = self
            .resolved_selected_entry()
            .is_some_and(|entry| self.autofill.is_tracked(&entry.id));
        (reading || autofill_button_shown(values), reading)
    }

    pub(crate) fn press_autofill(&mut self) -> AppResult<()> {
        let Some(target) = self.selected_entry_target() else {
            return Ok(());
        };
        if target.locked {
            self.toast(ToastVariant::Error, "Encryption identity not available");
            return Ok(());
        }
        if !self.allow_selected_entry_edit() {
            return Ok(());
        }
        if !self.services.config.model.is_configured() {
            self.toast(ToastVariant::Info, "No model set up");
            return Ok(());
        }
        if self.autofill.is_tracked(&target.id) {
            return Ok(());
        }
        let Some(entry) = self.resolved_selected_entry() else {
            return Ok(());
        };
        if !has_empty_field(entry) {
            return Ok(());
        }
        let journal = entry.journal.clone();
        let path = entry.path.clone();
        let (entry, _) = self
            .services
            .store
            .read_entry_with_revision(&journal, &path)?;
        if !has_empty_field(&entry) {
            self.replace_entry_from_disk(entry);
            return Ok(());
        }

        let id = entry.id.clone();
        let body_hash = body_hash(&entry.body);
        let prefix = autofill_prefix(&entry);
        let current = current_fields(&entry);
        let body = entry.body.clone();
        self.replace_entry_from_disk(entry);

        let known_people = known_names(self, MetadataKind::People);
        let known_activities = known_names(self, MetadataKind::Activities);
        let prompt = autofill_prompt(&body, &current, &known_people, &known_activities);
        let model = chat_model(&self.services.config.model);
        let cancel = Arc::new(AtomicBool::new(false));
        let request = AutofillRequest {
            id: id.clone(),
            cancel: Arc::clone(&cancel),
            model,
            messages: vec![
                ChatMessage {
                    role: Role::System,
                    content: prompt.system,
                },
                ChatMessage {
                    role: Role::User,
                    content: prompt.entry,
                },
            ],
            schema: prompt.schema,
        };
        self.autofill.enqueue(Job {
            id,
            journal,
            path,
            body_hash,
            prefix,
            cancel,
            request: Some(request),
        });
        Ok(())
    }

    /// A save that changes the body. The in-flight job toasts when it ends.
    pub(crate) fn cancel_autofill_body(&mut self, path: &Path) {
        let Some(id) = notema_storage::entry_id(path) else {
            return;
        };
        self.autofill.cancel(&id);
    }

    pub(crate) fn apply_autofill_results(&mut self) -> bool {
        let results = self.autofill.drain();
        let lost = self.autofill.take_lost();
        let mut changed = false;
        for result in results {
            if self.apply_autofill_reply(result) {
                changed = true;
            }
        }
        if lost {
            changed |= self.fail_tracked_autofill();
        } else {
            self.autofill.submit_next();
        }
        changed
    }

    fn fail_tracked_autofill(&mut self) -> bool {
        let jobs = self.autofill.take_jobs();
        let any = !jobs.is_empty();
        for job in jobs {
            if job.cancel.load(Ordering::Acquire) {
                self.toast(
                    ToastVariant::Info,
                    autofill_toast(&job.prefix, "Entry changed — press again to fill"),
                );
            } else {
                self.toast(
                    ToastVariant::Error,
                    autofill_toast(&job.prefix, "Autofill failed"),
                );
            }
        }
        any
    }

    pub(crate) fn apply_autofill_reply(&mut self, result: AutofillResult) -> bool {
        let Some(job) = self.autofill.take_job(&result.id) else {
            return false;
        };
        match self.settle_autofill(job, result.assistant) {
            FillEnd::Changed { prefix } => {
                self.toast(
                    ToastVariant::Info,
                    autofill_toast(&prefix, "Entry changed — press again to fill"),
                );
            }
            FillEnd::Failed { prefix } => {
                self.toast(
                    ToastVariant::Error,
                    autofill_toast(&prefix, "Autofill failed"),
                );
            }
            FillEnd::Filled { prefix, outcome } => {
                self.toast(ToastVariant::Success, autofill_toast(&prefix, &outcome));
            }
            FillEnd::ReloadFailed => {}
        }
        true
    }

    fn settle_autofill(
        &mut self,
        job: Job,
        assistant: Option<Result<String, notema_model::ModelError>>,
    ) -> FillEnd {
        let prefix = job.prefix.clone();
        if job.cancel.load(Ordering::Acquire) {
            return FillEnd::Changed { prefix };
        }
        let Some(assistant) = assistant else {
            return FillEnd::Failed { prefix };
        };
        let text = match assistant {
            Ok(text) => text,
            Err(_) => return FillEnd::Failed { prefix },
        };
        match self.write_autofill(&job, &text) {
            WriteAutofill::Changed => FillEnd::Changed { prefix },
            WriteAutofill::Failed => FillEnd::Failed { prefix },
            WriteAutofill::Filled(outcome) => FillEnd::Filled { prefix, outcome },
            WriteAutofill::ReloadFailed => FillEnd::ReloadFailed,
        }
    }

    fn write_autofill(&mut self, job: &Job, text: &str) -> WriteAutofill {
        let (entry, revision) = match self
            .services
            .store
            .read_entry_with_revision(&job.journal, &job.path)
        {
            Ok(pair) => pair,
            Err(_) => return WriteAutofill::Failed,
        };
        if body_hash(&entry.body) != job.body_hash || job.cancel.load(Ordering::Acquire) {
            return WriteAutofill::Changed;
        }
        let decision = decide_autofill(&current_fields(&entry), text);
        if decision.feelings.is_none()
            && decision.people.is_none()
            && decision.activities.is_none()
            && decision.mood.is_none()
        {
            return WriteAutofill::Failed;
        }

        let original = entry.metadata_bundle();
        let mut metadata = original.clone();
        if let Some(feelings) = decision.feelings {
            metadata.feelings = feelings;
        }
        if let Some(people) = decision.people {
            metadata.people = people;
        }
        if let Some(activities) = decision.activities {
            metadata.activities = activities;
        }
        if let Some(mood) = decision.mood {
            metadata.mood = Some(mood);
        }
        let body = entry.body;
        let extra: &[MetadataField] = &[];
        let saved = self.services.store.save_entry_edit_if_revision(
            &job.path,
            revision,
            EntryEdit {
                body: &body,
                metadata: &metadata,
                original_metadata: &original,
                writing_seconds: None,
                remove_if_empty: false,
                extra_fields: extra,
            },
            notema_storage::EntryAssetOptions {
                download_remote: self.services.config.attachments.download_remote_images,
                replace_offline: false,
            },
        );
        if saved.is_err() {
            return WriteAutofill::Failed;
        }
        let path = job.path.clone();
        if let Err(error) = self.refresh_paths(&[path]) {
            self.toast(
                ToastVariant::Error,
                format!("Journal changes not reloaded: {error}"),
            );
            return WriteAutofill::ReloadFailed;
        }
        WriteAutofill::Filled(decision.outcome)
    }
}

enum FillEnd {
    Changed { prefix: String },
    Failed { prefix: String },
    Filled { prefix: String, outcome: String },
    ReloadFailed,
}

enum WriteAutofill {
    Changed,
    Failed,
    Filled(String),
    ReloadFailed,
}

fn autofill_toast(prefix: &str, message: &str) -> String {
    if prefix.is_empty() {
        message.to_string()
    } else {
        format!("{prefix}: {message}")
    }
}

fn has_empty_field(entry: &Entry) -> bool {
    entry.feelings.is_empty()
        || entry.people.is_empty()
        || entry.activities.is_empty()
        || entry.mood.is_none()
}

fn current_fields(entry: &Entry) -> CurrentFields {
    CurrentFields {
        feelings: entry.feelings.clone(),
        people: entry.people.clone(),
        activities: entry.activities.clone(),
        mood: entry.mood,
    }
}

fn known_names(app: &AppModel, kind: MetadataKind) -> Vec<String> {
    app.metadata_partitioned(kind)
        .0
        .into_iter()
        .map(|(name, _count)| name)
        .collect()
}

fn chat_model(section: &crate::config::ModelSection) -> ChatModel {
    ChatModel {
        base_url: section.base_url.clone(),
        model: section.model.clone(),
        token: section.token.clone(),
    }
}

pub(crate) fn body_hash(body: &str) -> [u8; 32] {
    Sha256::digest(body.as_bytes()).into()
}

fn autofill_prefix(entry: &Entry) -> String {
    let day = crate::tui::entry_rows::entry_day_label(entry);
    let time = entry
        .created_time()
        .map(|timestamp| timestamp.strftime("%H:%M").to_string());
    let when = match (day, time) {
        (Some(day), Some(time)) => Some(format!("{day} {time}")),
        (Some(day), None) => Some(day),
        (None, Some(time)) => Some(time),
        (None, None) => None,
    };
    let title = first_body_title(&entry.body);
    match (when, title) {
        (Some(when), Some(title)) => format!("{when} — {title}"),
        (Some(when), None) => when,
        (None, Some(title)) => title,
        (None, None) => String::new(),
    }
}

/// First non-empty body line. A leading `#` run is stripped only when the next
/// character is a space.
fn first_body_title(body: &str) -> Option<String> {
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let text = match heading_text(trimmed) {
            Some("") => continue,
            Some(text) => text,
            None => trimmed,
        };
        return Some(text.to_string());
    }
    None
}

fn heading_text(line: &str) -> Option<&str> {
    if !line.starts_with('#') {
        return None;
    }
    let after_hashes = line.trim_start_matches('#');
    if after_hashes.starts_with(' ') {
        Some(after_hashes.trim())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_support::app_with_entry;
    use std::fs;

    fn park_selected(app: &mut AppModel) -> (String, String) {
        let (id, journal, path, hash, prefix) = {
            let entry = app.resolved_selected_entry().unwrap();
            (
                entry.id.clone(),
                entry.journal.clone(),
                entry.path.clone(),
                body_hash(&entry.body),
                autofill_prefix(entry),
            )
        };
        app.autofill
            .park(id.clone(), journal, path, hash, prefix.clone());
        (id, prefix)
    }

    fn people_reply(id: &str) -> AutofillResult {
        AutofillResult {
            id: id.to_string(),
            assistant: Some(Ok(
                r#"{"feelings":[],"people":["Sam"],"activities":[],"mood":null}"#.to_string(),
            )),
        }
    }

    #[test]
    fn press_without_a_model_toasts_and_does_not_track() {
        let mut app = app_with_entry();
        let id = app.resolved_selected_entry().unwrap().id.clone();

        app.press_autofill().unwrap();

        assert!(!app.autofill.has_pending());
        assert!(!app.autofill.is_tracked(&id));
        let toast = app.toasts.items().last().unwrap();
        assert_eq!(toast.variant, ToastVariant::Info);
        assert_eq!(toast.message, "No model set up");
    }

    #[test]
    fn a_changed_body_is_not_filled() {
        let mut app = app_with_entry();
        let (id, prefix) = park_selected(&mut app);
        let path = app.resolved_selected_entry().unwrap().path.clone();
        let original = fs::read_to_string(&path).unwrap();
        fs::write(&path, original.replace("\nBody\n", "\nChanged body\n")).unwrap();

        assert!(app.apply_autofill_reply(people_reply(&id)));

        let journal = app.resolved_selected_entry().unwrap().journal.clone();
        let (entry, _) = app
            .services
            .store
            .read_entry_with_revision(&journal, &path)
            .unwrap();
        assert!(entry.people.is_empty(), "hash mismatch must not save");
        assert!(entry.body.contains("Changed body"));
        assert!(!app.autofill.is_tracked(&id));
        let toast = app.toasts.items().last().unwrap();
        assert_eq!(toast.variant, ToastVariant::Info);
        assert_eq!(
            toast.message,
            format!("{prefix}: Entry changed — press again to fill")
        );
    }

    #[test]
    fn a_matching_body_fills_an_empty_field() {
        let mut app = app_with_entry();
        let (id, prefix) = park_selected(&mut app);

        assert!(app.apply_autofill_reply(people_reply(&id)));

        let entry = app.resolved_selected_entry().unwrap();
        assert_eq!(entry.people, ["Sam".to_string()]);
        assert!(entry.feelings.is_empty());
        assert!(entry.activities.is_empty());
        assert_eq!(entry.mood, None);
        assert!(!app.autofill.is_tracked(&id));
        let toast = app.toasts.items().last().unwrap();
        assert_eq!(toast.variant, ToastVariant::Success);
        assert_eq!(toast.message, format!("{prefix}: Autofilled people"));
        assert_eq!(prefix, "Wednesday 01 10:00 — A");
    }

    #[test]
    fn a_missing_reply_toasts_failed() {
        let mut app = app_with_entry();
        let (id, prefix) = park_selected(&mut app);

        assert!(app.apply_autofill_reply(AutofillResult {
            id: id.clone(),
            assistant: None,
        }));

        let toast = app.toasts.items().last().unwrap();
        assert_eq!(toast.variant, ToastVariant::Error);
        assert_eq!(toast.message, format!("{prefix}: Autofill failed"));
    }

    #[test]
    fn a_cancelled_lost_job_toasts_entry_changed() {
        let mut app = app_with_entry();
        let (id, prefix) = park_selected(&mut app);
        app.autofill.cancel(&id);

        assert!(app.fail_tracked_autofill());

        let toast = app.toasts.items().last().unwrap();
        assert_eq!(toast.variant, ToastVariant::Info);
        assert_eq!(
            toast.message,
            format!("{prefix}: Entry changed — press again to fill")
        );
    }

    #[test]
    fn a_lost_job_toasts_autofill_failed() {
        let mut app = app_with_entry();
        let (_id, prefix) = park_selected(&mut app);

        assert!(app.fail_tracked_autofill());

        let toast = app.toasts.items().last().unwrap();
        assert_eq!(toast.variant, ToastVariant::Error);
        assert_eq!(toast.message, format!("{prefix}: Autofill failed"));
    }

    #[test]
    fn a_tracked_entry_stays_shown_when_every_field_is_set() {
        let mut app = app_with_entry();
        park_selected(&mut app);
        let people = vec!["Sam".to_string()];
        let activities = vec!["walk".to_string()];
        let feelings = vec!["calm".to_string()];
        let values = EntryMetadataValues {
            people: &people,
            activities: &activities,
            feelings: &feelings,
            mood: Some(0),
            environment: &[],
        };

        let (show, reading) = app.reader_autofill_state(values);

        assert!(show);
        assert!(reading);
        assert!(!autofill_button_shown(values));
    }
}
