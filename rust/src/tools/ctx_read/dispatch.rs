use super::{
    CrpMode, ReadMode, ReadOutput, ReadTuning, SessionCache, count_tokens, dedup_hook,
    handle_with_options_inner, kernel, protocol,
};
const MAX_RELAY_CONTENT_BYTES: usize = 8192;

/// Cursor writes terminal output to `.cursor/projects/*/terminals/*.txt` and
/// polls these files every ~3s. Without special handling, each poll triggers a
/// full re-read because the conversation gate blocks stubs under concurrency
/// (#1040). Terminal files are system infrastructure — not conversation-scoped
/// — so they safely bypass the gate while the content-hash check still
/// guarantees correctness.
fn is_terminal_poll_file(path: &str) -> bool {
    (path.contains("/.cursor/") || path.contains("\\.cursor\\"))
        && (path.contains("/terminals/") || path.contains("\\terminals\\"))
        && std::path::Path::new(path)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("txt"))
}

/// Modes whose compressed output is useful for cross-agent relay.
const RELAY_ELIGIBLE_MODES: &[&str] = &["map", "map:v2", "signatures", "signatures:v2"];

/// Extract relay-eligible content from a rendered read.
fn relay_eligible<'a>(mode: &'a str, content: &'a str) -> (Option<&'a str>, Option<&'a str>) {
    if RELAY_ELIGIBLE_MODES.iter().any(|m| mode.starts_with(m))
        && content.len() <= MAX_RELAY_CONTENT_BYTES
    {
        (Some(content), Some(mode))
    } else {
        (None, None)
    }
}
/// Reads a file through the cache and applies the requested compression mode.
pub fn handle(cache: &mut SessionCache, path: &str, mode: &str, crp_mode: CrpMode) -> String {
    handle_with_options(cache, path, mode, false, crp_mode, None)
}

/// Like `handle`, but invalidates the cache first to force a fresh disk read.
pub fn handle_fresh(cache: &mut SessionCache, path: &str, mode: &str, crp_mode: CrpMode) -> String {
    handle_with_options(cache, path, mode, true, crp_mode, None)
}

/// Reads a file with task-aware filtering to prioritize task-relevant content.
pub fn handle_with_task(
    cache: &mut SessionCache,
    path: &str,
    mode: &str,
    crp_mode: CrpMode,
    task: Option<&str>,
) -> String {
    handle_with_task_result(cache, path, mode, crp_mode, task).content
}

/// Task-aware read with the structural cache-hit result retained for callers
/// that need to account for each file in a batch independently.
pub fn handle_with_task_result(
    cache: &mut SessionCache,
    path: &str,
    mode: &str,
    crp_mode: CrpMode,
    task: Option<&str>,
) -> ReadOutput {
    let mut result = handle_with_options_resolved(
        cache,
        path,
        mode,
        false,
        crp_mode,
        task,
        ReadTuning::resolve(None, &[]),
    );
    kernel::enrich_with_kernel(&mut result.content, task);
    result.output_tokens = count_tokens(&result.content);
    result
}

/// Like `handle_with_task`, also returns the resolved mode name and pre-counted tokens.
pub fn handle_with_task_resolved(
    cache: &mut SessionCache,
    path: &str,
    mode: &str,
    crp_mode: CrpMode,
    task: Option<&str>,
) -> ReadOutput {
    handle_with_options_resolved(
        cache,
        path,
        mode,
        false,
        crp_mode,
        task,
        ReadTuning::resolve(None, &[]),
    )
}

/// Like [`handle_with_task_resolved`] but with an explicit per-call
/// aggressiveness (the `ctx_read` `aggressiveness` arg, #714). `None` falls back
/// to the `LEAN_CTX_AGGRESSIVENESS` env var / config field.
pub fn handle_with_task_resolved_tuned(
    cache: &mut SessionCache,
    path: &str,
    mode: &str,
    crp_mode: CrpMode,
    task: Option<&str>,
    aggressiveness: Option<f64>,
    protect: &[String],
) -> ReadOutput {
    handle_with_options_resolved(
        cache,
        path,
        mode,
        false,
        crp_mode,
        task,
        ReadTuning::resolve(aggressiveness, protect),
    )
}

/// Like [`handle_with_task_resolved_tuned`] but accepts pre-read file content,
/// avoiding disk I/O under the cache write-lock (Two-Phase Read pattern, #1098).
#[allow(clippy::too_many_arguments)]
pub fn handle_with_preread(
    cache: &mut SessionCache,
    path: &str,
    mode: &str,
    fresh: bool,
    crp_mode: CrpMode,
    task: Option<&str>,
    aggressiveness: Option<f64>,
    protect: &[String],
    preread: String,
) -> ReadOutput {
    handle_with_options_resolved_preread(
        cache,
        path,
        mode,
        fresh,
        crp_mode,
        task,
        ReadTuning::resolve(aggressiveness, protect),
        Some(preread),
    )
}

/// Fresh read with task-aware filtering (invalidates cache first).
pub fn handle_fresh_with_task(
    cache: &mut SessionCache,
    path: &str,
    mode: &str,
    crp_mode: CrpMode,
    task: Option<&str>,
) -> String {
    handle_fresh_with_task_result(cache, path, mode, crp_mode, task).content
}

