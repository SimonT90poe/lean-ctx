use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Canonical resolver for the current agent identity. Reads `LEAN_CTX_AGENT_ID`
/// (or legacy `LCTX_AGENT_ID`), falling back to `"local"`. Resolved once per
/// process and cached, so all subsystems (heatmap, savings ledger, audit)
/// attribute traces to the same identity.
#[must_use]
pub(crate) fn current_agent_id() -> &'static str {
    static CACHE: OnceLock<String> = OnceLock::new();
    CACHE.get_or_init(|| {
        std::env::var("LEAN_CTX_AGENT_ID")
            .or_else(|_| std::env::var("LCTX_AGENT_ID"))
            .unwrap_or_else(|_| "local".to_string())
    })
}

/// Identity of this process in the cross-agent delivery registry (#1904).
///
/// Unlike [`current_agent_id`] (a stable, shared attribution id), delivery
/// needs one id per *consumer*: the registry skips records whose `agent_id`
/// matches the requester, so two consumers sharing an id could never see each
/// other's deliveries, and a consumer matching a foreign id would be treated as
/// the reader. `CLAUDECODE` is the constant `"1"` in every Claude Code process
/// and was used verbatim before, which collapsed all Claude Code clients on a
/// machine into a single "agent".
///
/// Priority (first non-empty wins):
/// 1. `LEAN_CTX_AGENT_ID` / legacy `LCTX_AGENT_ID` — explicit operator identity,
///    shared with [`current_agent_id`]
/// 2. `CURSOR_TASK_ID` — Cursor's per-subagent id
/// 3. `claude-{pid}` when `CLAUDECODE` is set
/// 4. `codex-{pid}` when `CODEX_THREAD_ID` is set
/// 5. `local-{pid}`
///
/// Resolved once per process, so the id is stable for the process lifetime.
#[must_use]
pub(crate) fn delivery_agent_id() -> &'static str {
    static CACHE: OnceLock<String> = OnceLock::new();
    CACHE.get_or_init(|| {
        resolve_delivery_agent_id(|key| std::env::var(key).ok(), std::process::id())
    })
}

/// Pure core of [`delivery_agent_id`]: every input explicit, so the priority
/// matrix is testable without touching the process environment.
fn resolve_delivery_agent_id(env: impl Fn(&str) -> Option<String>, pid: u32) -> String {
    let non_empty = |key: &str| {
        env(key)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    if let Some(id) = non_empty("LEAN_CTX_AGENT_ID").or_else(|| non_empty("LCTX_AGENT_ID")) {
        return id;
    }
    if let Some(task) = non_empty("CURSOR_TASK_ID") {
        return task;
    }
    if non_empty("CLAUDECODE").is_some() {
        return format!("claude-{pid}");
    }
    if non_empty("CODEX_THREAD_ID").is_some() {
        return format!("codex-{pid}");
    }
    format!("local-{pid}")
}

pub(crate) fn get_or_create_keypair(agent_id: &str) -> Result<SigningKey, String> {
    let path = key_path(agent_id)?;
    if path.exists() {
        load_key(&path)
    } else {
        generate_and_save(agent_id)
    }
}

pub(crate) fn get_public_key(agent_id: &str) -> Result<VerifyingKey, String> {
    let key = get_or_create_keypair(agent_id)?;
    Ok(key.verifying_key())
}

pub(crate) fn sign_bytes(agent_id: &str, data: &[u8]) -> Result<Vec<u8>, String> {
    let key = get_or_create_keypair(agent_id)?;
    let sig = key.sign(data);
    Ok(sig.to_bytes().to_vec())
}

/// Sign `data` and return the signature together with the verifying key of
/// the SAME keypair — one atomic key-store resolution.
///
/// Callers that embed both the signature and the public key MUST use this
/// instead of separate `sign_bytes` + `get_public_key` calls: those perform
/// two independent store reads, and when the store location or key file
/// changes in between (env-driven data-dir moves under test, key
/// regeneration by a concurrent process), the embedded public key belongs to
/// a different keypair than the signature — which then can never verify.
pub(crate) fn sign_with_public_key(
    agent_id: &str,
    data: &[u8],
) -> Result<(Vec<u8>, VerifyingKey), String> {
    let key = get_or_create_keypair(agent_id)?;
    let sig = key.sign(data);
    Ok((sig.to_bytes().to_vec(), key.verifying_key()))
}

/// Sign with an already-resolved keypair (no store access). Pair with
/// [`get_or_create_keypair`] when the public key must be embedded in the
/// payload *before* the signature is computed over it.
#[must_use]
pub(crate) fn sign_bytes_with(key: &SigningKey, data: &[u8]) -> Vec<u8> {
    key.sign(data).to_bytes().to_vec()
}

pub(crate) fn verify_signature(
    public_key_bytes: &[u8],
    data: &[u8],
    signature_bytes: &[u8],
) -> bool {
    let pk_bytes: [u8; 32] = match public_key_bytes.try_into() {
        Ok(b) => b,
        Err(_) => return false,
    };
    let Ok(verifying_key) = VerifyingKey::from_bytes(&pk_bytes) else {
        return false;
    };
    let sig_bytes: [u8; 64] = match signature_bytes.try_into() {
        Ok(b) => b,
        Err(_) => return false,
    };
    let signature = Signature::from_bytes(&sig_bytes);
    verifying_key.verify(data, &signature).is_ok()
}

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

pub(crate) fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) {
        return Err("odd-length hex string".to_string());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

fn key_path(agent_id: &str) -> Result<PathBuf, String> {
    let base = crate::core::data_dir::lean_ctx_data_dir()?;
    Ok(base.join("keys").join(format!("{agent_id}.key")))
}

fn pub_key_path(agent_id: &str) -> Result<PathBuf, String> {
    let base = crate::core::data_dir::lean_ctx_data_dir()?;
    Ok(base.join("keys").join(format!("{agent_id}.pub")))
}

fn generate_and_save(agent_id: &str) -> Result<SigningKey, String> {
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|e| format!("CSPRNG unavailable: {e}"))?;
    let signing_key = SigningKey::from_bytes(&seed);

    let key_file = key_path(agent_id)?;
    let pub_file = pub_key_path(agent_id)?;

    if let Some(parent) = key_file.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir keys: {e}"))?;
    }

    std::fs::write(&key_file, signing_key.to_bytes()).map_err(|e| format!("write key: {e}"))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        let _ = std::fs::set_permissions(&key_file, perms);
    }

    let pub_bytes = signing_key.verifying_key().to_bytes();
    std::fs::write(&pub_file, pub_bytes).map_err(|e| format!("write pub: {e}"))?;

    Ok(signing_key)
}

