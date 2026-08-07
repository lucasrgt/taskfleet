use std::{env, fs, path::Path, process::ExitCode};

fn main() -> ExitCode {
    let root = env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
    let manifest_path = root.join("app").join("manifest.json");
    let Ok(raw) = fs::read_to_string(&manifest_path) else {
        eprintln!("missing {}", manifest_path.display());
        return ExitCode::from(1);
    };
    if !raw.contains("\"name\"") || !raw.contains("\"required_features\"") {
        eprintln!("manifest missing required fields");
        return ExitCode::from(1);
    }
    for feature in ["health", "auth"] {
        let path = root.join("app").join("features").join(format!("{feature}.txt"));
        if !path.is_file() {
            eprintln!("missing feature surface {}", path.display());
            return ExitCode::from(1);
        }
        let body = fs::read_to_string(&path).unwrap_or_default();
        if body.trim().is_empty() {
            eprintln!("feature surface {} is empty", path.display());
            return ExitCode::from(1);
        }
    }
    println!("miniapp verify ok");
    ExitCode::SUCCESS
}