/// Fresh task-aware read with the structural cache-hit result retained.
pub fn handle_fresh_with_task_result(
    cache: &mut SessionCache,
    path: &str,
    mode: &str,
    crp_mode: CrpMode,
    task: Option<&str>,
) -> ReadOutput {
    handle_with_options_resolved(
        cache,
        path,
        mode,
        true,
        crp_mode,
        task,
        ReadTuning::resolve(None, &[]),
    )
}

/// Fresh read with task-aware filtering, also returns the resolved mode name and pre-counted tokens.
pub fn handle_fresh_with_task_resolved(
    cache: &mut SessionCache,
    path: &str,
    mode: &str,
    crp_mode: CrpMode,
    task: Option<&str>,
) -> ReadOutput {
    handle_with_options_resolved(
        cache,
        path,
        mode,
        true,
        crp_mode,
        task,
        ReadTuning::resolve(None, &[]),
    )
}

/// Fresh-read variant of [`handle_with_task_resolved_tuned`] (#714).
pub fn handle_fresh_with_task_resolved_tuned(
    cache: &mut SessionCache,
    path: &str,
    mode: &str,
    crp_mode: CrpMode,
    task: Option<&str>,
    aggressiveness: Option<f64>,
    protect: &[String],
) -> ReadOutput {
    handle_with_options_resolved(
        cache,
        path,
        mode,
        true,
        crp_mode,
        task,
        ReadTuning::resolve(aggressiveness, protect),
    )
}

fn handle_with_options(
    cache: &mut SessionCache,
    path: &str,
    mode: &str,
    fresh: bool,
    crp_mode: CrpMode,
    task: Option<&str>,
) -> String {
    handle_with_options_resolved(
        cache,
        path,
        mode,
        fresh,
        crp_mode,
        task,
        ReadTuning::resolve(None, &[]),
    )
    .content
}

/// `LEAN_CTX_FORCE_FRESH=1` — an explicit operator override that always forces a
/// cold full read, independent of conversation scoping.
pub(crate) fn force_fresh_env() -> bool {
    static FORCE_FRESH: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FORCE_FRESH.get_or_init(|| {
        std::env::var("LEAN_CTX_FORCE_FRESH").is_ok_and(|v| v == "1" || v == "true")
    })
}

/// Detects a subagent (forked agent) execution context.
///
/// A subagent must never be served a stub for content only the parent received.
/// That used to be enforced by force-freshing *every* subagent read; with
/// conversation scoping (#954/#955) the subagent instead runs under its own scope
/// (`conversation::current_conversation_id` → `task:{id}` or `proc:{id}`), so
/// the stub gate withholds cross-agent stubs precisely while restoring the
/// subagent's *own* cheap re-reads. The blanket force-fresh is therefore kept
/// only as the fallback when scoping is disabled (#956).
///
/// Checks `CURSOR_TASK_ID` (Cursor) and `CLAUDE_CODE_ENTRYPOINT=local-agent`
/// (future Claude Code subagent marker). Current Claude Code is handled by
/// per-process scoping in [`crate::core::conversation`] (#1292).
pub(crate) fn is_subagent_context() -> bool {
    static IS_SUBAGENT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *IS_SUBAGENT.get_or_init(|| {
        std::env::var("CURSOR_TASK_ID").is_ok_and(|v| !v.is_empty())
            || std::env::var("CLAUDE_CODE_ENTRYPOINT")
                .ok()
                .as_deref()
                .map(str::trim)
                == Some("local-agent")
    })
}

/// Keeps subagent cache isolation while independently deciding whether a
/// cross-agent delivery lookup may return a stub.
#[allow(clippy::fn_params_excessive_bools)]
pub(crate) fn effective_fresh_flags(
    fresh: bool,
    force_fresh: bool,
    subagent_context: bool,
    delivery_for_subagents: bool,
) -> (bool, bool) {
    let effective_fresh_for_cache = fresh || force_fresh || subagent_context;
    let effective_fresh_for_delivery =
        fresh || force_fresh || (subagent_context && !delivery_for_subagents);
    (effective_fresh_for_cache, effective_fresh_for_delivery)
}

pub(crate) fn effective_fresh_for_delivery(fresh: bool) -> bool {
    let config = crate::core::config::Config::load();
    effective_fresh_flags(
        fresh,
        force_fresh_env(),
        is_subagent_context(),
        config.ocla.delivery.delivery_for_subagents,
    )
    .1
}

fn handle_with_options_resolved(
    cache: &mut SessionCache,
    path: &str,
    mode: &str,
    fresh: bool,
    crp_mode: CrpMode,
    task: Option<&str>,
    tuning: ReadTuning<'_>,
) -> ReadOutput {
    handle_with_options_resolved_preread(cache, path, mode, fresh, crp_mode, task, tuning, None)
}

