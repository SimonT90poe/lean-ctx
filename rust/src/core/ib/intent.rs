//! Task-intent classification from session metadata and findings.
//!
//! Maps agent task descriptions to [`TaskIntent`] categories used by
//! information-bottleneck compression to select query terms.

use std::fmt;

use crate::core::session::SessionState;

/// Task intent categories derived from cognitive task analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TaskIntent {
    /// Diagnose and correct erroneous behavior.
    Debug,
    /// Restructure existing code without changing its behavior.
    Refactor,
    /// Add or build new behavior.
    Implement,
    /// Assess code for correctness or quality.
    Review,
    /// Investigate an unfamiliar codebase or problem.
    Explore,
    /// No intent signal was found.
    #[default]
    Unknown,
}

const INTENT_KEYWORDS: &[(TaskIntent, &[&str])] = &[
    (
        TaskIntent::Debug,
        &["fix", "bug", "error", "debug", "crash", "panic"],
    ),
    (
        TaskIntent::Refactor,
        &[
            "refactor",
            "rename",
            "move",
            "extract",
            "restructure",
            "clean",
        ],
    ),
    (
        TaskIntent::Implement,
        &["implement", "add", "create", "build", "feature", "new"],
    ),
    (
        TaskIntent::Review,
        &["review", "check", "audit", "verify", "inspect"],
    ),
    (
        TaskIntent::Explore,
        &["understand", "explore", "analyze", "investigate", "find"],
    ),
];

const ERROR_KEYWORDS: &[&str] = &["error", "panic", "crash", "failed", "failure", "bug"];

/// Classify the agent's current task intent from session state.
///
/// Task metadata has priority over findings; error-bearing findings supply a
/// debug signal when the task itself does not identify an intent.
pub fn classify_intent(session: &SessionState) -> TaskIntent {
    if let Some(task) = &session.task {
        if let Some(intent) = classify_text(task.intent.as_deref().unwrap_or_default()) {
            return intent;
        }
        if let Some(intent) = classify_text(&task.description) {
            return intent;
        }
    }

    if session
        .findings
        .iter()
        .rev()
        .any(|finding| contains_keyword(&finding.summary, ERROR_KEYWORDS))
    {
        return TaskIntent::Debug;
    }

    TaskIntent::Unknown
}

pub(crate) fn classify_text(text: &str) -> Option<TaskIntent> {
    INTENT_KEYWORDS
        .iter()
        .find_map(|(intent, keywords)| contains_keyword(text, keywords).then_some(*intent))
}

fn contains_keyword(text: &str, keywords: &[&str]) -> bool {
    text.split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .any(|word| {
            keywords
                .iter()
                .any(|keyword| word.eq_ignore_ascii_case(keyword))
        })
}

impl fmt::Display for TaskIntent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Debug => "debug",
            Self::Refactor => "refactor",
            Self::Implement => "implement",
            Self::Review => "review",
            Self::Explore => "explore",
            Self::Unknown => "unknown",
        };
        formatter.write_str(name)
    }
}

#[cfg(test)]
pub mod tests {
    use crate::core::session::{SessionState, TaskInfo};

    use super::{TaskIntent, classify_intent};

    fn session_with_task(description: &str) -> SessionState {
        let mut session = SessionState::new();
        session.task = Some(TaskInfo {
            description: description.to_owned(),
            intent: None,
            progress_pct: None,
        });
        session
    }

    #[test]
    fn classify_debug_from_task() {
        let session = session_with_task("Fix the parser crash");
        assert_eq!(classify_intent(&session), TaskIntent::Debug);
    }

    #[test]
    fn classify_refactor_from_task() {
        let session = session_with_task("Refactor the session store");
        assert_eq!(classify_intent(&session), TaskIntent::Refactor);
    }

    #[test]
    fn classify_unknown_when_empty() {
        assert_eq!(classify_intent(&SessionState::new()), TaskIntent::Unknown);
    }

    #[test]
    fn display_format() {
        assert_eq!(TaskIntent::Implement.to_string(), "implement");
    }
}
