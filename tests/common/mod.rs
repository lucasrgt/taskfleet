#![allow(dead_code)]

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::{Value, json};
use tempfile::TempDir;

pub struct Fixture {
    pub temp: TempDir,
    pub repo: PathBuf,
    pub config: PathBuf,
}

impl Fixture {
    pub fn new(extra: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repository");
        fs::create_dir(&repo).unwrap();
        run(&repo, &["init", "-b", "main"]);
        run(&repo, &["config", "user.name", "Taskfleet Test"]);
        run(&repo, &["config", "user.email", "taskfleet@example.test"]);
        fs::write(repo.join("seed.txt"), "seed\n").unwrap();
        run(&repo, &["add", "."]);
        run(&repo, &["commit", "-m", "seed"]);
        let config = repo.join("taskfleet.toml");
        fs::write(
            &config,
            format!(
                r#"schema = 1
[project]
repository = "."
database = ".taskfleet/state.sqlite"
worktree_root = "../worktrees"
{extra}
"#
            ),
        )
        .unwrap();
        Self { temp, repo, config }
    }

    pub fn service(&self) -> taskfleet::Service {
        taskfleet::Service::open(&self.config).unwrap()
    }
}

pub fn task(uri: &str, title: &str, platform: &str) -> Value {
    json!({"uri":uri,"title":title,"description":"complete dossier","tags":["backend"],"priority":"high","source":{"provider":"fixture"},"meta":{"platform":platform,"points":3},"depends_on":[]})
}

pub fn ingest(service: &mut taskfleet::Service, tasks: Vec<Value>) {
    service.call("task.ingest", &json!({"tasks":tasks})).unwrap();
}

pub fn run(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git").arg("-C").arg(repo).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

pub fn commit(worktree: &Path, file: &str, body: &str) {
    fs::write(worktree.join(file), body).unwrap();
    run(worktree, &["add", "."]);
    run(worktree, &["commit", "-m", &format!("change {file}")]);
}