fn handle_with_options_resolved_preread(
    cache: &mut SessionCache,
    path: &str,
    mode: &str,
    fresh: bool,
    crp_mode: CrpMode,
    task: Option<&str>,
    tuning: ReadTuning<'_>,
    preread: Option<String>,
) -> ReadOutput {
    // Subagents retain isolated session caches, but can use delivery stubs when
    // configured because those stubs explicitly identify another agent's read.
    let config = crate::core::config::Config::load();
    let (effective_fresh_for_cache, effective_fresh_for_delivery) = effective_fresh_flags(
        fresh,
        force_fresh_env(),
        is_subagent_context(),
        config.ocla.delivery.delivery_for_subagents,
    );

    let compress_protected = mode != "raw"
        && !mode.starts_with("lines:")
        && crate::core::config::Config::load()
            .proxy
            .is_path_compress_protected(path);

    // Hash once for cross-agent delivery. The same snapshot is used for both
    // the pre-read lookup and the post-read record, avoiding a second disk read.
    let delivery_metadata = config
        .ocla
        .delivery_enabled()
        .then(|| file_blake3_prefix(path))
        .flatten();

    if !effective_fresh_for_delivery
        && !compress_protected
        && let Some(fp) = delivery_metadata
        && let Some(stub) = try_cross_agent_stub(path, mode, fp.hash, fp.mtime)
    {
        return stub;
    }

    if mode == "auto" {
        let touched: Vec<String> = cache
            .get_all_entries()
            .iter()
            .map(|(p, _)| (*p).clone())
            .collect();
        if crate::core::relevance_gate::should_gate(path, mode, task, &touched) {
            let meta = std::fs::metadata(path);
            let byte_count = meta.as_ref().map_or(0, std::fs::Metadata::len);
            let line_count = preread
                .as_ref()
                .map_or(0, |c| bytecount::count(c.as_bytes(), b'\n'));
            let stub = crate::core::relevance_gate::irrelevant_stub(path, line_count, byte_count);
            let stub_tokens = count_tokens(&stub);
            return ReadOutput {
                content: stub,
                resolved_mode: "auto".into(),
                output_tokens: stub_tokens,
                is_cache_hit: false,
            };
        }
    }

    if let Ok(mut bt) = crate::core::bounce_tracker::global().lock() {
        bt.next_seq();
    }
    let mut result = handle_with_options_inner(
        cache,
        path,
        mode,
        effective_fresh_for_cache,
        crp_mode,
        task,
        tuning,
        preread,
    );

    if let Some(entry) = cache.get_mut(path) {
        entry.last_mode.clone_from(&result.resolved_mode);
        if matches!(result.resolved_mode.as_str(), "full" | "full-compact")
            && entry.full_content_delivered
            && result.is_cache_hit
            && entry.bump_reread() >= crate::core::cache::full_degradation_threshold()
        {
            entry.full_content_delivered = false;
            entry.reset_reread_count();
            crate::core::auto_mode_resolver::count_source("full_delivery_degraded");
        }
        // #841: a partial/filtered read means the model's most recent view is NOT
        // the full content. Clear the delivery flag so a subsequent mode="full"
        // re-delivers real content instead of the [unchanged] stub. Without this,
        // a task→full sequence returns an empty stub because the flag was set by an
        // earlier full delivery and never cleared by the intervening non-full read.
        if !matches!(result.resolved_mode.as_str(), "full" | "full-compact") {
            entry.full_content_delivered = false;
            entry.reset_reread_count();
        }
    }

    if !result.is_cache_hit
        && let Some(fp) = delivery_metadata
    {
        record_read_delivery(
            path,
            fp,
            &result.resolved_mode,
            &result.content,
            result.output_tokens,
        );
    }

    // SSOT via [`ReadMode`] (#528): lossy summaries may elide shared blocks.
    let dedup_allowed = result
        .resolved_mode
        .parse::<ReadMode>()
        .is_ok_and(|m| m.is_lossy_summary());
    if dedup_allowed && let Some(deduped) = cache.apply_dedup(path, &result.content) {
        let new_tokens = count_tokens(&deduped);
        if new_tokens < result.output_tokens {
            result.content = deduped;
            result.output_tokens = new_tokens;
        }
    }

    // R28: Kernel content dedup — detect re-reads of unchanged content.
    if let Some(stub) = dedup_hook::maybe_dedup(path, &result.content, mode, fresh) {
        let stub_tokens = count_tokens(&stub);
        if stub_tokens < result.output_tokens {
            result.content = stub;
            result.output_tokens = stub_tokens;
            result.is_cache_hit = true;
            crate::core::anti_interrupt::spawn_redundant_read(path);
        }
    }

    // R30: Feed bounce-tracker signal into adaptive compression bridge.
    crate::core::context_kernel::adaptive_hook::update_from_bounce_tracker();
    if let Ok(mut bt) = crate::core::bounce_tracker::global().lock() {
        let original_tokens = cache.get(path).map_or(0, |e| e.original_tokens);
        let bounces_before = bt.total_bounces();
        let output_tokens = result.output_tokens;
        bt.record_read(path, &result.resolved_mode, output_tokens, original_tokens);

        if bt.total_bounces() > bounces_before {
            crate::core::anti_interrupt::spawn_bounce_waste(output_tokens as u64);
        }

        // Quality signals (#538): compressed reads count as clean until a
        // bounce proves otherwise (the bounce signal outweighs 6:1); large
        // full reads of never-bouncing extensions are wasted compression
        // opportunities and push the learned threshold up.
        // SSOT via [`ReadMode`] (#528): only verbatim `full` and the `diff`
        // delta are uncompressed. A resolved window is always the canonical
        // `lines:N-M` (parses to `Lines` ⇒ compressed); the default of `true`
        // for the unreachable bare `"lines"` keeps prior behaviour everywhere a
        // real resolved mode can occur.
        let compressed = result
            .resolved_mode
            .parse::<ReadMode>()
            .map_or(true, |m| m.counts_as_compressed());
        if compressed {
            crate::core::adaptive_thresholds::record_quality_signal(
                path,
                crate::core::threshold_learning::QualitySignal::CleanCompressed,
            );
        } else if result.resolved_mode == "full"
            && result.output_tokens > 2000
            && bt.bounce_rate_for_extension(path).unwrap_or(0.0) < 0.05
        {
            crate::core::adaptive_thresholds::record_quality_signal(
                path,
                crate::core::threshold_learning::QualitySignal::WastedFull,
            );
        }
    }

    // Stigmergy (#540): deposit a Hot scent for this read in the background
    // (the field file lock may briefly block; never stall the read path). The
    // foreign-claim hint is intentionally NOT appended to the body: it carries a
    // relative timestamp ("claimed Nm ago"), which would make the output a
    // non-pure function of wall-clock time and defeat provider prompt caching
    // (#498). The deposit remains so the field still reflects active work.
    {
        let self_agent = crate::core::scent_field::scent_agent_id();
        let scent_path = crate::core::pathutil::normalize_tool_path(path);
        std::thread::spawn(move || {
            crate::core::scent_field::deposit(
                self_agent,
                crate::core::scent_field::ScentKind::Hot,
                &scent_path,
                0.3,
            );
        });
    }

    crate::core::context_gc::maybe_gc(cache);

    result
}

