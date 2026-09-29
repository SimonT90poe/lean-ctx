use super::types::{
    BudgetConfig, CapabilitiesConfig, CompressionConfig, ConstraintsConfig, DegradationConfig,
    LayoutConfig, OutputHints, PipelineConfig, Profile, ProfileAutonomy, ProfileMeta, ReadConfig,
    RoutingConfig, TranslationConfig,
};
use std::collections::HashMap;

// ── Built-in Profiles ──────────────────────────────────────

pub(super) fn builtin_coder() -> Profile {
    Profile {
        profile: ProfileMeta {
            name: "coder".to_string(),
            inherits: None,
            description: "Default coding workflow with guarded autonomy drivers".to_string(),
        },
        read: ReadConfig {
            default_mode: Some("auto".to_string()),
            max_tokens_per_file: Some(50_000),
            prefer_cache: Some(true),
        },
        compression: CompressionConfig {
            crp_mode: None,
            output_density: Some("terse".to_string()),
            terse_mode: Some(true),
            ..CompressionConfig::default()
        },
        translation: TranslationConfig {
            enabled: Some(true),
            ruleset: Some("auto".to_string()),
        },
        layout: LayoutConfig::default(),
        memory: crate::core::memory_policy::MemoryPolicyOverrides::default(),
        verification: crate::core::output_verification::VerificationConfig::default(),
        budget: BudgetConfig {
            max_context_tokens: Some(150_000),
            max_shell_invocations: Some(100),
            ..BudgetConfig::default()
        },
        constraints: ConstraintsConfig {
            quality_floor: Some(0.95),
            ..ConstraintsConfig::default()
        },
        capabilities: CapabilitiesConfig::default(),
        pipeline: PipelineConfig::default(),
        routing: RoutingConfig::default(),
        degradation: DegradationConfig::default(),
        autonomy: ProfileAutonomy {
            auto_prefetch: Some(true),
            auto_response: Some(true),
            checkpoint_interval: Some(10),
            ..ProfileAutonomy::default()
        },
        output_hints: OutputHints::default(),
    }
}

pub(super) fn builtin_exploration() -> Profile {
    Profile {
        profile: ProfileMeta {
            name: "exploration".to_string(),
            inherits: None,
            description: "Broad context for understanding codebases".to_string(),
        },
        read: ReadConfig {
            default_mode: Some("map".to_string()),
            max_tokens_per_file: Some(80_000),
            prefer_cache: Some(true),
        },
        compression: CompressionConfig {
            terse_mode: Some(true),
            output_density: Some("terse".to_string()),
            ..CompressionConfig::default()
        },
        translation: TranslationConfig::default(),
        layout: LayoutConfig::default(),
        memory: crate::core::memory_policy::MemoryPolicyOverrides::default(),
        verification: crate::core::output_verification::VerificationConfig::default(),
        budget: BudgetConfig {
            max_context_tokens: Some(200_000),
            ..BudgetConfig::default()
        },
        constraints: ConstraintsConfig {
            quality_floor: Some(0.90),
            ..ConstraintsConfig::default()
        },
        capabilities: CapabilitiesConfig::default(),
        pipeline: PipelineConfig::default(),
        routing: RoutingConfig::default(),
        degradation: DegradationConfig::default(),
        autonomy: ProfileAutonomy::default(),
        output_hints: OutputHints {
            related_hint: Some(true),
            compressed_hint: Some(true),
            cross_source_hint: Some(true),
            proactive_context: Some(true),
            ..OutputHints::default()
        },
    }
}

