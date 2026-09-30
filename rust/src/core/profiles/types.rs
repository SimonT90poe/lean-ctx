use serde::{Deserialize, Serialize};

/// A complete context profile definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    #[serde(default)]
    pub profile: ProfileMeta,
    #[serde(default)]
    pub read: ReadConfig,
    #[serde(default)]
    pub compression: CompressionConfig,
    #[serde(default)]
    pub translation: TranslationConfig,
    #[serde(default)]
    pub layout: LayoutConfig,
    #[serde(default)]
    pub memory: crate::core::memory_policy::MemoryPolicyOverrides,
    #[serde(default)]
    pub verification: crate::core::output_verification::VerificationConfig,
    #[serde(default)]
    pub budget: BudgetConfig,
    #[serde(default)]
    pub constraints: ConstraintsConfig,
    #[serde(default)]
    pub capabilities: CapabilitiesConfig,
    #[serde(default)]
    pub pipeline: PipelineConfig,
    #[serde(default)]
    pub routing: RoutingConfig,
    #[serde(default)]
    pub degradation: DegradationConfig,
    #[serde(default)]
    pub autonomy: ProfileAutonomy,
    #[serde(default)]
    pub output_hints: OutputHints,
}

/// Profile identity and inheritance.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProfileMeta {
    #[serde(default)]
    pub name: String,
    pub inherits: Option<String>,
    #[serde(default)]
    pub description: String,
}

/// Read behavior configuration.
///
/// Fields are `Option<T>` for field-level profile inheritance.
/// Use `_effective()` methods to get the resolved value with defaults.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ReadConfig {
    pub default_mode: Option<String>,
    pub max_tokens_per_file: Option<usize>,
    pub prefer_cache: Option<bool>,
}

impl ReadConfig {
    pub fn default_mode_effective(&self) -> &str {
        self.default_mode.as_deref().unwrap_or("auto")
    }
    pub fn max_tokens_per_file_effective(&self) -> usize {
        self.max_tokens_per_file.unwrap_or(50_000)
    }
    pub fn prefer_cache_effective(&self) -> bool {
        self.prefer_cache.unwrap_or(false)
    }
}

/// Compression strategy configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CompressionConfig {
    /// Enable adaptive compression depth (#1195). When true, compression
    /// aggressiveness is reduced dynamically based on bounce rate and session
    /// length. Default: true.
    pub adaptive: Option<bool>,
    pub crp_mode: Option<String>,
    pub output_density: Option<String>,
    pub entropy_threshold: Option<f64>,
    pub terse_mode: Option<bool>,
}

impl CompressionConfig {
    /// The CRP mode this profile actually produces, after env and
    /// `compression_level` precedence — the same value tools use (#1890).
    pub fn crp_mode_effective(&self) -> &'static str {
        crate::core::protocol::CrpMode::resolve(self.crp_mode.as_deref()).as_str()
    }
    pub fn output_density_effective(&self) -> &str {
        self.output_density.as_deref().unwrap_or("normal")
    }
    pub fn entropy_threshold_effective(&self) -> f64 {
        self.entropy_threshold.unwrap_or(0.3)
    }
    pub fn terse_mode_effective(&self) -> bool {
        self.terse_mode.unwrap_or(false)
    }
    pub fn adaptive_effective(&self) -> bool {
        self.adaptive.unwrap_or(true)
    }
}

/// Translation (tokenizer-aware) configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TranslationConfig {
    /// If false, preserve legacy CRP/TDD formats without post-translation.
    pub enabled: Option<bool>,
    /// legacy|ascii|auto
    pub ruleset: Option<String>,
}

impl TranslationConfig {
    pub fn enabled_effective(&self) -> bool {
        self.enabled.unwrap_or(false)
    }
    pub fn ruleset_effective(&self) -> &str {
        self.ruleset.as_deref().unwrap_or("legacy")
    }
}

/// Layout (attention-aware reorder) configuration.
///
/// Reserved: no read path reorders content, so these keys are accepted for
/// profile compatibility but have no effect (#1915,
/// docs/contracts/attention-layout-driver-v1.md).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LayoutConfig {
    pub enabled: Option<bool>,
    pub min_lines: Option<usize>,
}

/// Routing policy overrides (intent → model tier → read mode/budgets).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RoutingConfig {
    /// Hard cap for recommended model tier: fast|standard|premium.
    #[serde(default)]
    pub max_model_tier: Option<String>,
    /// If true, apply deterministic routing degradation under budget/pressure.
    #[serde(default)]
    pub degrade_under_pressure: Option<bool>,
}