/// Attempt to serve a `mode="full"` cache hit (`[unchanged …]`) using only a
/// shared borrow of the cache.
///
/// Returns `None` when the file is not cached, was modified on disk, full
/// content was never delivered, or the cache policy forbids stubbing — in those
/// cases the caller must fall back to the write path.
///
/// This is the read-locked fast path: it needs no `&mut SessionCache`, so the
/// dominant "re-read an unchanged file" case proceeds under a shared lock and
/// parallel reads of distinct files no longer serialize on a global write lock.
pub fn try_stub_hit_readonly(cache: &SessionCache, path: &str) -> Option<ReadOutput> {
    // Resolve the caller *fresh* (TTL-bypassed): the stub gate's concurrency
    // detection must see a just-appeared second chat with zero lag, else a stub
    // could leak across chats in the pre-detection window (#1042).
    let current_conversation = crate::core::conversation::current_conversation_id_fresh();
    try_stub_hit_readonly_scoped(cache, path, current_conversation.as_deref())
}

/// Conversation-scoped core of [`try_stub_hit_readonly`]. The current
/// conversation id is injected (not read from the global resolver) so the
/// conversation gate can be tested deterministically without global state.
pub(crate) fn try_stub_hit_readonly_scoped(
    cache: &SessionCache,
    path: &str,
    current_conversation: Option<&str>,
) -> Option<ReadOutput> {
    if !stub_policy_allows() {
        return None;
    }

    // Warm path: a live in-memory entry is the freshest source of truth.
    if let Some(file_ref) = cache.get_file_ref_readonly(path) {
        let (cached_mtime, cached_hash, line_count, delivered_conv) = {
            let entry = cache.get(path)?;
            (
                entry.stored_mtime,
                entry.hash.clone(),
                entry.line_count,
                entry.delivered_conversation.clone(),
            )
        };
        if crate::core::cache::is_cache_entry_stale_verified(path, cached_mtime, &cached_hash)
            || !cache.is_full_delivered(path)
        {
            return None;
        }
        // Terminal poll files bypass the conversation gate: they are system
        // infrastructure, not conversation-scoped content. The content-hash
        // verification above already guarantees the file is unchanged.
        let is_terminal_poll = is_terminal_poll_file(path);

        if !is_terminal_poll
            && !crate::core::conversation::conversation_allows_stub(
                current_conversation,
                delivered_conv.as_deref(),
            )
        {
            crate::core::cache_telemetry::record_conversation_mismatch();
            return None;
        }
        let original_tokens = cache.record_cache_hit(path)?.original_tokens;
        crate::core::telemetry::global_metrics().record_cache(true);
        let stub = render_unchanged_stub(&file_ref, path, line_count);
        crate::core::stats::record_reread(original_tokens.saturating_sub(stub.output_tokens));
        return Some(stub);
    }

    // Cold fallback (#955): no live entry (e.g. after a daemon restart or idle
    // clear). Serve the stub from the persisted index iff the file is unchanged
    // AND the *same known* conversation is asking — a stricter gate than the warm
    // path, because a cold stub crosses a process boundary (no "no context →
    // legacy" escape; see `conversation_allows_cold_stub`).
    let rec = crate::core::read_stub_index::lookup(path)?;
    if crate::core::cache::is_cache_entry_stale_verified(path, rec.stored_mtime(), &rec.hash) {
        return None;
    }
    if !is_terminal_poll_file(path)
        && !crate::core::conversation::conversation_allows_cold_stub(
            current_conversation,
            rec.delivered_conversation.as_deref(),
        )
    {
        crate::core::cache_telemetry::record_conversation_mismatch();
        return None;
    }
    Some(render_unchanged_stub(&rec.file_ref, path, rec.line_count))
}

