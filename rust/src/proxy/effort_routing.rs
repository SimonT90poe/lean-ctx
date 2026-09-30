//! Per-turn effort routing (#1148, opt-in dynamic thinking budget).
//!
//! Unlike the static `effort.rs` which applies a constant reasoning level
//! across all turns (for cache stability), this module classifies each turn
//! and adjusts thinking effort dynamically.
//!
//! **Opt-in only** (`proxy.effort_routing = true`). When disabled, the static
//! `effort.rs` path remains the sole controller. When enabled, this module
//! overrides the static level with a per-turn classification.
//!
//! ## Cache stability tradeoff
//!
//! Provider prompt caches (Anthropic `cache_control`, OpenAI prefix caching)
//! break when reasoning parameters change. This module accepts that tradeoff
//! because:
//! 1. Output tokens on Opus-class models cost **5x** input tokens — savings
//!    from reduced thinking often exceed the cache-miss penalty.
//! 2. Routine turns (file reads, passing tests) generate disproportionate
//!    thinking waste for trivial tool-result acknowledgements.
//! 3. The module uses a **two-level** strategy (not N levels) to minimize cache
//!    key diversity: `routine` or `full` — only two cache prefixes to warm.
//!
//! ## Classification
//!
//! A turn is classified as **routine** when the last assistant message was a
//! tool call and the tool result indicates success on a non-complex operation:
//! - File read (tool_use with `ctx_read`, `read_file`, `Read`)
//! - Successful shell command (exit_code == 0, no error indicators)
//! - Search results (grep/glob/find)
//! - Status checks (git status, test passing)
//!
//! A turn is classified as **full** (keep maximum thinking) when:
//! - The user sent a new message (requires understanding intent)
//! - The tool result contains errors/failures
//! - Multiple tool results arrived (complex multi-step)
//! - The content is architecturally complex (refactoring, debugging)

use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::core::config::Effort;

/// Turn classification result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnClass {
    /// Routine tool-result acknowledgement — minimize thinking.
    Routine,
    /// Full complexity — keep maximum thinking effort.
    Full,
}

/// Statistics for monitoring effort routing effectiveness.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct RoutingStats {
    pub routine_count: u64,
    pub full_count: u64,
}

static ESTIMATED_OUTPUT_TOKENS_WITHOUT_ROUTING: AtomicU64 = AtomicU64::new(0);
static ACTUAL_EFFORT_BUDGET_TOKENS: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct EffortSavingsStats {
    pub estimated_output_tokens_without_routing: u64,
    pub actual_effort_budget_tokens: u64,
}

static ROUTINE_COUNT: AtomicU64 = AtomicU64::new(0);
static FULL_COUNT: AtomicU64 = AtomicU64::new(0);

/// Classify the current turn based on the message array.
/// Returns `Routine` if the latest context is a simple tool-result
/// acknowledgement, `Full` otherwise.
pub fn classify_turn(messages: &Value) -> TurnClass {
    let Some(arr) = messages.as_array() else {
        return TurnClass::Full;
    };

    if arr.is_empty() {
        return TurnClass::Full;
    }

    let last = &arr[arr.len() - 1];
    let role = last.get("role").and_then(Value::as_str).unwrap_or("");

    if role == "tool" {
        classify_tool_result(last, arr)
    } else {
        TurnClass::Full
    }
}

/// Classify based on OpenAI Responses API `input` array (different structure).
pub fn classify_turn_responses(input: &Value) -> TurnClass {
    let Some(arr) = input.as_array() else {
        return TurnClass::Full;
    };
    if arr.is_empty() {
        return TurnClass::Full;
    }

    // In Responses API, look for the last item's type.
    let last = &arr[arr.len() - 1];
    let item_type = last.get("type").and_then(Value::as_str).unwrap_or("");

    if item_type == "function_call_output" {
        let output = last.get("output").and_then(Value::as_str).unwrap_or("");
        if is_routine_tool_output(output) {
            return TurnClass::Routine;
        }
    }

    TurnClass::Full
}

