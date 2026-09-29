//! The shared way to talk to a configured model.
//!
//! Callers pass the messages and a JSON schema. This crate posts one
//! chat completion and returns the assistant text. It does not interpret
//! the reply, and it does not read settings from disk.

#![forbid(unsafe_code)]

use std::time::Duration;

use serde_json::{Value, json};

/// Cap on the completion response body.
const MAX_RESPONSE_BYTES: u64 = 2 * 1024 * 1024;

/// Where to send a chat completion, and as which model.
///
/// The caller supplies every field. Nothing here is read from disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatModel {
    pub base_url: String,
    pub model: String,
    pub token: Option<String>,
}

/// One message in a chat completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}

/// A chat message role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

/// A chat completion ready to send. Building it does not touch the network.
#[derive(Debug, Clone, PartialEq)]
pub struct CompletionRequest {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Value,
}

/// A hosted chat endpoint and the model ids offered on its setup screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Provider {
    pub name: &'static str,
    pub base_url: &'static str,
    pub models: &'static [&'static str],
}

/// OpenAI-compatible base URL of a local Ollama server.
pub const OLLAMA_BASE_URL: &str = "http://127.0.0.1:11434/v1";

/// Ollama's native tag list. This is not the OpenAI-compatible base URL.
pub const OLLAMA_TAGS_URL: &str = "http://127.0.0.1:11434/api/tags";

/// Cap on an Ollama tag-list body.
const MAX_TAGS_BYTES: u64 = 64 * 1024;

/// Cloud providers shown in model setup, in menu order.
pub const PROVIDERS: &[Provider] = &[
    Provider {
        name: "OpenAI",
        base_url: "https://api.openai.com/v1",
        models: &["gpt-4.1-mini", "gpt-4.1", "gpt-4o-mini"],
    },
    Provider {
        name: "Groq",
        base_url: "https://api.groq.com/openai/v1",
        models: &["llama-3.3-70b-versatile", "llama-3.1-8b-instant"],
    },
    Provider {
        name: "OpenRouter",
        base_url: "https://openrouter.ai/api/v1",
        models: &[
            "openai/gpt-4.1-mini",
            "google/gemini-2.5-flash",
            "anthropic/claude-sonnet-4",
        ],
    },
];

/// Read `models[].name` from an Ollama `/api/tags` body.
///
/// An empty name is skipped. A missing `models` array, a non-array, or an
/// entry with no string name is [`ModelError::Unreadable`].
pub fn parse_ollama_tags(body: &str) -> Result<Vec<String>, ModelError> {
    let value: Value = serde_json::from_str(body).map_err(|_| ModelError::Unreadable)?;
    let models = value
        .get("models")
        .and_then(Value::as_array)
        .ok_or(ModelError::Unreadable)?;
    let mut names = Vec::new();
    for entry in models {
        let Some(name) = entry.get("name").and_then(Value::as_str) else {
            return Err(ModelError::Unreadable);
        };
        if name.is_empty() {
            continue;
        }
        names.push(name.to_string());
    }
    Ok(names)
}

/// GET [`OLLAMA_TAGS_URL`] and parse the names.
pub fn fetch_ollama_tags() -> Result<Vec<String>, ModelError> {
    let body = notema_http::get_string(OLLAMA_TAGS_URL, MAX_TAGS_BYTES)?;
    parse_ollama_tags(&body)
}

/// Build the `POST /v1/chat/completions` call for `model`.
///
/// `schema` is embedded as the caller passed it. The schema name is always
/// `response`. An Ollama base URL also sets `reasoning_effort` to `none`.
/// Thinking models think unless told otherwise, and this call only needs
/// the JSON object.
pub fn completion_request(
    model: &ChatModel,
    messages: &[ChatMessage],
    schema: &Value,
) -> CompletionRequest {
    let mut headers = vec![("Content-Type".to_string(), "application/json".to_string())];
    if let Some(token) = bearer(model.token.as_deref()) {
        headers.push(("Authorization".to_string(), format!("Bearer {token}")));
    }
    let messages: Vec<Value> = messages
        .iter()
        .map(|message| {
            json!({
                "role": message.role.as_str(),
                "content": message.content,
            })
        })
        .collect();
    let mut body = json!({
        "model": model.model,
        "messages": messages,
        "response_format": {
            "type": "json_schema",
            "json_schema": {
                "name": "response",
                "schema": schema,
            },
        },
    });
    if is_ollama(&model.base_url) {
        body["reasoning_effort"] = json!("none");
    }
    CompletionRequest {
        url: completions_url(&model.base_url),
        headers,
        body,
    }
}