/// Whether the cache policy and active profile allow serving `[unchanged …]`
/// stubs at all. Shared by the full-content stub path and the #1287 variant
/// stub path so the two can never diverge.
pub(crate) fn stub_policy_allows() -> bool {
    let no_deg = crate::core::config::Config::load().no_degrade_effective();
    let prof = crate::core::profiles::active_profile();
    let force_full = no_deg
        || (prof.read.default_mode_effective() == "full"
            && prof.compression.crp_mode.as_deref() == Some("off"));
    crate::server::compaction_sync::effective_cache_policy() != "safe" && !force_full
}

/// Renders the `[unchanged …]` stub body shared by the warm and cold stub paths.
///
/// #498 determinism: the stub is a pure function of (file_ref, path, line_count),
/// so identical re-reads stay byte-stable and provider prompt caching applies.
/// The `fresh=true` escape is a *static* suffix (no rotating proof lines or
/// read-count notes), so a re-reader in non-meta mode still sees how to force the
/// content (#513) without breaking byte-stability.
fn render_unchanged_stub(file_ref: &str, path: &str, line_count: usize) -> ReadOutput {
    let short = protocol::shorten_path(path);
    let out = if crate::core::protocol::meta_visible() {
        format!(
            "{file_ref}={short} [unchanged {line_count}L]\nUnchanged on disk. Use fresh=true to force re-read.",
        )
    } else {
        format!("{file_ref}={short} [unchanged {line_count}L · fresh=true to re-read]")
    };
    let out = crate::core::redaction::redact_text_if_enabled(&out);
    let sent = count_tokens(&out);
    ReadOutput {
        content: out,
        resolved_mode: "full".into(),
        output_tokens: sent,
        is_cache_hit: true,
    }
}

/// #1287: renders the `[unchanged <mode> view]` stub for a compressed-variant
/// re-read — same conversation, unchanged file, same mode key. Mirrors
/// [`render_unchanged_stub`]'s #498 determinism contract: a pure function of
/// (file_ref, path, mode), so identical re-reads stay byte-stable and the
/// static `fresh=true` escape survives non-meta mode.
pub(crate) fn render_unchanged_variant_stub(file_ref: &str, path: &str, mode: &str) -> ReadOutput {
    let short = protocol::shorten_path(path);
    let out = if crate::core::protocol::meta_visible() {
        format!(
            "{file_ref}={short} [unchanged {mode} view]\nSame {mode} output you already received. Use fresh=true to re-emit.",
        )
    } else {
        format!("{file_ref}={short} [unchanged {mode} view · fresh=true to re-emit]")
    };
    let out = crate::core::redaction::redact_text_if_enabled(&out);
    let sent = count_tokens(&out);
    ReadOutput {
        content: out,
        resolved_mode: mode.to_string(),
        output_tokens: sent,
        is_cache_hit: true,
    }
}

/// Outcome of [`resolve_explicit_delta_mode`]: the (possibly rewritten) read
/// mode plus an optional advisory note to surface to the agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeltaExplicitDecision {
    /// The mode the read should proceed with (rewritten only when the feature
    /// fires; otherwise the caller's mode, unchanged).
    pub mode: String,
    /// A byte-stable advisory appended to the read body when the mode was
    /// rewritten to `diff`. `None` when nothing was rewritten or the collapse
    /// was a silent `lines:`→`full` stub.
    pub note: Option<String>,
}

/// Decide whether an **explicit** `full`/`lines:N-M` re-read of a session-cached
/// file should be served as a delta instead of re-emitting content the model
/// already holds (the `delta_explicit` opt-in; env `LCTX_DELTA_EXPLICIT`).
///
/// Returns the mode the read should proceed with:
/// - **Changed on disk** (verified mtime+md5 stale) and full content is cached →
///   `diff`, plus an advisory note. The diff carries exactly the new
///   information in a fraction of the tokens.
/// - **Unchanged** and the request is `lines:` of an already-fully-delivered
///   file → `full`, so the read collapses to the ~15-token `[unchanged]` stub
///   instead of re-extracting a window the model has seen.
/// - Otherwise the caller's `mode` is returned untouched.
///
/// First reads (nothing cached) and `fresh=true` are never affected — the
/// caller gates those before calling. Staleness uses the **verified** variant
/// ([`crate::core::cache::is_cache_entry_stale_verified`]) so a same-second
/// write on a coarse-granularity filesystem cannot be mistaken for "unchanged"
/// and yield a misleading empty diff (#498 determinism).
///
/// Pure w.r.t. (cache, path, mode, enabled): no wall-clock, counters, or
/// randomness enter the result, so identical inputs stay byte-stable.
pub fn resolve_explicit_delta_mode(
    cache: &SessionCache,
    path: &str,
    mode: &str,
    explicit_mode: bool,
    fresh: bool,
    enabled: bool,
) -> DeltaExplicitDecision {
    let unchanged = DeltaExplicitDecision {
        mode: mode.to_string(),
        note: None,
    };
    if fresh
        || !enabled
        || !explicit_mode
        || !(mode == "full" || mode == "full-compact" || mode.starts_with("lines:"))
    {
        return unchanged;
    }
    let Some(entry) = cache.get(path) else {
        // First read this session — nothing to diff against.
        return unchanged;
    };
    let stale =
        crate::core::cache::is_cache_entry_stale_verified(path, entry.stored_mtime, &entry.hash);
    if stale {
        // Only divert to a diff when full content is actually cached: the diff
        // base is that full content (see `handle_diff`), never a compressed
        // view. Without it, `handle_diff` would have nothing to compare.
        if entry.content().is_some() {
            return DeltaExplicitDecision {
                mode: "diff".to_string(),
                note: Some(format!(
                    "[delta-explicit] requested mode={mode} served as a diff: the file \
                     changed since your last read and the diff is the new information. \
                     Pass fresh=true if you need the full content re-emitted."
                )),
            };
        }
        return unchanged;
    }
    // Unchanged on disk: a `lines:` window of a file already delivered in full
    // re-emits text the model holds — collapse to the full-mode stub
    // (~15 tokens). A plain `full` re-read already hits that stub downstream.
    if mode.starts_with("lines:") && cache.is_full_delivered(path) {
        return DeltaExplicitDecision {
            mode: "full".to_string(),
            note: None,
        };
    }
    unchanged
}