fn load_key(path: &Path) -> Result<SigningKey, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read key: {e}"))?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| "invalid key file (expected 32 bytes)".to_string())?;
    Ok(SigningKey::from_bytes(&arr))
}

/// Derive an Ed25519 keypair deterministically from a recovery phrase.
/// Same phrase always produces the same key, enabling identity recovery
/// across reinstalls and machines.
pub(crate) fn derive_keypair_from_phrase(phrase: &str) -> Result<SigningKey, String> {
    use argon2::Argon2;

    let normalized = phrase.trim().to_lowercase();
    let salt = b"lean-ctx-leaderboard-v1";
    let params =
        argon2::Params::new(65536, 3, 1, Some(32)).map_err(|e| format!("argon2 params: {e}"))?;
    let argon2 = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut seed = [0u8; 32];
    argon2
        .hash_password_into(normalized.as_bytes(), salt, &mut seed)
        .map_err(|e| format!("argon2 hash: {e}"))?;
    Ok(SigningKey::from_bytes(&seed))
}

/// Generate a 4-word recovery phrase from the BIP39 wordlist.
pub(crate) fn generate_recovery_phrase() -> String {
    let mut buf = [0u8; 8];
    getrandom::fill(&mut buf).expect("CSPRNG unavailable");
    (0..4)
        .map(|i| {
            let idx = u16::from_le_bytes([buf[i * 2], buf[i * 2 + 1]]) as usize % 2048;
            super::wordlist::WORDLIST[idx]
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Import a phrase-derived identity: derives the keypair and saves it to disk,
/// replacing any existing key for the given agent_id.
pub(crate) fn import_phrase_identity(agent_id: &str, phrase: &str) -> Result<SigningKey, String> {
    let signing_key = derive_keypair_from_phrase(phrase)?;

    let kp = key_path(agent_id)?;
    let pp = pub_key_path(agent_id)?;
    if let Some(parent) = kp.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir keys: {e}"))?;
    }
    std::fs::write(&kp, signing_key.to_bytes()).map_err(|e| format!("write key: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&kp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::write(&pp, signing_key.verifying_key().to_bytes())
        .map_err(|e| format!("write pub: {e}"))?;

    // Persist phrase for dashboard "Show phrase" feature.
    let phrase_path = kp.with_extension("phrase");
    let _ = std::fs::write(&phrase_path, phrase.trim());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&phrase_path, std::fs::Permissions::from_mode(0o600));
    }

    Ok(signing_key)
}

/// Read the stored recovery phrase for an agent, if one exists.
pub(crate) fn stored_recovery_phrase(agent_id: &str) -> Option<String> {
    let kp = key_path(agent_id).ok()?;
    let phrase_path = kp.with_extension("phrase");
    std::fs::read_to_string(phrase_path)
        .ok()
        .filter(|s| !s.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key| owned.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    #[test]
    fn delivery_id_two_claude_processes_are_distinct() {
        // #1904: both processes see CLAUDECODE=1; the id must still differ.
        let env = env_of(&[("CLAUDECODE", "1")]);
        let a = resolve_delivery_agent_id(&env, 100);
        let b = resolve_delivery_agent_id(&env, 200);
        assert_eq!(a, "claude-100");
        assert_ne!(a, b, "two Claude Code clients must not share an identity");
        assert_ne!(a, "1", "the constant CLAUDECODE value is not an identity");
    }

    #[test]
    fn delivery_id_priority_matrix() {
        let explicit = env_of(&[
            ("LEAN_CTX_AGENT_ID", "ops-agent"),
            ("CURSOR_TASK_ID", "t1"),
            ("CLAUDECODE", "1"),
        ]);
        assert_eq!(resolve_delivery_agent_id(&explicit, 7), "ops-agent");
        let legacy = env_of(&[("LCTX_AGENT_ID", "legacy"), ("CLAUDECODE", "1")]);
        assert_eq!(resolve_delivery_agent_id(&legacy, 7), "legacy");
        let cursor = env_of(&[("CURSOR_TASK_ID", "t1"), ("CLAUDECODE", "1")]);
        assert_eq!(resolve_delivery_agent_id(&cursor, 7), "t1");
        let codex = env_of(&[("CODEX_THREAD_ID", "th")]);
        assert_eq!(resolve_delivery_agent_id(&codex, 7), "codex-7");
        assert_eq!(resolve_delivery_agent_id(env_of(&[]), 7), "local-7");
    }

    #[test]
    fn delivery_id_ignores_blank_values() {
        let env = env_of(&[("LEAN_CTX_AGENT_ID", "  "), ("CURSOR_TASK_ID", "")]);
        assert_eq!(resolve_delivery_agent_id(env, 9), "local-9");
    }

    #[test]
    fn delivery_id_is_stable_within_process() {
        assert_eq!(delivery_agent_id(), delivery_agent_id());
    }
    #[test]
    fn sign_and_verify_roundtrip() {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).unwrap();
        let key = SigningKey::from_bytes(&seed);
        let data = b"test payload";
        let sig = key.sign(data);

        let pub_bytes = key.verifying_key().to_bytes();
        assert!(verify_signature(&pub_bytes, data, &sig.to_bytes()));
    }

    #[test]
    fn verify_rejects_tampered_data() {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).unwrap();
        let key = SigningKey::from_bytes(&seed);
        let sig = key.sign(b"original");

        let pub_bytes = key.verifying_key().to_bytes();
        assert!(!verify_signature(&pub_bytes, b"tampered", &sig.to_bytes()));
    }

    #[test]
    fn hex_roundtrip() {
        let data = vec![0xde, 0xad, 0xbe, 0xef];
        let encoded = hex_encode(&data);
        assert_eq!(encoded, "deadbeef");
        let decoded = hex_decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn phrase_derivation_is_deterministic() {
        let phrase = "abandon ability able about";
        let key1 = derive_keypair_from_phrase(phrase).unwrap();
        let key2 = derive_keypair_from_phrase(phrase).unwrap();
        assert_eq!(key1.to_bytes(), key2.to_bytes());
        assert_eq!(
            key1.verifying_key().to_bytes(),
            key2.verifying_key().to_bytes()
        );
    }

    #[test]
    fn phrase_derivation_is_case_insensitive() {
        let lower = derive_keypair_from_phrase("abandon ability able about").unwrap();
        let upper = derive_keypair_from_phrase("ABANDON ABILITY ABLE ABOUT").unwrap();
        let mixed = derive_keypair_from_phrase("Abandon Ability Able About").unwrap();
        assert_eq!(lower.to_bytes(), upper.to_bytes());
        assert_eq!(lower.to_bytes(), mixed.to_bytes());
    }

    #[test]
    fn phrase_derivation_trims_whitespace() {
        let clean = derive_keypair_from_phrase("abandon ability able about").unwrap();
        let padded = derive_keypair_from_phrase("  abandon ability able about  ").unwrap();
        assert_eq!(clean.to_bytes(), padded.to_bytes());
    }

    #[test]
    fn different_phrases_produce_different_keys() {
        let key1 = derive_keypair_from_phrase("abandon ability able about").unwrap();
        let key2 = derive_keypair_from_phrase("zoo zero zone youth").unwrap();
        assert_ne!(key1.to_bytes(), key2.to_bytes());
    }

    #[test]
    fn generated_phrase_has_four_words() {
        let phrase = generate_recovery_phrase();
        let words: Vec<_> = phrase.split_whitespace().collect();
        assert_eq!(words.len(), 4);
        for word in &words {
            assert!(
                crate::core::wordlist::WORDLIST.contains(word),
                "word \"{word}\" not in BIP39 wordlist"
            );
        }
    }

    #[test]
    fn phrase_sign_verify_roundtrip() {
        let phrase = "abandon ability able about";
        let key = derive_keypair_from_phrase(phrase).unwrap();
        let data = b"test data for leaderboard";
        let sig = sign_bytes_with(&key, data);
        let pub_bytes = key.verifying_key().to_bytes();
        assert!(verify_signature(&pub_bytes, data, &sig));
    }

    #[test]
    fn import_phrase_identity_roundtrip() {
        let _isolated = crate::core::data_dir::isolated_data_dir();
        let phrase = "abandon ability able about";
        let key = import_phrase_identity("test-rejoin", phrase).unwrap();

        // Stored phrase is readable
        let stored = stored_recovery_phrase("test-rejoin");
        assert_eq!(stored.as_deref(), Some(phrase));

        // Loading the key from disk produces the same keypair
        let loaded = get_or_create_keypair("test-rejoin").unwrap();
        assert_eq!(key.to_bytes(), loaded.to_bytes());
    }
}