/// Read `choices[0].message.content` and return that string unchanged.
pub fn read_completion(body: &str) -> Result<String, ModelError> {
    let value: Value = serde_json::from_str(body).map_err(|_| ModelError::Unreadable)?;
    value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(ModelError::Unreadable)
}

const COMPLETION_TIMEOUT: Duration = Duration::from_secs(60);

/// Post one completion and return the assistant text.
pub fn complete(
    model: &ChatModel,
    messages: &[ChatMessage],
    schema: &Value,
) -> Result<String, ModelError> {
    let request = completion_request(model, messages, schema);
    let headers: Vec<(&str, &str)> = request
        .headers
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let body = serde_json::to_string(&request.body).expect("completion request body is JSON");
    let response = notema_http::post(
        &request.url,
        &headers,
        &body,
        MAX_RESPONSE_BYTES,
        COMPLETION_TIMEOUT,
    )?;
    read_completion(&response)
}

fn bearer(token: Option<&str>) -> Option<&str> {
    token.filter(|token| !token.trim().is_empty())
}

fn completions_url(base_url: &str) -> String {
    let base = canonical_base(base_url);
    format!("{base}/chat/completions")
}

fn is_ollama(base_url: &str) -> bool {
    canonical_base(base_url) == OLLAMA_BASE_URL
}

fn canonical_base(base_url: &str) -> &str {
    base_url.trim().trim_end_matches('/')
}

/// A chat completion failed in transit, or the body was not a completion.
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error(transparent)]
    Transport(#[from] notema_http::HttpError),
    #[error("unreadable completion body")]
    Unreadable,
}

impl ModelError {
    /// The HTTP status ureq raises before the body is read.
    pub fn status_code(&self) -> Option<u16> {
        match self {
            ModelError::Transport(error) => error.status_code(),
            ModelError::Unreadable => None,
        }
    }

    /// Timeout, connect, DNS, TLS, a body over the limit, or the iSH helper.
    /// Status codes are excluded.
    pub fn is_unreachable(&self) -> bool {
        match self {
            ModelError::Transport(error) => error.is_unreachable(),
            ModelError::Unreadable => false,
        }
    }

    /// Transport error for an HTTP status raised before the body is read.
    pub fn from_status_code(code: u16) -> Self {
        ModelError::Transport(notema_http::HttpError::from_status_code(code))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(token: Option<&str>) -> ChatModel {
        ChatModel {
            base_url: "http://127.0.0.1:11434/v1".to_string(),
            model: "llama3.2".to_string(),
            token: token.map(str::to_string),
        }
    }

    fn messages() -> Vec<ChatMessage> {
        vec![
            ChatMessage {
                role: Role::System,
                content: "Reply with JSON.".to_string(),
            },
            ChatMessage {
                role: Role::User,
                content: "Tuesday was quiet.".to_string(),
            },
        ]
    }

    fn schema() -> Value {
        json!({ "type": "object", "properties": { "ok": { "type": "boolean" } } })
    }

    fn header<'a>(request: &'a CompletionRequest, name: &str) -> Option<&'a str> {
        request
            .headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn request_without_a_token_carries_no_authorization() {
        let request = completion_request(&model(None), &messages(), &schema());

        assert_eq!(request.url, "http://127.0.0.1:11434/v1/chat/completions");
        assert_eq!(header(&request, "Content-Type"), Some("application/json"));
        assert_eq!(header(&request, "Authorization"), None);
        assert_eq!(request.body["model"], "llama3.2");
        assert_eq!(
            request.body["messages"],
            json!([
                { "role": "system", "content": "Reply with JSON." },
                { "role": "user", "content": "Tuesday was quiet." },
            ])
        );
        assert_eq!(request.body["response_format"]["type"], "json_schema");
        assert_eq!(
            request.body["response_format"]["json_schema"]["name"],
            "response"
        );
        assert_eq!(
            request.body["response_format"]["json_schema"]["schema"],
            schema()
        );
        assert_eq!(request.body["reasoning_effort"], "none");
    }