pub(crate) fn file_blake3_prefix(path: &str) -> Option<DeliveryFingerprint> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let bytes = std::fs::read(path).ok()?;
    let hash = blake3::hash(&bytes);
    let full = hash.as_bytes();
    let mut prefix = [0u8; 12];
    prefix.copy_from_slice(&full[..12]);
    Some(DeliveryFingerprint {
        hash: prefix,
        mtime,
        line_count: line_count_of(&bytes),
    })
}

/// Content snapshot taken once per read for cross-agent delivery: the lookup
/// and the record use the same hash, mtime and line count (#1909: records made
/// from the MCP path previously carried a hardcoded `0L`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DeliveryFingerprint {
    pub hash: [u8; 12],
    pub mtime: u64,
    pub line_count: u32,
}

/// Line count with `str::lines` semantics (the `SessionCache` convention): a
/// trailing newline does not open an extra line.
fn line_count_of(bytes: &[u8]) -> u32 {
    let newlines = bytecount::count(bytes, b'\n');
    let unterminated = usize::from(bytes.last().is_some_and(|b| *b != b'\n'));
    u32::try_from(newlines + unterminated).unwrap_or(u32::MAX)
}

/// Whether relayed content recorded in `relay_mode` answers a request for
/// `requested`. `auto` delegates the choice to lean-ctx, so any relay-eligible
/// view fits; an explicit mode only accepts that mode or a versioned variant of
/// it (`map` accepts `map:v2`) — never another view (`signatures` must not be
/// answered with a `map`) and never a `lines:` window.
fn relay_satisfies(requested: &str, relay_mode: &str) -> bool {
    requested == "auto"
        || relay_mode == requested
        || relay_mode
            .strip_prefix(requested)
            .is_some_and(|rest| rest.starts_with(':'))
}

pub(crate) fn try_cross_agent_stub(
    path: &str,
    mode: &str,
    hash: [u8; 12],
    mtime: u64,
) -> Option<ReadOutput> {
    if !crate::core::config::Config::load().ocla.delivery_enabled() {
        return None;
    }
    if matches!(mode, "full" | "raw" | "diff") || mode.starts_with("lines:") {
        return None;
    }
    let current_agent = crate::core::agent_identity::delivery_agent_id();
    let current_conversation = crate::core::conversation::current_conversation_id()
        .unwrap_or_else(|| current_agent.to_string());
    let reg = crate::core::ocla::OclaRegistry::global();
    let record = crate::daemon_client::try_delivery_check_blocking(
        &hash,
        mtime,
        path,
        Some(current_agent),
        Some(&current_conversation),
    )
    .or_else(|| {
        reg.delivery_registry.check_delivery(
            &hash,
            mtime,
            path,
            Some(current_agent),
            Some(&current_conversation),
        )
    })?;

    let short = protocol::shorten_path(path);

    // Relay carries the content itself, so it is correct in any conversation —
    // provided it is the view the caller asked for.
    if let Some(ref content) = record.relay_content {
        let relay_mode = record.relay_mode.as_deref().unwrap_or("map");
        if !relay_satisfies(mode, relay_mode) {
            return None;
        }
        let header = format!(
            "{short} [relayed from {} · {relay_mode} · {}L]",
            record.agent_id, record.line_count,
        );
        let body = format!("{header}\n{content}");
        let tokens = count_tokens(&body);
        reg.delivery_registry
            .record_stub_served(&record, tokens as u64);
        return Some(ReadOutput {
            content: body,
            resolved_mode: "cross-agent-relay".into(),
            output_tokens: tokens,
            is_cache_hit: true,
        });
    }

    // A content-free stub asserts "this is already in your context", which is
    // only true when the delivery provably reached the caller's conversation
    // (#1909). The registry never returns same-conversation records, so under
    // conversation scoping this withholds every content-free stub; it survives
    // only in explicit legacy mode (`LEAN_CTX_CONVERSATION_SCOPE=0`), whose
    // contract is one daemon == one conversation.
    if !crate::core::conversation::conversation_allows_stub(
        Some(&current_conversation),
        Some(&record.conversation_id),
    ) {
        return None;
    }

    let stub = format!(
        "{short} [cross-agent · {lines}L · read by {agent} · use fresh=true to force]",
        lines = record.line_count,
        agent = record.agent_id,
    );
    let tokens = count_tokens(&stub);
    reg.delivery_registry
        .record_stub_served(&record, tokens as u64);
    Some(ReadOutput {
        content: stub,
        resolved_mode: "cross-agent-stub".into(),
        output_tokens: tokens,
        is_cache_hit: true,
    })
}

