//! JSON-RPC 2.0 message types, as LSP uses them.
//!
//! Hand-written rather than taken from `lsp-types`, deliberately. See the crate
//! Cargo.toml for the reasoning; the short version is that real servers send
//! values outside the spec and we need to tolerate them, where a fully-modelled
//! protocol crate errors.
//!
//! # The one thing to get right here
//!
//! **A message's kind is determined by which fields it has, not by a tag.**
//! JSON-RPC has no `type` field, so:
//!
//! | has `method` | has `id` | it is |
//! |---|---|---|
//! | yes | yes | a request — someone must answer it |
//! | yes | no | a notification — nobody answers it |
//! | no | yes | a response to something we sent |
//! | no | no | malformed |
//!
//! Getting this wrong is not a parse error, it is a deadlock: a server *request*
//! mistaken for a notification is never answered, and a server waiting on an
//! answer stops serving. omp has a regression test for the id-collision case
//! precisely because this classification is easy to write plausibly and wrong.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A JSON-RPC id.
///
/// Both a number and a string are legal, and servers use both: `rust-analyzer`
/// sends numbers, some others send strings. Modelling this as a number only
/// works until it does not, and then the failure is an unanswerable request.
///
/// # Why there is an `Other` variant
///
/// Found by probing this type rather than by a test. An `untagged` enum of
/// number-or-string silently yields `None` for anything else — a float, a bool,
/// an object. Combined with the classification rule below, **an id we cannot
/// parse demotes a server request to a notification**, so we never answer it and
/// the server waits forever. That is the deadlock this module's header warns
/// about, reachable through a type conversion rather than through the logic.
///
/// `Other` keeps the raw value so it can be echoed back verbatim. We do not need
/// to understand an id, only to return exactly what we were given.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    Number(i64),
    String(String),
    /// Anything else, preserved so the answer carries the id the server sent.
    Other(Value),
}

/// Equality and hashing go through the JSON text.
///
/// `serde_json::Value` implements neither `Hash` nor `Eq` (floats), but a pending
/// map needs both. Comparing the serialized form is exact for the shapes that
/// matter and total for the rest.
impl PartialEq for RequestId {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Number(a), Self::Number(b)) => a == b,
            (Self::String(a), Self::String(b)) => a == b,
            _ => self.key() == other.key(),
        }
    }
}

impl Eq for RequestId {}

impl std::hash::Hash for RequestId {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        // Hash the same representation equality compares, or a HashMap lookup
        // can miss a key it holds.
        self.key().hash(state);
    }
}

impl RequestId {
    /// Canonical text form, used for equality, hashing, and messages.
    fn key(&self) -> String {
        match self {
            Self::Number(id) => format!("n:{id}"),
            Self::String(id) => format!("s:{id}"),
            Self::Other(value) => format!("o:{value}"),
        }
    }
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Number(id) => write!(f, "{id}"),
            Self::String(id) => write!(f, "{id}"),
            Self::Other(value) => write!(f, "{value}"),
        }
    }
}

impl From<i64> for RequestId {
    fn from(id: i64) -> Self {
        Self::Number(id)
    }
}

impl From<String> for RequestId {
    fn from(id: String) -> Self {
        Self::String(id)
    }
}

/// An error object in a response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// The spec's "method not found".
///
/// This code is load-bearing rather than informational. A server answering
/// `-32601` is healthy and simply does not implement the request, so the correct
/// response is to stop asking, not to tear the connection down. omp has a test
/// asserting it is recognised **by code and not by message text**, because
/// servers word it differently and matching on prose silently stops working.
pub const METHOD_NOT_FOUND: i64 = -32601;

impl ResponseError {
    pub fn is_method_not_found(&self) -> bool {
        self.code == METHOD_NOT_FOUND
    }
}

impl std::fmt::Display for ResponseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (code {})", self.message, self.code)
    }
}

/// One decoded incoming message, classified by shape.
#[derive(Debug, Clone)]
pub enum Incoming {
    /// The server is asking us something and is waiting for an answer.
    Request {
        id: RequestId,
        method: String,
        params: Value,
    },
    /// The server is telling us something. No answer.
    Notification { method: String, params: Value },
    /// An answer to something we sent.
    Response {
        id: RequestId,
        result: Result<Value, ResponseError>,
    },
}

/// Why a message could not be classified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// Not JSON, or not a JSON object.
    NotAnObject,
    /// Neither a `method` nor an `id`, so it is neither a call nor an answer.
    NeitherCallNorAnswer,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAnObject => write!(f, "JSON-RPC message is not a JSON object"),
            Self::NeitherCallNorAnswer => {
                write!(f, "JSON-RPC message has neither a method nor an id")
            }
        }
    }
}

impl std::error::Error for DecodeError {}

/// Classify a decoded JSON value as an incoming message.
///
/// Notably tolerant in three places, each because a real server needs it:
///
/// - **`jsonrpc: "2.0"` is not required.** Some servers omit it. Refusing the
///   message would be spec-correct and useless.
/// - **A response with neither `result` nor `error`** is treated as a `null`
///   result, which is what a server means by it.
/// - **Unknown extra fields are ignored**, since servers add their own.
pub fn decode(value: Value) -> Result<Incoming, DecodeError> {
    let Value::Object(mut object) = value else {
        return Err(DecodeError::NotAnObject);
    };

    let id: Option<RequestId> = match object.remove("id") {
        // An explicit `"id": null` is not an id. Notifications sometimes carry
        // it, and treating null as an id would have us answer a notification —
        // which a server may then reject as an unsolicited response.
        None | Some(Value::Null) => None,
        // Anything else present *is* an id, even a shape we do not model. Falling
        // back to `None` here is the deadlock described on `RequestId`: the
        // request would classify as a notification, go unanswered, and stall the
        // server. `Other` preserves it so the answer echoes it verbatim.
        Some(raw) => Some(
            serde_json::from_value(raw.clone()).unwrap_or(RequestId::Other(raw)),
        ),
    };
    let method = object
        .remove("method")
        .and_then(|value| value.as_str().map(str::to_string));
    let params = object.remove("params").unwrap_or(Value::Null);

    match (method, id) {
        (Some(method), Some(id)) => Ok(Incoming::Request { id, method, params }),
        (Some(method), None) => Ok(Incoming::Notification { method, params }),
        (None, Some(id)) => {
            let result = match object.remove("error") {
                Some(Value::Null) | None => {
                    Ok(object.remove("result").unwrap_or(Value::Null))
                }
                Some(raw) => match serde_json::from_value::<ResponseError>(raw) {
                    Ok(error) => Err(error),
                    // A malformed error object is still a failure, and reporting
                    // it as a success with a null result would be a lie the
                    // caller cannot detect.
                    Err(problem) => Err(ResponseError {
                        code: 0,
                        message: format!("malformed error object: {problem}"),
                        data: None,
                    }),
                },
            };
            Ok(Incoming::Response { id, result })
        }
        (None, None) => Err(DecodeError::NeitherCallNorAnswer),
    }
}

/// Serialize a request we are sending.
pub fn request(id: &RequestId, method: &str, params: &Value) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    })
}

/// Serialize a notification we are sending.
pub fn notification(method: &str, params: &Value) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    })
}

/// Serialize a successful answer to a server request.
pub fn response(id: &RequestId, result: &Value) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

/// Serialize an error answer to a server request.
pub fn error_response(id: &RequestId, code: i64, message: &str) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message},
    })
}

#[cfg(test)]
#[path = "jsonrpc_tests.rs"]
mod jsonrpc_tests;