fn builtin_bugfix() -> Profile {
    Profile {
        profile: ProfileMeta {
            name: "bugfix".to_string(),
            inherits: None,
            description: "Focused context for debugging specific issues".to_string(),
        },
        read: ReadConfig {
            default_mode: Some("auto".to_string()),
            max_tokens_per_file: Some(30_000),
            prefer_cache: Some(false),
        },
        compression: CompressionConfig {
            crp_mode: Some("tdd".to_string()),
            output_density: Some("terse".to_string()),
            ..CompressionConfig::default()
        },
        translation: TranslationConfig::default(),
        layout: LayoutConfig::default(),
        memory: crate::core::memory_policy::MemoryPolicyOverrides::default(),
        verification: crate::core::output_verification::VerificationConfig::default(),
        budget: BudgetConfig {
            max_context_tokens: Some(100_000),
            max_shell_invocations: Some(50),
            ..BudgetConfig::default()
        },
        constraints: ConstraintsConfig::default(),
        capabilities: CapabilitiesConfig::default(),
        pipeline: PipelineConfig::default(),
        routing: RoutingConfig {
            max_model_tier: Some("standard".to_string()),
            ..RoutingConfig::default()
        },
        degradation: DegradationConfig::default(),
        autonomy: ProfileAutonomy {
            checkpoint_interval: Some(10),
            ..ProfileAutonomy::default()
        },
        output_hints: OutputHints::default(),
    }
}

fn builtin_hotfix() -> Profile {
    Profile {
        profile: ProfileMeta {
            name: "hotfix".to_string(),
            inherits: None,
            description: "Minimal context, fast iteration for urgent fixes".to_string(),
        },
        read: ReadConfig {
            default_mode: Some("signatures".to_string()),
            max_tokens_per_file: Some(2_000),
            prefer_cache: Some(true),
        },
        compression: CompressionConfig {
            crp_mode: Some("tdd".to_string()),
            output_density: Some("ultra".to_string()),
            ..CompressionConfig::default()
        },
        translation: TranslationConfig::default(),
        layout: LayoutConfig::default(),
        memory: crate::core::memory_policy::MemoryPolicyOverrides::default(),
        verification: crate::core::output_verification::VerificationConfig::default(),
        budget: BudgetConfig {
            max_context_tokens: Some(30_000),
            max_shell_invocations: Some(20),
            max_cost_usd: Some(1.0),
        },
        constraints: ConstraintsConfig::default(),
        capabilities: CapabilitiesConfig::default(),
        pipeline: PipelineConfig::default(),
        routing: RoutingConfig {
            max_model_tier: Some("fast".to_string()),
            ..RoutingConfig::default()
        },
        degradation: DegradationConfig::default(),
        autonomy: ProfileAutonomy {
            checkpoint_interval: Some(5),
            ..ProfileAutonomy::default()
        },
        output_hints: OutputHints::default(),
    }
}

fn builtin_ci_debug() -> Profile {
    Profile {
        profile: ProfileMeta {
            name: "ci-debug".to_string(),
            inherits: None,
            description: "CI/CD debugging with shell-heavy workflows".to_string(),
        },
        read: ReadConfig {
            default_mode: Some("auto".to_string()),
            max_tokens_per_file: Some(50_000),
            prefer_cache: Some(false),
        },
        compression: CompressionConfig {
            output_density: Some("terse".to_string()),
            ..CompressionConfig::default()
        },
        translation: TranslationConfig::default(),
        layout: LayoutConfig::default(),
        memory: crate::core::memory_policy::MemoryPolicyOverrides::default(),
        verification: crate::core::output_verification::VerificationConfig::default(),
        budget: BudgetConfig {
            max_context_tokens: Some(150_000),
            max_shell_invocations: Some(200),
            ..BudgetConfig::default()
        },
        constraints: ConstraintsConfig::default(),
        capabilities: CapabilitiesConfig::default(),
        pipeline: PipelineConfig::default(),
        routing: RoutingConfig {
            max_model_tier: Some("standard".to_string()),
            ..RoutingConfig::default()
        },
        degradation: DegradationConfig::default(),
        autonomy: ProfileAutonomy::default(),
        output_hints: OutputHints::default(),
    }
}