/// Records a completed (non-cache-hit) read for cross-agent delivery, relaying
/// the rendered view when it is a compact, relay-eligible one. Shared by the
/// CLI/daemon dispatch path and the MCP handler so both record identically.
pub(crate) fn record_read_delivery(
    path: &str,
    fingerprint: DeliveryFingerprint,
    resolved_mode: &str,
    content: &str,
    output_tokens: usize,
) {
    let relay = relay_eligible(resolved_mode, content);
    record_cross_agent_delivery(
        path,
        fingerprint.hash,
        fingerprint.mtime,
        fingerprint.line_count,
        output_tokens,
        relay.0,
        relay.1,
    );
}

pub(crate) fn record_cross_agent_delivery(
    path: &str,
    hash: [u8; 12],
    mtime: u64,
    line_count: u32,
    tokens: usize,
    relay_content: Option<&str>,
    relay_mode: Option<&str>,
) {
    if !crate::core::config::Config::load().ocla.delivery_enabled() {
        return;
    }
    let agent_id = crate::core::agent_identity::delivery_agent_id().to_string();
    let conversation_id =
        crate::core::conversation::current_conversation_id().unwrap_or_else(|| agent_id.clone());
    let entry = crate::core::ocla::types::DeliveryEntry {
        blake3: hash,
        path: path.into(),
        line_count,
        token_count: tokens as u64,
        agent_id,
        conversation_id,
        mtime,
        relay_content: relay_content
            .filter(|c| c.len() <= MAX_RELAY_CONTENT_BYTES)
            .map(str::to_string),
        relay_mode: relay_mode.map(str::to_string),
    };
    crate::daemon_client::try_delivery_record_blocking(&entry);
    let reg = crate::core::ocla::OclaRegistry::global();
    reg.delivery_registry.record_delivery(entry);
}

#[cfg(test)]
mod tests {
    use super::{
        SessionCache, effective_fresh_flags, is_terminal_poll_file, try_cross_agent_stub,
        try_stub_hit_readonly_scoped,
    };
    use std::sync::atomic::Ordering;

    #[test]
    fn cross_agent_stub_miss_returns_none() {
        let stub = try_cross_agent_stub("/nonexistent/file.rs", "auto", [0; 12], 0);
        assert!(stub.is_none());
    }

    #[test]
    fn subagent_delivery_policy_keeps_cache_fresh_but_allows_delivery_by_default() {
        let delivery_for_subagents =
            crate::core::config::DeliveryConfig::default().delivery_for_subagents;
        assert!(
            delivery_for_subagents,
            "delivery must default to enabled for subagents"
        );
        let (cache_fresh, delivery_fresh) =
            effective_fresh_flags(false, false, true, delivery_for_subagents);
        assert!(cache_fresh, "subagent cache must remain isolated");
        assert!(
            !delivery_fresh,
            "default policy must allow a cross-agent delivery lookup"
        );
    }

    #[test]
    fn subagent_delivery_policy_can_force_fresh_delivery() {
        let (cache_fresh, delivery_fresh) = effective_fresh_flags(false, false, true, false);
        assert!(cache_fresh);
        assert!(delivery_fresh, "disabled policy must bypass delivery stubs");
    }

    #[test]
    fn line_count_matches_str_lines_semantics() {
        for sample in ["", "a", "a\n", "a\nb", "a\nb\n", "\n\n", "a\r\nb\r\n"] {
            assert_eq!(
                super::line_count_of(sample.as_bytes()) as usize,
                sample.lines().count(),
                "line count diverged for {sample:?}"
            );
        }
    }

    #[test]
    fn fingerprint_carries_real_line_count() {
        // #1909: the MCP path recorded a hardcoded 0L.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("three.rs");
        std::fs::write(&file, "fn a() {}\nfn b() {}\nfn c() {}\n").unwrap();
        let fp = super::file_blake3_prefix(file.to_str().unwrap()).unwrap();
        assert_eq!(fp.line_count, 3);
    }

    #[test]
    fn relay_answers_only_the_requested_view() {
        use super::relay_satisfies;
        assert!(relay_satisfies("auto", "map:v2"));
        assert!(relay_satisfies("auto", "signatures"));
        assert!(relay_satisfies("map", "map"));
        assert!(relay_satisfies("map", "map:v2"));
        assert!(relay_satisfies("signatures", "signatures:v2"));
        assert!(!relay_satisfies("signatures", "map:v2"));
        assert!(!relay_satisfies("map", "mapx"));
        assert!(!relay_satisfies("map:v2", "map"));
        assert!(!relay_satisfies("aggressive", "map"));
        assert!(!relay_satisfies("lines:1-5", "map"));
    }

    /// Records a foreign delivery for a fresh file and returns its path + key.
    fn foreign_delivery(
        relay: Option<(&str, &str)>,
    ) -> (tempfile::TempDir, String, super::DeliveryFingerprint) {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("foreign.rs");
        std::fs::write(&file, "pub fn foreign() {}\npub fn other() {}\n").unwrap();
        let path = file.to_string_lossy().to_string();
        let fp = super::file_blake3_prefix(&path).unwrap();
        crate::core::ocla::OclaRegistry::global()
            .delivery_registry
            .record_delivery(crate::core::ocla::types::DeliveryEntry {
                blake3: fp.hash,
                path: path.clone(),
                line_count: fp.line_count,
                token_count: 40,
                agent_id: "claude-foreign".into(),
                conversation_id: "conv-foreign".into(),
                mtime: fp.mtime,
                relay_content: relay.map(|(c, _)| c.to_string()),
                relay_mode: relay.map(|(_, m)| m.to_string()),
            });
        (dir, path, fp)
    }

