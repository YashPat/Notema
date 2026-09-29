//! The autofill decision and the prompt that asks for it.
//!
//! [`autofill_prompt`] builds the system string, the entry string, and the JSON
//! schema. [`decide_autofill`] reads a model response and decides which empty
//! fields to write. No I/O and no terminal types.

#![forbid(unsafe_code)]

use notema_domain::{MOOD_RANGE, feelings, normalize_feelings};
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

/// The system string, the entry string, and the JSON schema for one autofill call.
///
/// The caller maps `system` and `entry` onto chat messages. This crate does not
/// call the model.
#[derive(Debug, Clone, PartialEq)]
pub struct AutofillPrompt {
    pub system: String,
    pub entry: String,
    pub schema: Value,
}

/// Build the prompt for one entry.
///
/// `known_people` and `known_activities` are the distinct strings already stored
/// on entries. Blank strings are dropped. This function does not walk the journal.
pub fn autofill_prompt(
    body: &str,
    current: &CurrentFields,
    known_people: &[String],
    known_activities: &[String],
) -> AutofillPrompt {
    AutofillPrompt {
        system: system_prompt(known_people, known_activities),
        entry: entry_prompt(body, current),
        schema: response_schema(),
    }
}

fn system_prompt(known_people: &[String], known_activities: &[String]) -> String {
    let feeling_names: Vec<&str> = feelings().collect();
    let feelings_json = serde_json::to_string(&feeling_names).expect("feeling names are strings");
    let people_json = serde_json::to_string(&listed(known_people)).expect("people are strings");
    let activities_json =
        serde_json::to_string(&listed(known_activities)).expect("activities are strings");
    let mood_start = *MOOD_RANGE.start();
    let mood_end = *MOOD_RANGE.end();

    format!(
        "\
Return only a JSON object with keys \"feelings\", \"people\", \"activities\", and \"mood\".
Use only these feeling names: {feelings_json}
Mood is an integer from {mood_start} to {mood_end}.
People already used: {people_json}
Activities already used: {activities_json}
Reuse a name from those lists when it is the same person, or same activity. Add a new name only when it is worth adding.
Copy a value the user already entered. Do not overwrite it. Choose a value only for an empty field."
    )
}

fn listed(values: &[String]) -> Vec<&str> {
    values
        .iter()
        .filter(|value| !value.trim().is_empty())
        .map(String::as_str)
        .collect()
}

fn entry_prompt(body: &str, current: &CurrentFields) -> String {
    format!(
        "Body:\n{body}\nAlready entered: {}\nEmpty: {}",
        already_entered(current),
        empty_names(current),
    )
}

fn already_entered(current: &CurrentFields) -> String {
    let mut parts = Vec::new();
    if !current.feelings.is_empty() {
        parts.push(json_field("feelings", &current.feelings));
    }
    if !current.people.is_empty() {
        parts.push(json_field("people", &current.people));
    }
    if !current.activities.is_empty() {
        parts.push(json_field("activities", &current.activities));
    }
    if let Some(mood) = current.mood {
        parts.push(format!("\"mood\":{mood}"));
    }
    format!("{{{}}}", parts.join(","))
}

fn json_field(name: &str, values: &[String]) -> String {
    let encoded = serde_json::to_string(values).expect("field values are strings");
    format!("\"{name}\":{encoded}")
}

fn empty_names(current: &CurrentFields) -> String {
    let mut names = Vec::new();
    if current.feelings.is_empty() {
        names.push("feelings");
    }
    if current.people.is_empty() {
        names.push("people");
    }
    if current.activities.is_empty() {
        names.push("activities");
    }
    if current.mood.is_none() {
        names.push("mood");
    }
    serde_json::to_string(&names).expect("field names are strings")
}

fn response_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "feelings": { "type": "array", "items": { "type": "string" } },
            "people": { "type": "array", "items": { "type": "string" } },
            "activities": { "type": "array", "items": { "type": "string" } },
            "mood": { "type": "integer" }
        },
        "required": ["feelings", "people", "activities", "mood"],
        "additionalProperties": false
    })
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

    #[test]
    fn schema_is_stable_and_unconstrained() {
        let first = autofill_prompt("one", &CurrentFields::default(), &[], &[]);
        let second = autofill_prompt(
            "two",
            &CurrentFields {
                mood: Some(0),
                ..CurrentFields::default()
            },
            &["Sam".to_string()],
            &["hiking".to_string()],
        );
        assert_eq!(first.schema, second.schema);
        let text = first.schema.to_string();
        assert!(!text.contains("enum"));
        assert!(first.schema["properties"]["mood"].get("minimum").is_none());
        assert!(first.schema["properties"]["mood"].get("maximum").is_none());
        assert_eq!(
            first.schema["required"],
            serde_json::json!(["feelings", "people", "activities", "mood"])
        );
        assert_eq!(first.schema["additionalProperties"], false);
        assert_eq!(first.schema["properties"]["feelings"]["type"], "array");
        assert_eq!(first.schema["properties"]["mood"]["type"], "integer");
    }

    #[test]
    fn system_lists_canonical_feelings_bounds_and_known_names() {
        let prompt = autofill_prompt(
            "body",
            &CurrentFields::default(),
            &["Ada".to_string(), "  ".to_string(), "Sam".to_string()],
            &["hiking".to_string()],
        );
        assert!(prompt.system.contains("\"happy\""));
        assert!(!prompt.system.contains("joyous"));
        assert!(prompt.system.contains("Mood is an integer from -5 to 5."));
        assert!(
            prompt
                .system
                .contains("People already used: [\"Ada\",\"Sam\"]")
        );
        assert!(
            prompt
                .system
                .contains("Activities already used: [\"hiking\"]")
        );
        assert!(prompt.system.contains(
            "Reuse a name from those lists when it is the same person, or same activity."
        ));
        assert!(prompt.system.contains(
            "Copy a value the user already entered. Do not overwrite it. Choose a value only for an empty field."
        ));
    }

    #[test]
    fn empty_known_lists_are_empty_arrays() {
        let prompt = autofill_prompt("body", &CurrentFields::default(), &[], &[]);
        assert!(prompt.system.contains("People already used: []"));
        assert!(prompt.system.contains("Activities already used: []"));
    }

    #[test]
    fn entry_keeps_set_fields_and_names_empty_ones() {
        let current = CurrentFields {
            feelings: vec![],
            people: vec!["Ada".to_string()],
            activities: vec![],
            mood: Some(0),
        };
        let prompt = autofill_prompt("Walked.\n", &current, &[], &[]);
        assert!(prompt.entry.starts_with("Body:\nWalked.\n"));
        assert!(
            prompt
                .entry
                .contains("Already entered: {\"people\":[\"Ada\"],\"mood\":0}")
        );
        assert!(
            prompt
                .entry
                .contains("Empty: [\"feelings\",\"activities\"]")
        );
        assert!(!prompt.entry.contains("\"feelings\":"));
    }
}
