use std::{
    env,
    path::{Path, PathBuf},
    process::{Command, Output},
};

const MAX_FILE_LINES: u64 = 500;
const MINIMUM_LINE_COVERAGE: u64 = 95;

fn main() {
    if let Err(error) = execute(env::args().skip(1).collect()) {
        eprintln!("verify failed: {error}");
        std::process::exit(1);
    }
}

fn execute(arguments: Vec<String>) -> Result<(), String> {
    if arguments.as_slice() != ["verify"] {
        return Err("usage: cargo xtask verify".into());
    }
    let root = repository_root()?;
    println!("Taskfleet repository verification");
    run(&root, "cargo", &["fmt", "--all", "--", "--check"])?;
    run(
        &root,
        "cargo",
        &["clippy", "--workspace", "--all-targets", "--all-features", "--", "-D", "warnings"],
    )?;
    enforce_line_budget(&root)?;
    run(&root, "cargo", &["test", "--workspace", "--all-features", "--locked"])?;
    let coverage = MINIMUM_LINE_COVERAGE.to_string();
    run(
        &root,
        "cargo",
        &[
            "llvm-cov",
            "--package",
            "taskfleet",
            "--all-features",
            "--locked",
            "--ignore-filename-regex",
            r"src[/\\]main\.rs$",
            "--fail-under-lines",
            &coverage,
        ],
    )?;
    println!("verify passed");
    Ok(())
}

fn repository_root() -> Result<PathBuf, String> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "xtask must be inside the repository".into())
}

fn run(root: &Path, program: &str, arguments: &[&str]) -> Result<(), String> {
    println!("  > {program} {}", arguments.join(" "));
    let status = Command::new(program)
        .args(arguments)
        .current_dir(root)
        .status()
        .map_err(|error| format!("could not start {program}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} exited with {status}"))
    }
}

fn enforce_line_budget(root: &Path) -> Result<(), String> {
    let output = Command::new("tokei")
        .args(["src", "--output", "json"])
        .current_dir(root)
        .output()
        .map_err(|error| format!("could not start tokei: {error}"))?;
    let (total, files) = production_lines(&output)?;
    if let Some((path, lines)) = files.into_iter().find(|(_, lines)| *lines > MAX_FILE_LINES) {
        return Err(format!("production file budget exceeded: {path} has {lines}/{MAX_FILE_LINES} lines"));
    }
    println!("    production lines: {total} (max {MAX_FILE_LINES} code lines per file)");
    Ok(())
}

fn production_lines(output: &Output) -> Result<(u64, Vec<(String, u64)>), String> {
    if !output.status.success() {
        return Err(format!("tokei exited with {}", output.status));
    }
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).map_err(|error| format!("invalid tokei JSON: {error}"))?;
    let rust = report.get("Rust").ok_or("tokei JSON did not contain Rust")?;
    let total = rust["code"].as_u64().ok_or("tokei JSON did not contain Rust.code")?;
    let files = rust["reports"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| Some((item["name"].as_str()?.to_owned(), item["stats"]["code"].as_u64()?)))
        .collect();
    Ok((total, files))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::ExitStatus;

    #[test]
    fn parses_report() {
        let output = Output {
            status: success(),
            stdout: br#"{"Rust":{"code":12,"reports":[{"name":"src/lib.rs","stats":{"code":12}}]}}"#.to_vec(),
            stderr: vec![],
        };
        assert_eq!(production_lines(&output).unwrap(), (12, vec![("src/lib.rs".into(), 12)]));
    }

    #[cfg(unix)]
    fn success() -> ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        ExitStatus::from_raw(0)
    }
    #[cfg(windows)]
    fn success() -> ExitStatus {
        use std::os::windows::process::ExitStatusExt;
        ExitStatus::from_raw(0)
    }
}
