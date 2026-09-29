// SPDX-License-Identifier: Apache-2.0

//! Setup dimensions read from the build and the host at send time.

use super::super::telemetry_v2::{
    ClientFamily, DistributionChannel, EmbeddingsState, IntegrationMode, SetupProfileMetrics,
};

pub(super) fn distribution_channel() -> DistributionChannel {
    match option_env!("LEAN_CTX_DISTRIBUTION_CHANNEL") {
        Some("cargo") => DistributionChannel::Cargo,
        Some("homebrew") => DistributionChannel::Homebrew,
        Some("npm") => DistributionChannel::Npm,
        Some("docker") => DistributionChannel::Docker,
        Some("source") => DistributionChannel::Source,
        _ => DistributionChannel::Unknown,
    }
}

pub(super) fn setup_profile() -> SetupProfileMetrics {
    let integration_mode = match crate::core::config::Config::load().hook_mode_override() {
        None => IntegrationMode::Default,
        Some(crate::hooks::HookMode::Mcp) => IntegrationMode::Mcp,
        Some(crate::hooks::HookMode::Hybrid) => IntegrationMode::Hybrid,
        Some(crate::hooks::HookMode::Replace) => IntegrationMode::Replace,
    };
    SetupProfileMetrics {
        integration_mode,
        embeddings: embeddings_state(),
    }
}

#[cfg(feature = "embeddings")]
fn embeddings_state() -> EmbeddingsState {
    if crate::core::embeddings::EmbeddingEngine::is_available() {
        EmbeddingsState::Installed
    } else if crate::tools::ctx_knowledge::embeddings_auto_download_allowed() {
        EmbeddingsState::NotInstalled
    } else {
        EmbeddingsState::Disabled
    }
}

#[cfg(not(feature = "embeddings"))]
fn embeddings_state() -> EmbeddingsState {
    EmbeddingsState::Unsupported
}

/// Client that drives this installation: the MCP handshake of this process,
/// then the last handshake persisted by any process (the daemon and CLI never
/// see one themselves), then the host's environment variables.
pub(super) fn client_family() -> ClientFamily {
    const PERSISTED_MAX_AGE_SECS: u64 = 7 * 24 * 60 * 60;
    let handshake = Some(crate::core::client_capabilities::current().client_id)
        .filter(|id| id != "unknown")
        .or_else(|| {
            crate::core::client_capabilities::load_persisted(PERSISTED_MAX_AGE_SECS)
                .map(|caps| caps.client_id)
        });
    if let Some(family) = handshake.as_deref().and_then(ClientFamily::from_client_id) {
        return family;
    }
    if std::env::var_os("CLAUDECODE").is_some() {
        ClientFamily::Claude
    } else if std::env::var_os("CODEX_HOME").is_some() {
        ClientFamily::Codex
    } else if std::env::var_os("CURSOR_TRACE_ID").is_some() {
        ClientFamily::Cursor
    } else if std::env::var_os("GEMINI_CLI").is_some() {
        ClientFamily::Gemini
    } else {
        ClientFamily::Other
    }
}
