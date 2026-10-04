//! Odex app-server protocol.
//!
//! JSON-RPC 2.0 over stdio (newline-delimited). The shape follows the
//! engine/UI split of upstream's app-server v2: `thread/*`, `turn/*` and
//! `item/*` notifications, plus server→client requests for approvals.
//!
//! This crate is the single source of truth. TypeScript bindings for the
//! desktop are generated with [`codegen::generate_ts`].

pub mod codegen;
pub mod common;
pub mod config_types;
pub mod ext;
pub mod items;
pub mod jsonrpc;
pub mod methods;
pub mod models;
pub mod notifications;
pub mod registry;
pub mod server_requests;
pub mod workspace;

pub use common::*;
pub use ext::*;
pub use items::*;
pub use methods::*;
pub use models::*;
pub use notifications::*;
pub use registry::{method, notification, server_request};
pub use server_requests::*;
pub use workspace::*;

/// Protocol version. Bump the major on breaking changes; the desktop refuses
/// to talk to an engine with a different major.
pub const PROTOCOL_VERSION: &str = "1.0.0";
pub const SERVER_NAME: &str = "odex-engine";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn item_roundtrip_uses_type_tag() {
        let item = ThreadItem::AgentMessage { id: "i1".into(), text: "hi".into() };
        let v = serde_json::to_value(&item).unwrap();
        assert_eq!(v, json!({"type": "agentMessage", "id": "i1", "text": "hi"}));
        let back: ThreadItem = serde_json::from_value(v).unwrap();
        assert_eq!(back, item);
    }

    #[test]
    fn user_input_variants() {
        let v = json!([{"type":"text","text":"a"},{"type":"localImage","path":"x.png"}]);
        let parsed: Vec<UserInput> = serde_json::from_value(v).unwrap();
        assert_eq!(parsed.len(), 2);
    }

    #[test]
    fn permission_mode_maps() {
        assert_eq!(PermissionMode::Auto.sandbox_mode(), SandboxMode::WorkspaceWrite);
        assert_eq!(PermissionMode::FullAccess.approval_policy(), ApprovalPolicy::Never);
        let v = serde_json::to_value(PermissionMode::FullAccess).unwrap();
        assert_eq!(v, json!("full-access"));
    }

    #[test]
    fn jsonrpc_classifies() {
        use jsonrpc::JsonRpcMessage;
        let r = JsonRpcMessage::parse(r#"{"jsonrpc":"2.0","id":1,"method":"thread/list"}"#).unwrap();
        assert!(matches!(r, JsonRpcMessage::Request(_)));
        let n = JsonRpcMessage::parse(r#"{"jsonrpc":"2.0","method":"item/delta","params":{}}"#).unwrap();
        assert!(matches!(n, JsonRpcMessage::Notification(_)));
        let s = JsonRpcMessage::parse(r#"{"jsonrpc":"2.0","id":"a","result":{}}"#).unwrap();
        assert!(matches!(s, JsonRpcMessage::Response(_)));
    }

    #[test]
    fn method_tables_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for m in registry::CLIENT_REQUESTS
            .iter()
            .chain(registry::SERVER_NOTIFICATIONS)
            .chain(registry::SERVER_REQUESTS)
        {
            assert!(seen.insert(m.method), "duplicate method {}", m.method);
        }
    }

    #[test]
    fn approval_decision_shape() {
        let d: ApprovalDecision = serde_json::from_value(json!({"type":"deny","feedback":"no"})).unwrap();
        assert_eq!(d, ApprovalDecision::Deny { feedback: Some("no".into()) });
        let d: ApprovalDecision = serde_json::from_value(json!({"type":"approveForSession"})).unwrap();
        assert_eq!(d, ApprovalDecision::ApproveForSession);
    }
}