/// Classify based on Anthropic messages structure.
pub fn classify_turn_anthropic(messages: &Value) -> TurnClass {
    let Some(arr) = messages.as_array() else {
        return TurnClass::Full;
    };
    if arr.is_empty() {
        return TurnClass::Full;
    }

    let last = &arr[arr.len() - 1];
    let role = last.get("role").and_then(Value::as_str).unwrap_or("");

    if role != "user" {
        return TurnClass::Full;
    }

    // Anthropic puts tool_results in user messages with content array.
    let content = last.get("content");
    if let Some(Value::Array(blocks)) = content {
        let all_tool_results = !blocks.is_empty()
            && blocks
                .iter()
                .all(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"));

        if all_tool_results {
            // Check if any tool result has errors.
            let has_errors = blocks.iter().any(|b| {
                b.get("is_error") == Some(&Value::Bool(true))
                    || b.get("content")
                        .and_then(|c| c.as_str().or_else(|| extract_text_from_content(c)))
                        .is_some_and(contains_error_indicators)
            });

            if has_errors {
                return TurnClass::Full;
            }

            // Multiple tool results → likely complex multi-step.
            if blocks.len() > 3 {
                return TurnClass::Full;
            }

            // Check individual results.
            let all_routine = blocks.iter().all(|b| {
                let text = b
                    .get("content")
                    .and_then(|c| c.as_str().or_else(|| extract_text_from_content(c)))
                    .unwrap_or("");
                is_routine_tool_output(text)
            });

            if all_routine {
                return TurnClass::Routine;
            }
        }
    }

    TurnClass::Full
}

/// Map a turn classification to the effort level to apply.
/// `base` is the operator's configured static effort level.
pub fn effort_for_turn(class: TurnClass, base: Effort) -> Effort {
    match class {
        TurnClass::Routine => {
            ROUTINE_COUNT.fetch_add(1, Ordering::Relaxed);
            // Routine turns get minimal thinking regardless of base.
            Effort::Minimal
        }
        TurnClass::Full => {
            FULL_COUNT.fetch_add(1, Ordering::Relaxed);
            base
        }
    }
}

/// Adjust effort from a classifier intent while preserving the configured
/// effort for intents that are not clearly simple reads or coding work.
pub fn intent_aware_effort(intent: &str, base_effort: Effort) -> Effort {
    let intent = intent.to_ascii_lowercase();
    if [
        "code",
        "coding",
        "fix",
        "implement",
        "refactor",
        "debug",
        "build",
        "patch",
        "test",
    ]
    .iter()
    .any(|term| intent.contains(term))
    {
        Effort::High
    } else if [
        "read",
        "list",
        "show",
        "explain",
        "summarize",
        "status",
        "search",
        "lookup",
    ]
    .iter()
    .any(|term| intent.contains(term))
    {
        Effort::Minimal
    } else {
        base_effort
    }
}

/// Snapshot routing statistics.
pub fn stats() -> RoutingStats {
    RoutingStats {
        routine_count: ROUTINE_COUNT.load(Ordering::Relaxed),
        full_count: FULL_COUNT.load(Ordering::Relaxed),
    }
}

pub fn effort_savings_stats() -> EffortSavingsStats {
    EffortSavingsStats {
        estimated_output_tokens_without_routing: ESTIMATED_OUTPUT_TOKENS_WITHOUT_ROUTING
            .load(Ordering::Relaxed),
        actual_effort_budget_tokens: ACTUAL_EFFORT_BUDGET_TOKENS.load(Ordering::Relaxed),
    }
}

/// Dynamic reasoning budget selected from the current request context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskComplexity {
    pub score: u8,
    pub effort: Effort,
    pub budget_tokens: u32,
}

impl TaskComplexity {
    pub const fn from_score(score: u8) -> Self {
        match score {
            1 => Self {
                score: 1,
                effort: Effort::Low,
                budget_tokens: 1_024,
            },
            2 => Self {
                score: 2,
                effort: Effort::Low,
                budget_tokens: 2_048,
            },
            3 => Self {
                score: 3,
                effort: Effort::Medium,
                budget_tokens: 4_096,
            },
            4 => Self {
                score: 4,
                effort: Effort::High,
                budget_tokens: 8_192,
            },
            _ => Self {
                score: 5,
                effort: Effort::High,
                budget_tokens: 16_384,
            },
        }
    }
}