    #[test]
    fn content_free_stub_withheld_for_another_conversation() {
        // #1909: agent B in a new conversation must get content, never a stub
        // claiming content that only agent A's context holds.
        if !crate::core::conversation::scope_enabled() {
            return; // legacy mode (LEAN_CTX_CONVERSATION_SCOPE=0) keeps the stub by contract
        }
        let (_dir, path, fp) = foreign_delivery(None);
        for mode in ["auto", "signatures", "map", "aggressive"] {
            assert!(
                try_cross_agent_stub(&path, mode, fp.hash, fp.mtime).is_none(),
                "content-free cross-agent stub leaked for mode={mode}"
            );
        }
    }

    #[test]
    fn relay_served_only_for_matching_mode() {
        let relay = "pub fn foreign() {}";
        let (_dir, path, fp) = foreign_delivery(Some((relay, "map:v2")));
        let hit = try_cross_agent_stub(&path, "map", fp.hash, fp.mtime)
            .expect("map request must accept a map:v2 relay");
        assert!(hit.content.contains(relay), "{}", hit.content);
        assert!(hit.content.contains("· 2L]"), "{}", hit.content);
        assert!(
            try_cross_agent_stub(&path, "signatures", fp.hash, fp.mtime).is_none(),
            "a signatures request must not be answered with a map relay"
        );
        assert!(try_cross_agent_stub(&path, "lines:1-2", fp.hash, fp.mtime).is_none());
    }

    #[test]
    fn warm_stub_hit_records_central_telemetry() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("telemetry-hit.rs");
        std::fs::write(&file, "fn telemetry_hit() {}\n").unwrap();
        let path = file.to_string_lossy();
        let mut cache = SessionCache::new();
        cache.store(&path, "fn telemetry_hit() {}\n");
        cache.mark_full_delivered(&path);

        let metrics = crate::core::telemetry::global_metrics();
        let before = metrics.cache_hits.load(Ordering::Relaxed);
        let output = try_stub_hit_readonly_scoped(&cache, &path, None);
        let after = metrics.cache_hits.load(Ordering::Relaxed);

        assert!(output.is_some(), "warm re-read must use the stub cache");
        assert!(
            after > before,
            "stub cache hit must increment central telemetry"
        );
    }

    #[test]
    fn relay_does_not_poison_session_cache() {
        let mut cache = SessionCache::new();
        let path = "/tmp/test_relay_poison.rs";
        cache.store(path, "original content");
        assert_eq!(
            cache
                .get(path)
                .map(|e| e.compressed_outputs.contains_key("cross-agent-relay")),
            Some(false),
            "cross-agent-relay must not exist in session cache"
        );
    }
    #[test]
    fn is_terminal_poll_file_matches_cursor_terminal_paths() {
        assert!(is_terminal_poll_file(
            "/Users/me/.cursor/projects/Users-me-proj/terminals/12345.txt"
        ));
        assert!(is_terminal_poll_file(
            "/home/dev/.cursor/projects/foo/terminals/857272.txt"
        ));
        assert!(!is_terminal_poll_file("/Users/me/project/src/main.rs"));
        assert!(!is_terminal_poll_file(
            "/Users/me/.cursor/projects/foo/agent-transcripts/abc.jsonl"
        ));
        assert!(!is_terminal_poll_file("/Users/me/.cursor/terminals.log"));
    }

    #[test]
    fn terminal_poll_file_stub_bypasses_conversation_gate() {
        let dir = tempfile::tempdir().unwrap();
        let terminals_dir = dir.path().join(".cursor").join("proj").join("terminals");
        std::fs::create_dir_all(&terminals_dir).unwrap();
        let file = terminals_dir.join("252028.txt");
        std::fs::write(&file, "---\npid: 1234\n---\nterminal output\n").unwrap();
        let path = file.to_string_lossy().to_string();

        let mut cache = SessionCache::new();
        cache.store(&path, "---\npid: 1234\n---\nterminal output\n");
        cache.mark_full_delivered(&path);

        // Mismatched conversation normally blocks stub — but terminal files bypass
        let output = try_stub_hit_readonly_scoped(&cache, &path, Some("different-conv"));
        assert!(
            output.is_some(),
            "terminal poll file must bypass conversation gate and serve stub"
        );
    }

    #[test]
    fn non_terminal_file_respects_conversation_gate() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("main.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();
        let path = file.to_string_lossy().to_string();

        let mut cache = SessionCache::new();
        cache.store(&path, "fn main() {}\n");
        cache.mark_full_delivered(&path);
        if let Some(entry) = cache.get_mut(&path) {
            entry.delivered_conversation = Some("conv-A".to_string());
        }

        // Mismatched conversation on regular file must block stub
        let output = try_stub_hit_readonly_scoped(&cache, &path, Some("conv-B"));
        assert!(
            output.is_none(),
            "regular file must respect conversation gate"
        );
    }
}
