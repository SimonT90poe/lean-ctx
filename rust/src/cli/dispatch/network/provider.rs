//! `lean-ctx provider …` — provider scaffolding, sync, OAuth.

/// Reads a `--data-source <id>` / `--data-source=<id>` flag, defaulting to "jira".
fn data_source_flag(args: &[String]) -> String {
    args.iter()
        .enumerate()
        .find_map(|(i, a)| {
            if let Some(v) = a.strip_prefix("--data-source=") {
                return Some(v.to_string());
            }
            if a == "--data-source" {
                return args.get(i + 1).cloned();
            }
            None
        })
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "jira".to_string())
}

fn slugify_id(raw: &str) -> Option<String> {
    let id: String = raw
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if id.is_empty() { None } else { Some(id) }
}

fn provider_usage() {
    println!(
        "Usage: lean-ctx provider <command>\n\n\
         Commands:\n  \
         init <id> [--force]                Scaffold a config provider in .lean-ctx/providers/\n  \
         auth jira [--data-source <id>]     Connect a Jira Cloud site via OAuth 2.0 (3LO)\n  \
         logout jira [--data-source <id>]   Remove stored Jira OAuth credentials\n  \
         list                               List connected Jira OAuth data sources\n\n\
         Jira OAuth requires your own Atlassian app credentials in the environment:\n  \
         JIRA_OAUTH_CLIENT_ID, JIRA_OAUTH_CLIENT_SECRET\n  \
         (optional) JIRA_OAUTH_SCOPES — default: \"read:jira-work read:jira-user offline_access\"\n\n\
         Register a free app at https://developer.atlassian.com/console/myapps/"
    );
}

/// `provider init <id>` — scaffold a config-provider TOML in the project-local
/// `.lean-ctx/providers/` directory the discovery layer auto-loads (P4 DX).
fn provider_init(args: &[String]) {
    use crate::core::providers::scaffold;

    let force = args.iter().any(|a| a == "--force" || a == "-f");
    let Some(raw) = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .map(String::as_str)
    else {
        eprintln!("Usage: lean-ctx provider init <id> [--force]");
        std::process::exit(1);
    };
    // Provider ids share the addon slug shape (`[a-z0-9-]`).
    let Some(id) = slugify_id(raw) else {
        eprintln!("`{raw}` has no usable id characters ([a-z0-9-]).");
        std::process::exit(1);
    };

    let dir = std::path::Path::new(scaffold::PROVIDERS_SUBDIR);
    let path = dir.join(format!("{id}.toml"));
    if path.exists() && !force {
        eprintln!("{} already exists. Re-run with --force.", path.display());
        std::process::exit(1);
    }
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("Error creating {}: {e}", dir.display());
        std::process::exit(1);
    }
    if let Err(e) = std::fs::write(&path, scaffold::provider_config(&id)) {
        eprintln!("Error writing {}: {e}", path.display());
        std::process::exit(1);
    }
    println!("✓ Wrote {} (provider `{id}`).", path.display());
    println!("\nNext:");
    println!("  1. Edit base_url, [auth] and [resources] for your API.");
    println!("  2. Export the token env var referenced under [auth].");
    println!("  3. It is auto-discovered — query it via ctx_provider / ctx_semantic_search.");
}

pub(crate) fn cmd_provider(rest: &[String]) {
    use crate::core::providers::jira_oauth;

    let sub = rest.first().map_or("help", std::string::String::as_str);
    match sub {
        "init" | "new" => provider_init(&rest[1..]),
        "auth" | "login" | "connect" => {
            let target = rest.get(1).map_or("", std::string::String::as_str);
            if !target.eq_ignore_ascii_case("jira") {
                eprintln!("Only 'jira' is supported for OAuth today.\n");
                provider_usage();
                std::process::exit(1);
            }
            let args: &[String] = if rest.len() > 2 { &rest[2..] } else { &[] };
            let data_source = data_source_flag(args);
            match jira_oauth::run_auth_flow(&data_source) {
                Ok(()) => {}
                Err(e) => {
                    eprintln!("\x1b[31m✗\x1b[0m Jira OAuth failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        "logout" | "disconnect" => {
            let target = rest.get(1).map_or("", std::string::String::as_str);
            if !target.eq_ignore_ascii_case("jira") {
                provider_usage();
                std::process::exit(1);
            }
            let args: &[String] = if rest.len() > 2 { &rest[2..] } else { &[] };
            let data_source = data_source_flag(args);
            match jira_oauth::remove_credential(&data_source) {
                Ok(true) => {
                    println!(
                        "\x1b[32m✓\x1b[0m Removed Jira OAuth credentials for '{data_source}'."
                    );
                }
                Ok(false) => {
                    println!("No stored Jira OAuth credentials for '{data_source}'.");
                }
                Err(e) => {
                    eprintln!("\x1b[31m✗\x1b[0m {e}");
                    std::process::exit(1);
                }
            }
        }
        "list" | "ls" | "status" => {
            let conns = jira_oauth::list_connections();
            if conns.is_empty() {
                println!("No Jira OAuth data sources connected. Run: lean-ctx provider auth jira");
            } else {
                println!("Connected Jira OAuth data sources:");
                for c in conns {
                    println!("  • {c}");
                }
            }
        }
        _ => provider_usage(),
    }
}