/// Scores a conversation from its **first** user message only (#1912).
///
/// The injected thinking budget is a top-level request field, and Anthropic
/// invalidates cached message blocks whenever thinking parameters change. The
/// opening request of a session never changes turn to turn, so scoring it pins
/// the budget for the whole conversation instead of letting a growing history
/// (new errors, a longer last turn) move it between tiers.
pub fn score_session_complexity(messages: &[Value]) -> u8 {
    messages
        .iter()
        .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))
        .map_or(1, |first_user| {
            score_complexity(std::slice::from_ref(first_user))
        })
}

/// Scores request complexity on a stable 1–5 scale.
pub fn score_complexity(messages: &[Value]) -> u8 {
    let user_text = messages
        .iter()
        .rev()
        .find(|message| message.get("role").and_then(Value::as_str) == Some("user"))
        .and_then(|message| {
            extract_text_from_content(message.get("content").unwrap_or(&Value::Null))
        })
        .unwrap_or_default();
    let text = user_text.to_ascii_lowercase();
    let file_mentions = count_file_mentions(&text);
    let has_errors = messages.iter().any(message_has_error);
    let critical = ["security", "vulnerability", "production", "incident", "cve"]
        .iter()
        .any(|marker| text.contains(marker));
    let complex = ["debug", "architecture", "refactor", "design"]
        .iter()
        .any(|marker| text.contains(marker));

    if critical {
        5
    } else if has_errors || complex {
        4
    } else if file_mentions >= 2 || user_text.len() > 1_200 || text.contains("test") {
        3
    } else if file_mentions == 1 || user_text.len() > 300 {
        2
    } else {
        1
    }
}

/// Scores and mutates a provider request in one step for proxy handlers.
pub fn route_effort_budget(body: &mut Value) -> TaskComplexity {
    let complexity = body
        .get("messages")
        .or_else(|| body.get("input"))
        .and_then(Value::as_array)
        .map_or(1, |messages| score_session_complexity(messages));
    apply_effort_budget(body, complexity)
}

/// Anthropic's floor for `thinking.budget_tokens`.
const MIN_THINKING_BUDGET: u32 = 1_024;