fn builtin_review() -> Profile {
    Profile {
        profile: ProfileMeta {
            name: "review".to_string(),
            inherits: None,
            description: "Code review with broad read-only context".to_string(),
        },
        read: ReadConfig {
            default_mode: Some("map".to_string()),
            max_tokens_per_file: Some(60_000),
            prefer_cache: Some(true),
        },
        compression: CompressionConfig {
            crp_mode: Some("compact".to_string()),
            ..CompressionConfig::default()
        },
        translation: TranslationConfig::default(),
        layout: LayoutConfig::default(),
        memory: crate::core::memory_policy::MemoryPolicyOverrides::default(),
        verification: crate::core::output_verification::VerificationConfig::default(),
        budget: BudgetConfig {
            max_context_tokens: Some(150_000),
            max_shell_invocations: Some(30),
            ..BudgetConfig::default()
        },
        constraints: ConstraintsConfig {
            quality_floor: Some(0.98),
            ..ConstraintsConfig::default()
        },
        capabilities: CapabilitiesConfig::default(),
        pipeline: PipelineConfig::default(),
        routing: RoutingConfig {
            max_model_tier: Some("standard".to_string()),
            ..RoutingConfig::default()
        },
        degradation: DegradationConfig::default(),
        autonomy: ProfileAutonomy::default(),
        output_hints: OutputHints {
            verify_footer: Some(true),
            related_hint: Some(true),
            compressed_hint: Some(true),
            ..OutputHints::default()
        },
    }
}

fn builtin_passthrough() -> Profile {
    Profile {
        profile: ProfileMeta {
            name: "passthrough".to_string(),
            inherits: None,
            description: "No output modification — always full content, no compression".to_string(),
        },
        read: ReadConfig {
            default_mode: Some("full".to_string()),
            max_tokens_per_file: Some(10_000_000),
            prefer_cache: Some(false),
        },
        compression: CompressionConfig {
            crp_mode: Some("off".to_string()),
            output_density: Some("normal".to_string()),
            entropy_threshold: None,
            terse_mode: Some(false),
            adaptive: None,
        },
        translation: TranslationConfig {
            enabled: Some(false),
            ..TranslationConfig::default()
        },
        layout: LayoutConfig::default(),
        memory: crate::core::memory_policy::MemoryPolicyOverrides::default(),
        verification: crate::core::output_verification::VerificationConfig::default(),
        budget: BudgetConfig {
            max_context_tokens: Some(1_000_000),
            ..BudgetConfig::default()
        },
        constraints: ConstraintsConfig::default(),
        capabilities: CapabilitiesConfig::default(),
        pipeline: PipelineConfig {
            intent: Some(false),
            relevance: Some(false),
            compression: Some(false),
            translation: Some(false),
        },
        routing: RoutingConfig::default(),
        degradation: DegradationConfig {
            enforce: Some(false),
            ..DegradationConfig::default()
        },
        autonomy: ProfileAutonomy::default(),
        output_hints: OutputHints::default(),
    }
}

/// Returns all built-in profile definitions.
pub(crate) fn builtin_profiles() -> HashMap<String, Profile> {
    let mut map = HashMap::new();
    for p in [
        builtin_coder(),
        builtin_exploration(),
        builtin_bugfix(),
        builtin_hotfix(),
        builtin_ci_debug(),
        builtin_review(),
        builtin_passthrough(),
    ] {
        map.insert(p.profile.name.clone(), p);
    }
    map
}

/// Constructs a single built-in profile by name, building only the one
/// requested.
///
/// `active_profile()` resolves to a built-in on most calls (no on-disk
/// override), and it is invoked many times per tool dispatch. Going through
/// [`builtin_profiles`] there materialized all seven profile structs just to
/// drop six — this hot-path shortcut builds exactly one. The match arms must
/// stay in sync with [`builtin_profiles`].
pub(super) fn builtin_profile(name: &str) -> Option<Profile> {
    match name {
        "coder" => Some(builtin_coder()),
        "exploration" => Some(builtin_exploration()),
        "bugfix" => Some(builtin_bugfix()),
        "hotfix" => Some(builtin_hotfix()),
        "ci-debug" => Some(builtin_ci_debug()),
        "review" => Some(builtin_review()),
        "passthrough" => Some(builtin_passthrough()),
        _ => None,
    }
}
