use crate::core::agent_connector;

pub(crate) fn cmd_pair(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        return 0;
    }
    let code = first_positional(args);
    let Some(code) = code else {
        eprintln!("error: pair code required\n  usage: lean-ctx pair LCTX-XXXX");
        return 1;
    };
    if !is_valid_pair_code(code) {
        eprintln!("error: invalid pair code '{code}'\n  expected: LCTX-XXXX");
        return 1;
    }

    eprintln!("lean-ctx pair {code}");
    eprintln!("  Version: {}", env!("CARGO_PKG_VERSION"));
    eprintln!(
        "  OS:      {}/{}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    eprintln!();

    let connectors = agent_connector::detect_and_create_connectors();
    eprintln!("  Detected agents:");
    for c in &connectors {
        let info = c.info();
        let st = if info.available {
            "\u{2713}"
        } else {
            "\u{2717}"
        };
        eprintln!(
            "    {st} {} {}",
            display_agent_name(c.name()),
            info.version.as_deref().unwrap_or("?")
        );
    }
    eprintln!();
    eprintln!("  WebSocket pairing not yet implemented \u{2014} coming in v3.10");
    0
}

fn is_valid_pair_code(code: &str) -> bool {
    code.starts_with("LCTX-")
        && code.len() == 9
        && code[5..].chars().all(|c| c.is_ascii_alphanumeric())
}
fn display_agent_name(n: &str) -> &str {
    match n {
        "codex" => "Codex",
        "claude" | "claude-code" => "Claude Code",
        "cursor" => "Cursor",
        _ => n,
    }
}

fn first_positional(args: &[String]) -> Option<&str> {
    let mut skip = false;
    for arg in args {
        if skip {
            skip = false;
            continue;
        }
        if arg == "--server" {
            skip = true;
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        return Some(arg.as_str());
    }
    None
}

fn print_help() {
    println!(
        "\
lean-ctx pair \u{2014} pair with leanctx.com for remote benchmarks

USAGE:
    lean-ctx pair <CODE>

ARGS:
    CODE    Pairing code from leanctx.com (format: LCTX-XXXX)

EXAMPLES:
    lean-ctx pair LCTX-A9F2"
    );
}