/// The thinking budget the proxy may inject into an Anthropic request, or
/// `None` when injecting would change the request's meaning or be rejected.
///
/// Anthropic counts thinking against `max_tokens` and requires
/// `budget_tokens < max_tokens`; the budget is capped at half of `max_tokens`
/// so the answer the client sized `max_tokens` for keeps its room. Extended
/// thinking also rejects a non-default `temperature`, `top_k`, a `top_p`
/// below 0.95 and a forced `tool_choice`, and needs every prior assistant turn
/// to carry its thinking blocks — a history produced without thinking gets no
/// injection rather than a 400.
fn anthropic_thinking_budget(
    object: &serde_json::Map<String, Value>,
    task: TaskComplexity,
) -> Option<u32> {
    let max_tokens = object.get("max_tokens").and_then(Value::as_u64)?;
    let half = u32::try_from(max_tokens / 2).unwrap_or(u32::MAX);
    let budget = task.budget_tokens.min(half);
    if budget < MIN_THINKING_BUDGET {
        return None;
    }
    if object
        .get("temperature")
        .and_then(Value::as_f64)
        .is_some_and(|temperature| (temperature - 1.0).abs() > f64::EPSILON)
        || object.contains_key("top_k")
        || object
            .get("top_p")
            .and_then(Value::as_f64)
            .is_some_and(|top_p| top_p < 0.95)
    {
        return None;
    }
    let forced_tool = object
        .get("tool_choice")
        .and_then(|choice| choice.get("type"))
        .and_then(Value::as_str)
        .is_some_and(|kind| matches!(kind, "any" | "tool"));
    if forced_tool {
        return None;
    }
    let assistants = object
        .get("messages")
        .and_then(Value::as_array)
        .map(|messages| {
            messages
                .iter()
                .filter(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let history_has_thinking = assistants.iter().any(|message| {
        message
            .get("content")
            .and_then(Value::as_array)
            .is_some_and(|blocks| {
                blocks.iter().any(|block| {
                    matches!(
                        block.get("type").and_then(Value::as_str),
                        Some("thinking" | "redacted_thinking")
                    )
                })
            })
    });
    if !assistants.is_empty() && !history_has_thinking {
        return None;
    }
    Some(budget)
}

/// Whether an Anthropic model accepts the `thinking` parameter with a fixed
/// `budget_tokens`, the form this module injects.
///
/// The list is deliberately narrow and fails **closed**: unknown models get no
/// injection rather than risk a 400 from a model whose capabilities we guessed
/// wrong. Ineligible families:
/// - `claude-3-opus`/`claude-3-sonnet`/`claude-3-haiku`/`claude-3-5-haiku` have
///   no extended thinking at all.
/// - Opus/Sonnet 4.6+ and the 5-generation models reject `budget_tokens` (400);
///   they take `thinking: {type: "adaptive"}` plus `output_config.effort`.
fn anthropic_supports_thinking(model: &str) -> bool {
    let bare = model.rsplit('/').next().unwrap_or(model);
    let budget_token_families = [
        "claude-3-5-sonnet",
        "claude-3-7-sonnet",
        "claude-3.5-sonnet",
        "claude-3.7-sonnet",
        "claude-sonnet-4-5",
        "claude-opus-4-5",
        "claude-haiku-4-5",
        "claude-4-5",           // alias form (claude-4-5-sonnet / -haiku)
        "claude-sonnet-4-2025", // dated snapshot (claude-sonnet-4-20250514)
        "claude-opus-4-2025",   // dated snapshot (claude-opus-4-20250514)
    ];
    budget_token_families
        .iter()
        .any(|family| bare.starts_with(family))
        // Bare launch-model ids that predate the 4.6+ adaptive-only era.
        || bare == "claude-sonnet-4"
        || bare == "claude-opus-4"
}

/// Calculates complexity for a request and applies its provider-native effort budget.
pub fn apply_effort_budget(body: &mut Value, complexity: u8) -> TaskComplexity {
    inject_effort_budget(body, complexity).0
}

/// Applies the provider-native effort budget and reports whether the body was
/// changed, so callers can forward the client's original bytes when it was
/// not (#1912).
///
/// A value the client already chose — `thinking`, `reasoning_effort` or
/// `reasoning.effort` — is never overwritten.
pub(crate) fn inject_effort_budget(body: &mut Value, complexity: u8) -> (TaskComplexity, bool) {
    let task = TaskComplexity::from_score(complexity);
    let Some(object) = body.as_object_mut() else {
        return (task, false);
    };

    let model = object
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let is_anthropic = object.contains_key("messages")
        && (object.contains_key("max_tokens") || model.contains("claude"));
    let mut injected_budget = None;
    if is_anthropic && anthropic_supports_thinking(&model) {
        if object.get("thinking").is_none() {
            if let Some(budget) = anthropic_thinking_budget(object, task) {
                object.insert(
                    "thinking".into(),
                    serde_json::json!({"type": "enabled", "budget_tokens": budget}),
                );
                injected_budget = Some(budget);
            }
        }
    } else if crate::proxy::effort::openai_supports_effort(&model) {
        let client_chose = object.contains_key("reasoning_effort")
            || object
                .get("reasoning")
                .is_some_and(|reasoning| reasoning.get("effort").is_some());
        if !client_chose {
            let effort = Value::String(openai_effort_value(task.effort).into());
            if object.contains_key("messages") {
                // Chat Completions takes the flat `reasoning_effort` field.
                object.insert("reasoning_effort".into(), effort);
                injected_budget = Some(task.budget_tokens);
            } else if object.contains_key("input") {
                // The Responses API nests it under `reasoning`.
                let reasoning = object
                    .entry("reasoning")
                    .or_insert_with(|| Value::Object(serde_json::Map::new()));
                if let Some(reasoning) = reasoning.as_object_mut() {
                    reasoning.insert("effort".into(), effort);
                    injected_budget = Some(task.budget_tokens);
                }
            }
        }
    }
    let Some(budget) = injected_budget else {
        return (task, false);
    };
    ESTIMATED_OUTPUT_TOKENS_WITHOUT_ROUTING.fetch_add(16_384, Ordering::Relaxed);
    ACTUAL_EFFORT_BUDGET_TOKENS.fetch_add(budget.into(), Ordering::Relaxed);
    (task, true)
}

fn openai_effort_value(effort: Effort) -> &'static str {
    match effort {
        Effort::Minimal | Effort::Low => "low",
        Effort::Medium => "medium",
        Effort::High => "high",
    }
}

#[cfg(test)]
mod output_token_intelligence_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn trivial_task_gets_low_effort() {
        let messages = vec![json!({"role": "user", "content": "Read src/lib.rs"})];
        assert_eq!(score_complexity(&messages), 2);
        assert_eq!(TaskComplexity::from_score(1).budget_tokens, 1_024);
    }

    #[test]
    fn complex_task_gets_high_effort() {
        let messages =
            vec![json!({"role": "user", "content": "Design the architecture for this refactor"})];
        let task = TaskComplexity::from_score(score_complexity(&messages));
        assert_eq!(task.score, 4);
        assert_eq!(task.effort, Effort::High);
    }

    #[test]
    fn error_presence_increases_complexity() {
        let messages = vec![
            json!({"role": "tool", "content": "error[E0308]: mismatched types"}),
            json!({"role": "user", "content": "fix this"}),
        ];
        assert_eq!(score_complexity(&messages), 4);
    }

    #[test]
    fn applies_anthropic_thinking_budget() {
        let mut body = json!({"model": "claude-sonnet-4", "max_tokens": 32_000, "messages": []});
        let task = apply_effort_budget(&mut body, 4);
        assert_eq!(task.budget_tokens, 8_192);
        assert_eq!(body["thinking"]["budget_tokens"], 8_192);
        assert_eq!(body["thinking"]["type"], "enabled");
    }

    #[test]
    fn thinking_budget_stays_below_max_tokens() {
        // #1912: Anthropic requires budget_tokens < max_tokens, and thinking
        // eats into max_tokens — cap at half so the answer keeps its room.
        let mut body = json!({"model": "claude-sonnet-4-5", "max_tokens": 4096, "messages": []});
        let (_, changed) = inject_effort_budget(&mut body, 5);
        assert!(changed);
        let budget = body["thinking"]["budget_tokens"].as_u64().unwrap();
        assert_eq!(budget, 2_048);
        assert!(budget < 4096);
    }

    #[test]
    fn small_max_tokens_gets_no_thinking() {
        // Half of 1024 is below Anthropic's 1024 floor; injecting would 400.
        for max_tokens in [1_024, 2_047] {
            let mut body =
                json!({"model": "claude-sonnet-4-5", "max_tokens": max_tokens, "messages": []});
            let (_, changed) = inject_effort_budget(&mut body, 5);
            assert!(!changed, "max_tokens {max_tokens} must not get thinking");
            assert!(body.get("thinking").is_none());
        }
        let mut body = json!({"model": "claude-sonnet-4-5", "messages": []});
        assert!(
            !inject_effort_budget(&mut body, 5).1,
            "no max_tokens → no injection"
        );
    }

    #[test]
    fn skips_thinking_when_sampling_or_tool_choice_conflicts() {
        let base = json!({"model": "claude-sonnet-4-5", "max_tokens": 32_000, "messages": []});
        for (key, value) in [
            ("temperature", json!(0.2)),
            ("top_k", json!(40)),
            ("top_p", json!(0.5)),
            ("tool_choice", json!({"type": "any"})),
            ("tool_choice", json!({"type": "tool", "name": "x"})),
        ] {
            let mut body = base.clone();
            body[key] = value.clone();
            let (_, changed) = inject_effort_budget(&mut body, 4);
            assert!(!changed, "{key}={value} must block thinking");
            assert!(body.get("thinking").is_none());
        }
        let mut body = base.clone();
        body["temperature"] = json!(1.0);
        body["tool_choice"] = json!({"type": "auto"});
        assert!(
            inject_effort_budget(&mut body, 4).1,
            "defaults must still inject"
        );
    }

    #[test]
    fn skips_thinking_when_history_has_no_thinking_blocks() {
        // Enabling thinking mid-conversation on a history produced without
        // it would be rejected; a history that carries thinking is fine.
        let mut body = json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 32_000,
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": [{"type": "text", "text": "hello"}]},
                {"role": "user", "content": "debug this"}
            ]
        });
        assert!(!inject_effort_budget(&mut body, 4).1);
        assert!(body.get("thinking").is_none());

        body["messages"][1]["content"] = json!([
            {"type": "thinking", "thinking": "…", "signature": "sig"},
            {"type": "text", "text": "hello"}
        ]);
        assert!(inject_effort_budget(&mut body, 4).1);
    }

    #[test]
    fn session_score_is_stable_across_turns() {
        // #1912: the budget must not move as the conversation grows, or every
        // turn invalidates Anthropic's message cache.
        let first = json!({"role": "user", "content": "rename a variable"});
        let turn_one = vec![first.clone()];
        let turn_two = vec![
            first,
            json!({"role": "assistant", "content": "done"}),
            json!({"role": "tool", "content": "error[E0308]: mismatched types"}),
            json!({"role": "user", "content": "debug the production security incident"}),
        ];
        assert_eq!(
            score_session_complexity(&turn_one),
            score_session_complexity(&turn_two)
        );
        assert_ne!(score_complexity(&turn_one), score_complexity(&turn_two));
        assert_eq!(score_session_complexity(&[]), 1);
    }

    #[test]
    fn skips_thinking_for_unsupported_models() {
        let mut body = json!({"model": "claude-sonnet-5", "max_tokens": 4096, "messages": []});
        let task = apply_effort_budget(&mut body, 4);
        assert_eq!(task.budget_tokens, 8_192);
        assert!(
            body.get("thinking").is_none(),
            "sonnet-5 must not get thinking params"
        );
    }

    #[test]
    fn skips_thinking_for_openrouter_wrapped_unsupported() {
        let mut body =
            json!({"model": "anthropic/claude-sonnet-5", "max_tokens": 4096, "messages": []});
        apply_effort_budget(&mut body, 4);
        assert!(
            body.get("thinking").is_none(),
            "openrouter-wrapped sonnet-5 must not get thinking"
        );
    }

    #[test]
    fn preserves_client_thinking_params() {
        let mut body = json!({
            "model": "claude-sonnet-4",
            "thinking": {"type": "adaptive"},
            "messages": []
        });
        apply_effort_budget(&mut body, 4);
        assert_eq!(
            body["thinking"]["type"], "adaptive",
            "must not overwrite client thinking"
        );
        assert!(
            body["thinking"].get("budget_tokens").is_none(),
            "must not inject budget_tokens"
        );
    }

    #[test]
    fn skips_thinking_for_adaptive_only_models() {
        // Opus/Sonnet 4.6+ and the 5-generation reject `budget_tokens` with a
        // 400 — never inject, even though these models do support thinking.
        for model in [
            "claude-opus-4-6",
            "claude-opus-4-7",
            "claude-opus-4-8",
            "claude-sonnet-4-6",
            "claude-sonnet-5",
            "claude-opus-5",
            "claude-fable-5",
        ] {
            let mut body = json!({"model": model, "max_tokens": 4096, "messages": []});
            apply_effort_budget(&mut body, 4);
            assert!(
                body.get("thinking").is_none(),
                "{model} must not get thinking params"
            );
        }
    }

    #[test]
    fn skips_thinking_for_thinkingless_claude3() {
        // The 2024 claude-3 series has no extended thinking at all.
        for model in [
            "claude-3-opus-20240229",
            "claude-3-sonnet-20240229",
            "claude-3-haiku-20240307",
            "claude-3-5-haiku",
        ] {
            let mut body = json!({"model": model, "max_tokens": 4096, "messages": []});
            apply_effort_budget(&mut body, 4);
            assert!(
                body.get("thinking").is_none(),
                "{model} must not get thinking params"
            );
        }
    }

    #[test]
    fn applies_thinking_to_budget_token_era() {
        // claude-3-5/3-7 sonnet and the 4-5 era still accept budget_tokens.
        for model in [
            "claude-3-5-sonnet",
            "claude-3-7-sonnet",
            "anthropic/claude-3.5-sonnet",
            "claude-sonnet-4-5",
            "claude-opus-4-5",
            "claude-haiku-4-5",
            "claude-sonnet-4-20250514",
            "claude-opus-4-20250514",
        ] {
            let mut body = json!({"model": model, "max_tokens": 32_000, "messages": []});
            apply_effort_budget(&mut body, 4);
            assert_eq!(
                body["thinking"]["budget_tokens"], 8_192,
                "{model} must get a thinking budget"
            );
        }
    }

    #[test]
    fn applies_openai_reasoning_effort() {
        // Chat Completions takes the flat field; `reasoning` is Responses-only.
        let mut body = json!({"model": "gpt-5.4", "messages": []});
        assert!(inject_effort_budget(&mut body, 3).1);
        assert_eq!(body["reasoning_effort"], "medium");
        assert!(body.get("reasoning").is_none());

        let mut body = json!({"model": "gpt-5.4", "input": []});
        assert!(inject_effort_budget(&mut body, 3).1);
        assert_eq!(body["reasoning"]["effort"], "medium");
        assert!(body.get("reasoning_effort").is_none());
    }

    #[test]
    fn keeps_client_openai_effort() {
        // #1912: a client-chosen effort is the client's decision.
        let mut body = json!({"model": "gpt-5.4", "messages": [], "reasoning_effort": "high"});
        let before = body.clone();
        assert!(!inject_effort_budget(&mut body, 1).1);
        assert_eq!(body, before);

        let mut body = json!({"model": "gpt-5.4", "input": [], "reasoning": {"effort": "minimal"}});
        let before = body.clone();
        assert!(!inject_effort_budget(&mut body, 5).1);
        assert_eq!(body, before);
    }
}

