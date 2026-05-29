use assert_cmd::Command;
use std::path::Path;
use tempfile::TempDir;

pub struct TestRepo {
    pub temp_dir: TempDir,
}

impl TestRepo {
    pub fn new_git() -> Self {
        let temp_dir = tempfile::tempdir().expect("create temp dir");
        git(temp_dir.path(), &["init"]);
        git(
            temp_dir.path(),
            &["config", "user.email", "forge@example.test"],
        );
        git(temp_dir.path(), &["config", "user.name", "Forge Test"]);
        std::fs::write(temp_dir.path().join("README.md"), "hello\n").expect("write readme");
        git(temp_dir.path(), &["add", "README.md"]);
        git(temp_dir.path(), &["commit", "-m", "initial"]);
        Self { temp_dir }
    }

    pub fn path(&self) -> &Path {
        self.temp_dir.path()
    }

    // Not every integration-test crate that includes `common` drives the CLI
    // through assert_cmd (the concurrency suite spawns raw processes), so this is
    // dead in some compilation units.
    #[allow(dead_code)]
    pub fn forge(&self) -> Command {
        let mut command = Command::cargo_bin("forge").expect("forge binary");
        command.current_dir(self.path());
        command
    }
}

#[allow(dead_code)]
pub fn forge_in(path: &Path) -> Command {
    let mut command = Command::cargo_bin("forge").expect("forge binary");
    command.current_dir(path);
    command
}

fn git(cwd: &Path, args: &[&str]) {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}
