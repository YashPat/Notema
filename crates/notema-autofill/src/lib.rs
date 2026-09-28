//! The autofill decision.
//!
//! Given a model response and the entry's current fields, decide which empty
//! fields to write and the outcome sentence. No I/O and no terminal types.

#![forbid(unsafe_code)]

use notema_domain::{MOOD_RANGE, normalize_feelings};
use serde_json::Value;

/// The four fields autofill may write, as they stand on the entry now.
///
/// A non-empty list, or a mood of [`Some`], is already set and is left alone.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CurrentFields {
    pub feelings: Vec<String>,
    pub people: Vec<String>,
    pub activities: Vec<String>,
    pub mood: Option<i8>,
}

/// The empty fields worth writing, and the sentence that names them.
///
/// [`None`] means do not write that field. A list here is never empty.
/// A mood of zero is a real write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutofillDecision {
    pub feelings: Option<Vec<String>>,
    pub people: Option<Vec<String>>,
    pub activities: Option<Vec<String>>,
    pub mood: Option<i8>,
    pub outcome: String,
}

/// Decide which empty fields `response` may fill.
///
/// Strips one wrapping code fence, then reads
/// `{"feelings":[...],"people":[...],"activities":[...],"mood":N}`.
/// A failed parse, or a reply with nothing valid left to write, yields
/// [`AutofillDecision`] with every field [`None`] and outcome `Autofill failed`.
pub fn decide_autofill(current: &CurrentFields, response: &str) -> AutofillDecision {
    let Some(object) = model_object(response) else {
        return failed();
    };

    let feelings = assign_list(
        current.feelings.is_empty(),
        object.get("feelings"),
        feelings_from,
    );
    let people = assign_list(
        current.people.is_empty(),
        object.get("people"),
        free_form_from,
    );
    let activities = assign_list(
        current.activities.is_empty(),
        object.get("activities"),
        free_form_from,
    );
    let mood = assign_mood(current.mood.is_none(), object.get("mood"));

    let mut names = Vec::new();
    if feelings.is_some() {
        names.push("feelings");
    }
    if people.is_some() {
        names.push("people");
    }
    if activities.is_some() {
        names.push("activities");
    }
    if mood.is_some() {
        names.push("mood");
    }
    if names.is_empty() {
        return failed();
    }

    AutofillDecision {
        feelings,
        people,
        activities,
        mood,
        outcome: outcome_sentence(&names),
    }
}

fn failed() -> AutofillDecision {
    AutofillDecision {
        feelings: None,
        people: None,
        activities: None,
        mood: None,
        outcome: "Autofill failed".to_string(),
    }
}

fn outcome_sentence(names: &[&str]) -> String {
    match names {
        [] => "Autofill failed".to_string(),
        [only] => format!("Autofilled {only}"),
        [first, second] => format!("Autofilled {first} and {second}"),
        [rest @ .., last] => format!("Autofilled {}, and {last}", rest.join(", ")),
    }
}

/// Unwrap one markdown fence when the whole reply is that fence.
/// Prose around a fence, or a fence that is not well formed, is a failed parse.
fn unwrap_fence(response: &str) -> Option<&str> {
    let text = response.trim();
    if !text.starts_with("```") {
        return Some(text);
    }
    let (opening, rest) = text.split_once('\n')?;
    let tag = opening.trim().strip_prefix("```")?;
    let tag_ok = tag.is_empty()
        || tag
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '-'));
    if !tag_ok {
        return None;
    }
    let rest = rest.trim_end();
    let inner = rest.strip_suffix("```")?.trim_end_matches('\r');
    if !inner.is_empty() && !inner.ends_with('\n') {
        return None;
    }
    Some(inner.trim())
}

fn model_object(response: &str) -> Option<serde_json::Map<String, Value>> {
    match serde_json::from_str(unwrap_fence(response)?).ok()? {
        Value::Object(map) => Some(map),
        _ => None,
    }
}

fn assign_list(
    unset: bool,
    value: Option<&Value>,
    parse: fn(&Value) -> Option<Vec<String>>,
) -> Option<Vec<String>> {
    if !unset {
        return None;
    }
    let parsed = parse(value?)?;
    if parsed.is_empty() {
        None
    } else {
        Some(parsed)
    }
}

fn assign_mood(unset: bool, value: Option<&Value>) -> Option<i8> {
    if !unset {
        return None;
    }
    let Value::Number(number) = value? else {
        return None;
    };
    let number = i8::try_from(number.as_i64()?).ok()?;
    MOOD_RANGE.contains(&number).then_some(number)
}

fn feelings_from(value: &Value) -> Option<Vec<String>> {
    let Value::Array(items) = value else {
        return None;
    };
    let strings = items.iter().filter_map(Value::as_str);
    Some(normalize_feelings(strings))
}

