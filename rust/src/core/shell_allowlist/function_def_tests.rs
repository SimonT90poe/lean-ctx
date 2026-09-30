//! Shell function definitions and inline-env error text (#1488, #1489).
//! Split out of `tests.rs` to keep that file under the LOC gate.

use super::*;

// ---------------------------------------------------------------------------
// #1488: shell function definitions must not be blocked by the allowlist.
// ---------------------------------------------------------------------------

#[test]
fn gh1488_function_definition_not_blocked() {
    let allowlist = allow(&["echo"]);
    let result = check_all_segments("greet() { echo hi; }; greet", &allowlist);
    assert!(
        result.is_ok(),
        "function definition + call should not be blocked: {result:?}"
    );
}

#[test]
fn gh1488_function_with_disallowed_body_is_blocked() {
    let allowlist = allow(&["echo", "ls"]);
    let result = check_all_segments("bad() { evil_command; }; bad", &allowlist);
    assert!(
        result.is_err(),
        "function with disallowed body command must be blocked"
    );
}

#[test]
fn gh1488_detect_function_def_forms() {
    use super::super::tokenizer::detect_function_def;
    assert_eq!(
        detect_function_def("greet() { echo hi; }"),
        Some("greet".into())
    );
    assert_eq!(
        detect_function_def("function greet { echo hi; }"),
        Some("greet".into())
    );
    assert_eq!(
        detect_function_def("function greet() { echo hi; }"),
        Some("greet".into())
    );
    assert_eq!(detect_function_def("echo hello"), None);
    assert_eq!(detect_function_def("ls -la"), None);
}

#[test]
fn gh1488_extract_function_body_commands() {
    use super::super::tokenizer::extract_function_body_commands;
    let cmds = extract_function_body_commands("greet() { echo hi; echo bye; }");
    assert_eq!(cmds, vec!["echo hi", "echo bye"]);
}

// ---------------------------------------------------------------------------
// #1489: inline env override error must mention the `env` parameter.
// ---------------------------------------------------------------------------

#[test]
fn gh1489_inline_env_block_message_mentions_env_parameter() {
    let result = check_all_segments("PATH=/evil/bin echo hi", &allow(&["echo"]));
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("env parameter"),
        "error must mention `env` parameter: {err}"
    );
    assert!(
        err.contains("ctx_shell"),
        "error must mention ctx_shell: {err}"
    );
}
