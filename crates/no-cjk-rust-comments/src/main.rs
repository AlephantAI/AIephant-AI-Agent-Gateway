use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

use no_cjk_rust_comments::{any_comment_contains_han, diagnostics_for_comments_with_han};

fn main() -> std::process::ExitCode {
    let root = std::env::args_os()
        .nth(1)
        .unwrap_or_else(|| OsString::from("."));

    let root = PathBuf::from(root);
    let mut violations: Vec<String> = Vec::new();

    for path in collect_rs_files(&root) {
        let Ok(src) = fs::read_to_string(&path) else {
            eprintln!("skip (read error): {}", path.display());
            continue;
        };
        if any_comment_contains_han(&src) {
            let rel = path.strip_prefix(&root).unwrap_or(&path);
            let d = diagnostics_for_comments_with_han(&src, 8);
            if d.is_empty() {
                violations.push(format!(
                    "{}:1: comment contains Han (no line detail)",
                    rel.display()
                ));
            } else {
                for (line, excerpt) in d {
                    violations.push(format!(
                        "{}:{}: comment contains Han: {}",
                        rel.display(),
                        line,
                        excerpt
                    ));
                }
            }
        }
    }

    if violations.is_empty() {
        std::process::ExitCode::SUCCESS
    } else {
        eprintln!("no-cjk-rust-comments: Han found inside comment tokens:\n");
        for v in &violations {
            eprintln!("{v}");
        }
        std::process::ExitCode::from(1)
    }
}

fn collect_rs_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(root, &mut out);
    out.sort();
    out
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(read) = fs::read_dir(dir) else {
        return;
    };
    for ent in read.flatten() {
        let p = ent.path();
        let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if p.is_dir() {
            if name == "target" || name == ".git" || name == "node_modules" {
                continue;
            }
            walk(&p, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(p);
        }
    }
}