impl RoutingConfig {
    pub fn max_model_tier_effective(&self) -> &str {
        self.max_model_tier.as_deref().unwrap_or("premium")
    }

    pub fn degrade_under_pressure_effective(&self) -> bool {
        self.degrade_under_pressure.unwrap_or(true)
    }
}

/// Budget/SLO degradation policy configuration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DegradationConfig {
    /// If true, enforce throttling/blocking decisions. Default is warn-only.
    #[serde(default)]
    pub enforce: Option<bool>,
    /// Throttle duration (ms) when policy verdict is Throttle. Default: 250ms.
    #[serde(default)]
    pub throttle_ms: Option<u64>,
}

impl DegradationConfig {
    pub fn enforce_effective(&self) -> bool {
        self.enforce.unwrap_or(false)
    }

    pub fn throttle_ms_effective(&self) -> u64 {
        self.throttle_ms.unwrap_or(250)
    }
}

/// Controls which optional hints/footers are appended to tool output.
/// All default to `false` for minimal output overhead.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputHints {
    pub compressed_hint: Option<bool>,
    pub archive_hint: Option<bool>,
    pub verify_footer: Option<bool>,
    pub related_hint: Option<bool>,
    pub semantic_hint: Option<bool>,
    pub elicitation_hint: Option<bool>,
    pub checkpoint_in_output: Option<bool>,
    pub graph_context_block: Option<bool>,
    pub efficiency_hint: Option<bool>,
    /// Cross-source hints: append issue/PR/schema references from the property
    /// graph when reading a file. Default: off (speculative, costs tokens).
    pub cross_source_hint: Option<bool>,
    /// Proactive context: auto-expand previously compressed content when
    /// keyword-relevant to the current read. Default: off (speculative, up to
    /// 2000 tokens per read).
    pub proactive_context: Option<bool>,
}

impl OutputHints {
    pub fn compressed_hint(&self) -> bool {
        self.compressed_hint.unwrap_or(false)
    }
    pub fn archive_hint(&self) -> bool {
        self.archive_hint.unwrap_or(false)
    }
    pub fn verify_footer(&self) -> bool {
        self.verify_footer.unwrap_or(false)
    }
    pub fn related_hint(&self) -> bool {
        self.related_hint.unwrap_or(false)
    }
    pub fn semantic_hint(&self) -> bool {
        self.semantic_hint.unwrap_or(false)
    }
    pub fn elicitation_hint(&self) -> bool {
        self.elicitation_hint.unwrap_or(false)
    }
    pub fn checkpoint_in_output(&self) -> bool {
        self.checkpoint_in_output.unwrap_or(false)
    }
    pub fn graph_context_block(&self) -> bool {
        self.graph_context_block.unwrap_or(false)
    }
    pub fn efficiency_hint(&self) -> bool {
        self.efficiency_hint.unwrap_or(false)
    }
    pub fn cross_source_hint(&self) -> bool {
        self.cross_source_hint.unwrap_or(false)
    }
    pub fn proactive_context(&self) -> bool {
        self.proactive_context.unwrap_or(false)
    }
}

/// Token and cost budget limits.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct BudgetConfig {
    pub max_context_tokens: Option<usize>,
    pub max_shell_invocations: Option<usize>,
    pub max_cost_usd: Option<f64>,
}

impl BudgetConfig {
    pub fn max_context_tokens_effective(&self) -> usize {
        self.max_context_tokens.unwrap_or(200_000)
    }
    pub fn max_shell_invocations_effective(&self) -> usize {
        self.max_shell_invocations.unwrap_or(100)
    }
    pub fn max_cost_usd_effective(&self) -> f64 {
        self.max_cost_usd.unwrap_or(5.0)
    }
}

/// Targets for a profile's output and resource use.
///
/// Offline only: `quality_floor` feeds the benchmark spec and `max_context_tokens`
/// caps `lean-ctx calibrate`. No live read/shell/proxy path enforces any of these,
/// and `max_cost_usd`, `max_latency_ms` and `require_verification` are read by
/// nothing yet (#1905).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ConstraintsConfig {
    /// Minimum benchmark quality score, from 0.0 to 1.0 (offline benchmark gate).
    pub quality_floor: Option<f64>,
    /// Maximum cost per task in USD. Not enforced.
    pub max_cost_usd: Option<f64>,
    /// Maximum latency per operation in milliseconds. Not enforced.
    pub max_latency_ms: Option<u64>,
    /// Context limit applied by `lean-ctx calibrate` when lower than the budget.
    pub max_context_tokens: Option<usize>,
    /// Require output verification before accepting a result. Not enforced.
    pub require_verification: Option<bool>,
}

