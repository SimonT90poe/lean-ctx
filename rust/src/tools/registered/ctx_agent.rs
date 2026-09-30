use rmcp::ErrorData;
use rmcp::model::Tool;
use serde_json::{Map, Value, json};

use crate::server::tool_trait::{McpTool, ToolContext, ToolOutput, get_bool, get_str};
use crate::tool_defs::tool_def;

pub struct CtxAgentTool;

impl McpTool for CtxAgentTool {
    fn name(&self) -> &'static str {
        "ctx_agent"
    }

    fn tool_def(&self) -> Tool {
        tool_def(
            "ctx_agent",
            "Research-only local collaboration helper. It is not part of the default LeanCTX Runtime surface or a public agent-orchestration product.\n\
            Enable the session category explicitly before evaluating it.\n\
            Actions: register (agent_type+role), post (message+category), read (poll),\n\
            status (active|idle|finished), handoff (task+summary), sync (agents+messages+scent),\n\
            claim/release (file/task), brief (sub-agent briefing),\n\
            return (distill→knowledge), diary|recall_diary|diaries (agent journal),\n\
            share_knowledge|receive_knowledge (cross-agent), list, info, export, poll_events,\n\
            lease_acquire/lease_release (message=path or symbol:<name>; release takes category=lease_ref).\n\
            Leases are machine-wide: every lean-ctx process sharing the data dir sees the same holder.\n\
            ANTIPATTERN: Do not treat this local helper as a durable workflow or a hosted coordination service.",
            json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": crate::tools::ctx_agent::ACTIONS,
                        "description": crate::tools::ctx_agent::ACTIONS.join("|")
                    },
                    "agent_type": {
                        "type": "string",
                        "description": "cursor|claude|codex|gemini|crush|subagent"
                    },
                    "role": {
                        "type": "string",
                        "description": "dev|review|test|plan"
                    },
                    "message": {
                        "type": "string",
                        "description": "Post text or status detail"
                    },
                    "category": {
                        "type": "string",
                        "description": "finding|warning|request|status"
                    },
                    "to_agent": {
                        "type": "string",
                        "description": "Target agent ID"
                    },
                    "status": {
                        "type": "string",
                        "enum": ["active", "idle", "finished"],
                        "description": "active|idle|finished"
                    },
                    "ttl_hours": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "lease_acquire: 0 = 10 min (default), 1 = 1 h"
                    }
                },
                "allOf": [
                    { "if": { "properties": { "action": { "const": "post" } }, "required": ["action"] }, "then": { "required": ["action", "message"] } },
                    { "if": { "properties": { "action": { "const": "status" } }, "required": ["action"] }, "then": { "required": ["action", "status"] } },
                    { "if": { "properties": { "action": { "const": "handoff" } }, "required": ["action"] }, "then": { "required": ["action", "to_agent"] } },
                    { "if": { "properties": { "action": { "const": "claim" } }, "required": ["action"] }, "then": { "required": ["action", "message"] } },
                    { "if": { "properties": { "action": { "const": "release" } }, "required": ["action"] }, "then": { "required": ["action", "message"] } },
                    { "if": { "properties": { "action": { "const": "brief" } }, "required": ["action"] }, "then": { "required": ["action", "message"] } },
                    { "if": { "properties": { "action": { "const": "return" } }, "required": ["action"] }, "then": { "required": ["action", "message"] } },
                    { "if": { "properties": { "action": { "const": "diary" } }, "required": ["action"] }, "then": { "required": ["action", "message"] } },
                    { "if": { "properties": { "action": { "const": "share_knowledge" } }, "required": ["action"] }, "then": { "required": ["action", "message"] } }
                ],
                "required": ["action"]
            }),
        )
    }

    fn handle(
        &self,
        args: &Map<String, Value>,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, ErrorData> {
        let action = get_str(args, "action")
            .ok_or_else(|| ErrorData::invalid_params("action is required", None))?;
        let agent_type = get_str(args, "agent_type");
        let role = get_str(args, "role");
        let message = get_str(args, "message");
        let category = get_str(args, "category");
        let to_agent = get_str(args, "to_agent");
        let status = get_str(args, "status");
        let privacy = get_str(args, "privacy");
        let priority = get_str(args, "priority");
        let ttl_hours: Option<u64> = args.get("ttl_hours").and_then(serde_json::Value::as_u64);
        let format = get_str(args, "format");
        let write = get_bool(args, "write").unwrap_or(false);
        let filename = get_str(args, "filename");

        let project_root = ctx.project_root.clone();

        let agent_id_handle = ctx.agent_id.as_ref();
        let current_agent_id = agent_id_handle
            .map(|a| a.blocking_read().clone())
            .unwrap_or_default();

        let result = crate::tools::ctx_agent::handle(
            &action,
            agent_type.as_deref(),
            role.as_deref(),
            &project_root,
            current_agent_id.as_deref(),
            message.as_deref(),
            category.as_deref(),
            to_agent.as_deref(),
            status.as_deref(),
            privacy.as_deref(),
            priority.as_deref(),
            ttl_hours,
            format.as_deref(),
            write,
            filename.as_deref(),
        );

        if action == "register" {
            if let Some(id) = result.split(':').nth(1) {
                let id = id.split_whitespace().next().unwrap_or("").to_string();
                if !id.is_empty()
                    && let Some(handle) = agent_id_handle
                {
                    let mut guard = handle.blocking_write();
                    *guard = Some(id);
                }
            }

            let agent_role =
                crate::core::agents::AgentRole::from_str_loose(role.as_deref().unwrap_or("coder"));
            let depth = crate::core::agents::ContextDepthConfig::for_role(agent_role);
            let depth_hint = format!(
                "\n[context] role={:?} preferred_mode={} max_full={} max_sig={} budget_ratio={:.0}%",
                agent_role,
                depth.preferred_mode,
                depth.max_files_full,
                depth.max_files_signatures,
                depth.context_budget_ratio * 100.0,
            );
            return Ok(ToolOutput {
                text: format!("{result}{depth_hint}"),
                original_tokens: 0,
                saved_tokens: 0,
                mode: Some(action),
                path: None,
                changed: false,
                shell_outcome: None,
                content_blocks: None,
            });
        }

        Ok(ToolOutput {
            text: result,
            original_tokens: 0,
            saved_tokens: 0,
            mode: Some(action),
            path: None,
            changed: false,
            shell_outcome: None,
            content_blocks: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::CtxAgentTool;
    use crate::server::tool_trait::McpTool;
    use crate::tools::ctx_agent::ACTIONS;

    /// #1913: a merged `a|b|c` entry advertised an action no client could send.
    #[test]
    fn action_enum_lists_each_dispatched_action_once() {
        let tool = CtxAgentTool.tool_def();
        let schema = serde_json::Value::Object((*tool.input_schema).clone());
        let listed: Vec<&str> = schema["properties"]["action"]["enum"]
            .as_array()
            .expect("action enum")
            .iter()
            .map(|v| v.as_str().expect("string entry"))
            .collect();
        assert_eq!(listed, ACTIONS);
        let mut unique = listed.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), listed.len(), "no duplicate entries");
        for action in &listed {
            assert!(
                action.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
                "enum entry {action:?} must be a single action name"
            );
            let arm = format!("\"{action}\"");
            assert!(
                DISPATCH_SOURCE.lines().any(|line| {
                    let line = line.trim_start();
                    line.starts_with(&arm) && line.contains("=>")
                }),
                "advertised action {action:?} is not dispatched"
            );
        }
    }

    const DISPATCH_SOURCE: &str = include_str!("../ctx_agent.rs");
}
