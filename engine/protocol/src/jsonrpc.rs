//! JSON-RPC 2.0 envelope types.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

pub const JSONRPC_VERSION: &str = "2.0";

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(untagged)]
pub enum RequestId {
    Num(#[ts(type = "number")] i64),
    Str(String),
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestId::Num(n) => write!(f, "{n}"),
            RequestId::Str(s) => write!(f, "{s}"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: RequestId,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub params: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub params: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct JsonRpcError {
    #[ts(type = "number")]
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub data: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, TS)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: RequestId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub error: Option<JsonRpcError>,
}

/// Any JSON-RPC message on the wire.
#[derive(Debug, Clone)]
pub enum JsonRpcMessage {
    Request(JsonRpcRequest),
    Notification(JsonRpcNotification),
    Response(JsonRpcResponse),
}

impl JsonRpcMessage {
    /// Parse one line/frame into a message, classifying by the fields present.
    pub fn parse(text: &str) -> Result<Self, serde_json::Error> {
        let v: Value = serde_json::from_str(text)?;
        Self::from_value(v)
    }

    pub fn from_value(v: Value) -> Result<Self, serde_json::Error> {
        let has_method = v.get("method").is_some();
        let has_id = v.get("id").map(|i| !i.is_null()).unwrap_or(false);
        if has_method && has_id {
            Ok(JsonRpcMessage::Request(serde_json::from_value(v)?))
        } else if has_method {
            Ok(JsonRpcMessage::Notification(serde_json::from_value(v)?))
        } else {
            Ok(JsonRpcMessage::Response(serde_json::from_value(v)?))
        }
    }

    pub fn to_line(&self) -> String {
        let s = match self {
            JsonRpcMessage::Request(r) => serde_json::to_string(r),
            JsonRpcMessage::Notification(n) => serde_json::to_string(n),
            JsonRpcMessage::Response(r) => serde_json::to_string(r),
        };
        s.unwrap_or_else(|_| "{}".to_string())
    }
}

impl JsonRpcRequest {
    pub fn new(id: RequestId, method: impl Into<String>, params: Option<Value>) -> Self {
        Self { jsonrpc: JSONRPC_VERSION.into(), id, method: method.into(), params }
    }
}

impl JsonRpcNotification {
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self { jsonrpc: JSONRPC_VERSION.into(), method: method.into(), params }
    }
}

impl JsonRpcResponse {
    pub fn ok(id: RequestId, result: Value) -> Self {
        Self { jsonrpc: JSONRPC_VERSION.into(), id, result: Some(result), error: None }
    }
    pub fn err(id: RequestId, code: i64, message: impl Into<String>, data: Option<Value>) -> Self {
        Self {
            jsonrpc: JSONRPC_VERSION.into(),
            id,
            result: None,
            error: Some(JsonRpcError { code, message: message.into(), data }),
        }
    }
}

/// Standard and Odex-specific error codes.
pub mod error_codes {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
    /// Request arrived before `initialize`.
    pub const NOT_INITIALIZED: i64 = -32002;
    pub const THREAD_NOT_FOUND: i64 = -32010;
    pub const THREAD_BUSY: i64 = -32011;
    pub const NOT_TRUSTED: i64 = -32012;
    pub const MODEL_UNAVAILABLE: i64 = -32020;
    pub const GIT_ERROR: i64 = -32030;
}