impl ConstraintsConfig {
    pub fn quality_floor_effective(&self) -> f64 {
        self.quality_floor.unwrap_or(0.95)
    }

    pub fn max_cost_usd_effective(&self) -> f64 {
        self.max_cost_usd.unwrap_or(f64::MAX)
    }

    pub fn max_latency_ms_effective(&self) -> u64 {
        self.max_latency_ms.unwrap_or(u64::MAX)
    }

    pub fn max_context_tokens_effective(&self) -> usize {
        self.max_context_tokens.unwrap_or(usize::MAX)
    }

    pub fn require_verification_effective(&self) -> bool {
        self.require_verification.unwrap_or(false)
    }
}

/// Provider and strategy selected for one context-engineering capability.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CapabilityBinding {
    /// Provider identifier, such as `leanctx`, `rtk`, or `custom`.
    pub provider: Option<String>,
    /// Strategy identifier, such as `structural` or `adaptive`.
    pub strategy: Option<String>,
    /// Pinned provider or strategy version.
    pub version: Option<String>,
}

/// Capability provider bindings selected by a profile.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CapabilitiesConfig {
    pub code_context: Option<CapabilityBinding>,
    pub shell_output: Option<CapabilityBinding>,
    pub knowledge: Option<CapabilityBinding>,
    pub routing: Option<CapabilityBinding>,
    pub compression: Option<CapabilityBinding>,
}

/// Pipeline layer activation per profile.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PipelineConfig {
    pub intent: Option<bool>,
    pub relevance: Option<bool>,
    pub compression: Option<bool>,
    pub translation: Option<bool>,
}

impl PipelineConfig {
    pub fn intent_effective(&self) -> bool {
        self.intent.unwrap_or(true)
    }
    pub fn relevance_effective(&self) -> bool {
        self.relevance.unwrap_or(true)
    }
    pub fn compression_effective(&self) -> bool {
        self.compression.unwrap_or(true)
    }
    pub fn translation_effective(&self) -> bool {
        self.translation.unwrap_or(true)
    }
}

/// Autonomy overrides per profile.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProfileAutonomy {
    pub enabled: Option<bool>,
    pub auto_preload: Option<bool>,
    pub auto_dedup: Option<bool>,
    pub auto_related: Option<bool>,
    pub silent_preload: Option<bool>,
    /// Enable bounded prefetch after reads (opt-in by default).
    pub auto_prefetch: Option<bool>,
    /// Enable response shaping for large outputs (opt-in by default).
    pub auto_response: Option<bool>,
    pub dedup_threshold: Option<usize>,
    pub prefetch_max_files: Option<usize>,
    pub prefetch_budget_tokens: Option<usize>,
    pub response_min_tokens: Option<usize>,
    pub checkpoint_interval: Option<u32>,
}

impl ProfileAutonomy {
    pub fn enabled_effective(&self) -> bool {
        self.enabled.unwrap_or(true)
    }
    pub fn auto_preload_effective(&self) -> bool {
        self.auto_preload.unwrap_or(true)
    }
    pub fn auto_dedup_effective(&self) -> bool {
        self.auto_dedup.unwrap_or(true)
    }
    pub fn auto_related_effective(&self) -> bool {
        self.auto_related.unwrap_or(true)
    }
    pub fn silent_preload_effective(&self) -> bool {
        self.silent_preload.unwrap_or(true)
    }
    pub fn auto_prefetch_effective(&self) -> bool {
        self.auto_prefetch.unwrap_or(false)
    }
    pub fn auto_response_effective(&self) -> bool {
        self.auto_response.unwrap_or(false)
    }
    pub fn dedup_threshold_effective(&self) -> usize {
        self.dedup_threshold.unwrap_or(8)
    }
    pub fn prefetch_max_files_effective(&self) -> usize {
        self.prefetch_max_files.unwrap_or(3)
    }
    pub fn prefetch_budget_tokens_effective(&self) -> usize {
        self.prefetch_budget_tokens.unwrap_or(4000)
    }
    pub fn response_min_tokens_effective(&self) -> usize {
        self.response_min_tokens.unwrap_or(600)
    }
    pub fn checkpoint_interval_effective(&self) -> u32 {
        self.checkpoint_interval.unwrap_or(15)
    }
}
