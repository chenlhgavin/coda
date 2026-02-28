//! JSON-RPC 2.0 helpers for the Codex app-server protocol.

use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};

/// Thread-safe JSON-RPC request ID generator.
pub struct RequestIdGenerator {
    counter: AtomicU64,
}

impl RequestIdGenerator {
    pub fn new() -> Self {
        Self {
            counter: AtomicU64::new(1),
        }
    }

    /// Generate the next request ID.
    pub fn next_id(&self) -> u64 {
        self.counter.fetch_add(1, Ordering::SeqCst)
    }
}

impl Default for RequestIdGenerator {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for RequestIdGenerator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestIdGenerator")
            .field("counter", &self.counter.load(Ordering::SeqCst))
            .finish()
    }
}

/// Build a JSON-RPC 2.0 request.
pub fn build_request(id: u64, method: &str, params: Value) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    })
}

/// Build a JSON-RPC 2.0 notification (no `id` field).
pub fn build_notification(method: &str, params: Value) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": method,
        "params": params,
    })
}

/// Build a JSON-RPC 2.0 response (for replying to server requests).
pub fn build_response(id: Value, result: Value) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    })
}

/// Build a JSON-RPC 2.0 error response.
pub fn build_error_response(id: Value, code: i64, message: &str) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
        },
    })
}

/// Extract error code and message from a JSON-RPC response, if present.
///
/// Returns `Some((code, message))` if the response contains an `error` object,
/// `None` if it is a successful response.
pub fn extract_error(resp: &Value) -> Option<(i64, String)> {
    let err = resp.get("error")?;
    let code = err.get("code").and_then(|v| v.as_i64()).unwrap_or(-1);
    let message = err
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown error")
        .to_string();
    Some((code, message))
}

/// Check if a JSON-RPC message is a response (has `id` and `result` or `error`).
pub fn is_response(msg: &Value) -> bool {
    msg.get("id").is_some() && (msg.get("result").is_some() || msg.get("error").is_some())
}

/// Check if a JSON-RPC message is a request (has `id` and `method`).
pub fn is_request(msg: &Value) -> bool {
    msg.get("id").is_some() && msg.get("method").is_some()
}

/// Check if a JSON-RPC message is a notification (has `method` but no `id`).
pub fn is_notification(msg: &Value) -> bool {
    msg.get("method").is_some() && msg.get("id").is_none()
}

/// Extract the method name from a JSON-RPC request or notification.
pub fn get_method(msg: &Value) -> Option<&str> {
    msg.get("method").and_then(|v| v.as_str())
}

/// Extract the request ID from a JSON-RPC request or response.
pub fn get_id(msg: &Value) -> Option<u64> {
    msg.get("id").and_then(|v| v.as_u64())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_build_request_with_correct_format() {
        let req = build_request(
            1,
            "initialize",
            serde_json::json!({"clientInfo": {"name": "sdk", "version": "1.0"}}),
        );
        assert_eq!(req["jsonrpc"], "2.0");
        assert_eq!(req["id"], 1);
        assert_eq!(req["method"], "initialize");
        assert_eq!(req["params"]["clientInfo"]["name"], "sdk");
        assert_eq!(req["params"]["clientInfo"]["version"], "1.0");
    }

    #[test]
    fn test_should_build_notification_without_id() {
        let notif = build_notification("initialized", serde_json::json!({}));
        assert_eq!(notif["jsonrpc"], "2.0");
        assert_eq!(notif["method"], "initialized");
        assert!(notif.get("id").is_none());
    }

    #[test]
    fn test_should_build_response_with_result() {
        let resp = build_response(serde_json::json!(1), serde_json::json!({"status": "ok"}));
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["result"]["status"], "ok");
    }

    #[test]
    fn test_should_classify_messages_correctly() {
        let request =
            serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "test", "params": {}});
        let notification = serde_json::json!({"jsonrpc": "2.0", "method": "test", "params": {}});
        let response = serde_json::json!({"jsonrpc": "2.0", "id": 1, "result": {}});

        assert!(is_request(&request));
        assert!(!is_notification(&request));
        assert!(!is_response(&request));

        assert!(is_notification(&notification));
        assert!(!is_request(&notification));
        assert!(!is_response(&notification));

        assert!(is_response(&response));
        assert!(!is_notification(&response));
    }

    #[test]
    fn test_should_extract_error_from_error_response() {
        let resp = build_error_response(serde_json::json!(1), -32600, "Invalid request");
        let err = extract_error(&resp);
        assert_eq!(err, Some((-32600, "Invalid request".to_string())));
    }

    #[test]
    fn test_should_return_none_for_success_response() {
        let resp = build_response(serde_json::json!(1), serde_json::json!({"status": "ok"}));
        assert!(extract_error(&resp).is_none());
    }

    #[test]
    fn test_should_generate_sequential_ids() {
        let id_gen = RequestIdGenerator::new();
        assert_eq!(id_gen.next_id(), 1);
        assert_eq!(id_gen.next_id(), 2);
        assert_eq!(id_gen.next_id(), 3);
    }
}