fn free_form_from(value: &Value) -> Option<Vec<String>> {
    let Value::Array(items) = value else {
        return None;
    };
    let mut values = Vec::new();
    for item in items {
        let Value::String(text) = item else {
            continue;
        };
        let text = text.trim();
        if text.is_empty() || values.iter().any(|existing| existing == text) {
            continue;
        }
        values.push(text.to_string());
    }
    Some(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decide(response: &str) -> AutofillDecision {
        decide_autofill(&CurrentFields::default(), response)
    }

    #[test]
    fn fence_unwraps_one_json_block() {
        let decision = decide("```json\n{\"people\":[\"Sam\"],\"activities\":[\"hiking\"]}\n```");
        assert_eq!(
            decision.people.as_deref(),
            Some(["Sam".to_string()].as_slice())
        );
        assert_eq!(
            decision.activities.as_deref(),
            Some(["hiking".to_string()].as_slice())
        );
        assert_eq!(decision.outcome, "Autofilled people and activities");
    }

    #[test]
    fn out_of_range_mood_is_dropped() {
        let decision = decide("{\"feelings\":[\"calm\"],\"mood\":9}");
        assert_eq!(
            decision.feelings.as_deref(),
            Some(["calm".to_string()].as_slice())
        );
        assert_eq!(decision.mood, None);
        assert_eq!(decision.outcome, "Autofilled feelings");
    }

    #[test]
    fn unknown_feeling_is_dropped() {
        let decision = decide("{\"feelings\":[\"nope\",\"calm\"]}");
        assert_eq!(
            decision.feelings.as_deref(),
            Some(["calm".to_string()].as_slice())
        );
        assert_eq!(decision.outcome, "Autofilled feelings");
    }

    #[test]
    fn already_set_field_is_left_unchanged() {
        let current = CurrentFields {
            people: vec!["Ada".to_string()],
            ..CurrentFields::default()
        };
        let decision = decide_autofill(
            &current,
            "{\"people\":[\"Sam\"],\"activities\":[\"hiking\"]}",
        );
        assert_eq!(decision.people, None);
        assert_eq!(
            decision.activities.as_deref(),
            Some(["hiking".to_string()].as_slice())
        );
        assert_eq!(decision.outcome, "Autofilled activities");
    }

    #[test]
    fn parse_failure_writes_nothing() {
        let decision = decide("not json");
        assert_eq!(decision.feelings, None);
        assert_eq!(decision.people, None);
        assert_eq!(decision.activities, None);
        assert_eq!(decision.mood, None);
        assert_eq!(decision.outcome, "Autofill failed");
    }

    #[test]
    fn outcome_names_only_written_fields() {
        let decision = decide("{\"people\":[\"Sam\"],\"activities\":[\"hiking\"],\"mood\":9}");
        assert_eq!(decision.mood, None);
        assert_eq!(decision.outcome, "Autofilled people and activities");
    }

    #[test]
    fn empty_list_is_not_written() {
        let decision = decide("{\"people\":[],\"activities\":[\"hiking\"]}");
        assert_eq!(decision.people, None);
        assert_eq!(decision.outcome, "Autofilled activities");
    }

    #[test]
    fn feeling_alias_becomes_canonical() {
        let decision = decide("{\"feelings\":[\"joyous\"]}");
        assert_eq!(
            decision.feelings.as_deref(),
            Some(["happy".to_string()].as_slice())
        );
        assert_eq!(decision.outcome, "Autofilled feelings");
    }

    #[test]
    fn non_integer_mood_is_dropped() {
        let decision = decide("{\"feelings\":[\"calm\"],\"mood\":2.0}");
        assert_eq!(
            decision.feelings.as_deref(),
            Some(["calm".to_string()].as_slice())
        );
        assert_eq!(decision.mood, None);
        assert_eq!(decision.outcome, "Autofilled feelings");
    }

    #[test]
    fn prose_around_a_fence_fails() {
        let decision = decide("here you go\n```json\n{\"mood\":1}\n```");
        assert_eq!(decision.outcome, "Autofill failed");
        assert_eq!(decision.mood, None);
    }

    #[test]
    fn mood_zero_is_a_write() {
        let decision = decide("{\"mood\":0}");
        assert_eq!(decision.mood, Some(0));
        assert_eq!(decision.outcome, "Autofilled mood");
    }

    #[test]
    fn outcome_joins_three_fields() {
        let decision = decide("{\"feelings\":[\"calm\"],\"people\":[\"Sam\"],\"mood\":2}");
        assert_eq!(decision.outcome, "Autofilled feelings, people, and mood");
    }
}