fn count_file_mentions(text: &str) -> usize {
    text.split_whitespace()
        .filter(|word| {
            let word = word.trim_matches(|character: char| {
                !character.is_ascii_alphanumeric()
                    && character != '.'
                    && character != '_'
                    && character != '/'
            });
            word.contains('/')
                || [".rs", ".toml", ".json", ".md", ".py", ".ts", ".js"]
                    .iter()
                    .any(|suffix| word.ends_with(suffix))
        })
        .count()
}

fn message_has_error(message: &Value) -> bool {
    message.get("is_error").and_then(Value::as_bool) == Some(true)
        || extract_text_from_content(message.get("content").unwrap_or(&Value::Null))
            .is_some_and(contains_error_indicators)
}

// ---------------------------------------------------------------------------
// Internal classification helpers
// ---------------------------------------------------------------------------

fn classify_tool_result(msg: &Value, _all_messages: &[Value]) -> TurnClass {
    let content = msg.get("content").and_then(Value::as_str).unwrap_or("");

    if contains_error_indicators(content) {
        return TurnClass::Full;
    }

    if is_routine_tool_output(content) {
        return TurnClass::Routine;
    }

    TurnClass::Full
}

/// Heuristic: does this tool output look like a routine, successful result?
fn is_routine_tool_output(content: &str) -> bool {
    if content.is_empty() || content.len() < 10 {
        return false;
    }

    // Error indicators → not routine.
    if contains_error_indicators(content) {
        return false;
    }

    // Very large outputs (>8000 chars) likely need careful processing.
    if content.len() > 8000 {
        return false;
    }

    // Positive signals for routine:
    let routine_signals = [
        // File read results (lean-ctx or native).
        "deps ",      // lean-ctx read header
        "[unchanged", // cached re-read
        "[lean-ctx]", // lean-ctx footer
        "lines:",     // line count indicators
        // Shell success patterns.
        "exit_code: 0",
        "Command completed",
        "0 errors",
        "All tests passed",
        "no changes",
        "nothing to commit",
        "Already up to date",
        "Build succeeded",
        // Search results.
        "matches in",
        "0 matches",
    ];

    routine_signals.iter().any(|sig| content.contains(sig))
}

