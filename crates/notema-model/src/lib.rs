//! The shared way to talk to a configured model.
//!
//! Callers pass the messages and a JSON schema. This crate posts one
//! chat completion and returns the assistant text. It does not interpret
//! the reply, and it does not read settings from disk.

#![forbid(unsafe_code)]

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

/// Build the `POST /v1/chat/completions` call for `model`.
///
/// `schema` is embedded as the caller passed it. The schema name is always
/// `response`.
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
    CompletionRequest {
        url: completions_url(&model.base_url),
        headers,
        body: json!({
            "model": model.model,
            "messages": messages,
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": "response",
                    "schema": schema,
                },
            },
        }),
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
    let response = notema_http::post_string(&request.url, &headers, &body, MAX_RESPONSE_BYTES)?;
    read_completion(&response)
}

fn bearer(token: Option<&str>) -> Option<&str> {
    token.filter(|token| !token.trim().is_empty())
}

fn completions_url(base_url: &str) -> String {
    let base = base_url.strip_suffix('/').unwrap_or(base_url);
    format!("{base}/chat/completions")
}

/// A chat completion failed in transit, or the body was not a completion.
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error(transparent)]
    Transport(#[from] notema_http::HttpError),
    #[error("unreadable completion body")]
    Unreadable,
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
}