    #[test]
    fn a_cloud_request_does_not_set_reasoning_effort() {
        let mut configured = model(None);
        configured.base_url = "https://api.openai.com/v1".to_string();
        let request = completion_request(&configured, &messages(), &schema());
        assert!(request.body.get("reasoning_effort").is_none());
    }

    #[test]
    fn an_ollama_url_with_a_trailing_slash_still_sets_reasoning_effort() {
        let mut configured = model(None);
        configured.base_url = "http://127.0.0.1:11434/v1/".to_string();
        let request = completion_request(&configured, &messages(), &schema());
        assert_eq!(request.body["reasoning_effort"], "none");
    }

    #[test]
    fn a_blank_token_sends_no_authorization() {
        let request = completion_request(&model(Some("  ")), &messages(), &schema());
        assert_eq!(header(&request, "Authorization"), None);
    }

    #[test]
    fn a_token_is_sent_as_a_bearer() {
        let request = completion_request(&model(Some("secret-token")), &messages(), &schema());
        assert_eq!(
            header(&request, "Authorization"),
            Some("Bearer secret-token")
        );
    }

    #[test]
    fn a_trailing_slash_on_the_base_is_stripped() {
        let mut configured = model(None);
        configured.base_url = "http://127.0.0.1:11434/v1/".to_string();
        let request = completion_request(&configured, &messages(), &schema());
        assert_eq!(request.url, "http://127.0.0.1:11434/v1/chat/completions");
    }

    #[test]
    fn a_completion_body_returns_the_assistant_content() {
        let fenced = "```json\n{\"ok\":true}\n```";
        let body = json!({
            "choices": [{
                "message": { "role": "assistant", "content": fenced }
            }]
        });
        assert_eq!(read_completion(&body.to_string()).unwrap(), fenced);
    }

    #[test]
    fn a_non_json_body_is_an_error() {
        assert!(matches!(
            read_completion("not json"),
            Err(ModelError::Unreadable)
        ));
    }

    #[test]
    fn a_body_without_choices_is_an_error() {
        assert!(matches!(
            read_completion(r#"{"choices":[]}"#),
            Err(ModelError::Unreadable)
        ));
    }

    #[test]
    fn a_null_content_is_an_error() {
        let body = r#"{"choices":[{"message":{"content":null}}]}"#;
        assert!(matches!(read_completion(body), Err(ModelError::Unreadable)));
    }

    #[test]
    fn ollama_tags_returns_model_names() {
        let body = r#"{"models":[{"name":"llama3.2"},{"name":"qwen"}]}"#;
        assert_eq!(
            parse_ollama_tags(body).unwrap(),
            vec!["llama3.2".to_string(), "qwen".to_string()]
        );
    }

    #[test]
    fn an_empty_ollama_name_is_skipped() {
        let body = r#"{"models":[{"name":""},{"name":"llama3.2"}]}"#;
        assert_eq!(
            parse_ollama_tags(body).unwrap(),
            vec!["llama3.2".to_string()]
        );
    }

    #[test]
    fn a_nameless_ollama_entry_is_unreadable() {
        assert!(matches!(
            parse_ollama_tags(r#"{"models":[{"size":1}]}"#),
            Err(ModelError::Unreadable)
        ));
    }

    #[test]
    fn ollama_tags_without_a_models_array_are_unreadable() {
        assert!(matches!(
            parse_ollama_tags("{}"),
            Err(ModelError::Unreadable)
        ));
        assert!(matches!(
            parse_ollama_tags(r#"{"models":{}}"#),
            Err(ModelError::Unreadable)
        ));
    }

    #[test]
    fn truncated_ollama_tags_are_unreadable() {
        assert!(matches!(
            parse_ollama_tags(r#"{"models":[{"name":"llama"#),
            Err(ModelError::Unreadable)
        ));
    }

    #[test]
    fn a_status_is_not_an_unreachable_transport() {
        let denied = ModelError::from_status_code(401);
        assert_eq!(denied.status_code(), Some(401));
        assert!(!denied.is_unreachable());

        let timed_out =
            ModelError::Transport(notema_http::HttpError::TimedOut(Duration::from_secs(1)));
        assert_eq!(timed_out.status_code(), None);
        assert!(timed_out.is_unreachable());
        assert!(!ModelError::Unreadable.is_unreachable());
    }
}
