fn main() {
    guard_source_contamination();
    // Watch the whole source tree: the contamination guard must re-run on any
    // source edit. Never point this at a file that may not exist — a missing
    // path marks the build script stale on every invocation and recompiles the
    // whole crate each time (it pointed at a deleted dashboard.html for months).
    println!("cargo::rerun-if-changed=src");
}

fn guard_source_contamination() {
    let src = std::path::Path::new("src");
    if !src.is_dir() {
        return;
    }
    let mut contaminated = Vec::new();
    visit_rs_files(src, &mut contaminated);
    if !contaminated.is_empty() {
        let list = contaminated.join(
            "
  ",
        );
        panic!(
            "

[1;31mBUILD BLOCKED: lean-ctx marker contamination detected[0m

             The following source files contain `--- lean-ctx:` lines injected
             by shell hooks during in-place editing. Remove them before building:

               {list}

             Prevention: use StrReplace (not perl/sed) for source edits, or set
             LEAN_CTX_SHELL_PASSTHROUGH=1 before running in-place edit commands.
"
        );
    }
}

fn visit_rs_files(dir: &std::path::Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            visit_rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs")
            && let Ok(text) = std::fs::read_to_string(&path)
            && text.lines().any(|line| line.starts_with("--- lean-ctx:"))
        {
            out.push(path.display().to_string());
        }
    }
}