/// Check if content contains error/failure indicators that need full thinking.
fn contains_error_indicators(content: &str) -> bool {
    let lower = content.to_ascii_lowercase();
    let indicators = [
        "error",
        "failed",
        "failure",
        "fatal",
        "panic",
        "exception",
        "traceback",
        "stack trace",
        "segfault",
        "abort",
        "denied",
        "permission",
        "not found",
        "timed out",
        "exit_code: 1",
        "exit code 1",
        "compilation error",
        "syntax error",
        "type error",
    ];

    indicators.iter().any(|ind| lower.contains(ind))
}

/// Extract text from Anthropic content blocks.
fn extract_text_from_content(content: &Value) -> Option<&str> {
    if let Some(s) = content.as_str() {
        return Some(s);
    }
    if let Some(arr) = content.as_array() {
        for block in arr {
            if block.get("type").and_then(Value::as_str) == Some("text") {
                return block.get("text").and_then(Value::as_str);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn user_message_is_always_full() {
        let messages = json!([
            {"role": "user", "content": "What does this function do?"}
        ]);
        assert_eq!(classify_turn(&messages), TurnClass::Full);
    }

    #[test]
    fn successful_file_read_is_routine() {
        let messages = json!([
            {"role": "assistant", "content": "Let me read that file."},
            {"role": "tool", "content": "main.rs 50L\n  deps serde\n[lean-ctx] full source: ..."}
        ]);
        assert_eq!(classify_turn(&messages), TurnClass::Routine);
    }

    #[test]
    fn error_tool_result_is_full() {
        let messages = json!([
            {"role": "tool", "content": "error[E0308]: mismatched types\n  --> src/main.rs:5:12"}
        ]);
        assert_eq!(classify_turn(&messages), TurnClass::Full);
    }

    #[test]
    fn successful_shell_is_routine() {
        let messages = json!([
            {"role": "tool", "content": "Command completed in 150ms\nexit_code: 0\nAll tests passed"}
        ]);
        assert_eq!(classify_turn(&messages), TurnClass::Routine);
    }

    #[test]
    fn anthropic_tool_result_routine() {
        let messages = json!([
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "abc", "content": "[unchanged 5L]\n[lean-ctx] cached"}
            ]}
        ]);
        assert_eq!(classify_turn_anthropic(&messages), TurnClass::Routine);
    }

    #[test]
    fn anthropic_tool_result_with_error() {
        let messages = json!([
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "abc", "is_error": true, "content": "Tool failed"}
            ]}
        ]);
        assert_eq!(classify_turn_anthropic(&messages), TurnClass::Full);
    }

    #[test]
    fn effort_mapping() {
        assert_eq!(
            effort_for_turn(TurnClass::Routine, Effort::High),
            Effort::Minimal
        );
        assert_eq!(effort_for_turn(TurnClass::Full, Effort::High), Effort::High);
        assert_eq!(
            effort_for_turn(TurnClass::Full, Effort::Medium),
            Effort::Medium
        );
    }

    #[test]
    fn intent_adjusts_effort_by_task_complexity() {
        assert_eq!(
            intent_aware_effort("fix the parser", Effort::Low),
            Effort::High
        );
        assert_eq!(
            intent_aware_effort("read the config", Effort::High),
            Effort::Minimal
        );
        assert_eq!(intent_aware_effort("chat", Effort::Medium), Effort::Medium);
    }

    #[test]
    fn empty_messages_is_full() {
        assert_eq!(classify_turn(&json!([])), TurnClass::Full);
        assert_eq!(classify_turn(&json!(null)), TurnClass::Full);
    }

    #[test]
    fn deterministic_classification() {
        let messages = json!([
            {"role": "tool", "content": "Build succeeded\nexit_code: 0\nCommand completed in 2s"}
        ]);
        let c1 = classify_turn(&messages);
        let c2 = classify_turn(&messages);
        assert_eq!(c1, c2, "classification must be deterministic");
    }
}
