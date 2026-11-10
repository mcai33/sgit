use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};
use tempfile::TempDir;

struct Fixture {
    temp: TempDir,
    remote: PathBuf,
    a: PathBuf,
    b: PathBuf,
}

fn checked(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn git(dir: &Path, args: &[&str]) -> String {
    checked(
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", null_device())
            .output()
            .unwrap(),
    )
}

fn null_device() -> &'static str {
    if cfg!(windows) { "NUL" } else { "/dev/null" }
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        git(root, &["init", "--bare", "-b", "main", "remote.git"]);
        git(root, &["clone", "remote.git", "device-a"]);
        let a = root.join("device-a");
        git(&a, &["config", "user.name", "sgit test"]);
        git(&a, &["config", "user.email", "test@example.invalid"]);
        fs::write(a.join("code.txt"), "first\n").unwrap();
        git(&a, &["add", "code.txt"]);
        git(&a, &["commit", "-m", "initial"]);
        git(&a, &["push", "-u", "origin", "main"]);
        git(root, &["clone", "remote.git", "device-b"]);
        let b = root.join("device-b");
        git(&b, &["config", "user.name", "sgit test"]);
        git(&b, &["config", "user.email", "test@example.invalid"]);
        Self {
            remote: root.join("remote.git"),
            temp,
            a,
            b,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_sgit"))
            .args(args)
            .current_dir(self.temp.path())
            .env("SGIT_CONFIG_DIR", self.temp.path().join("config"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", null_device())
            .output()
            .unwrap()
    }

    fn add(&self, path: &Path) {
        let output = checked(self.run(&["add", path.to_str().unwrap()]));
        assert!(output.contains(path.file_name().unwrap().to_str().unwrap()));
    }

    fn edit_and_commit(&self, path: &Path, contents: &str) {
        fs::write(path.join("code.txt"), contents).unwrap();
        git(path, &["commit", "-am", "edit"]);
    }
}

#[test]
fn pushes_tracked_changes_and_explicitly_staged_new_files_then_pulls_on_another_device() {
    let repo = Fixture::new();
    repo.add(&repo.a);
    repo.add(&repo.b);
    fs::write(repo.a.join("code.txt"), "from A\n").unwrap();
    fs::write(repo.a.join("staged.txt"), "include\n").unwrap();
    fs::write(repo.a.join(".env"), "local secret\n").unwrap();
    git(&repo.a, &["add", "staged.txt"]);
    checked(repo.run(&["push", "device-a"]));
    checked(repo.run(&["pull", "device-b"]));
    assert_eq!(
        fs::read_to_string(repo.b.join("code.txt")).unwrap(),
        "from A\n"
    );
    assert!(repo.b.join("staged.txt").is_file());
    assert!(!repo.b.join(".env").exists());
    let head = git(&repo.a, &["rev-parse", "HEAD"]);
    checked(repo.run(&["push", "device-a"]));
    assert_eq!(git(&repo.a, &["rev-parse", "HEAD"]), head);
}

#[test]
fn divergence_blocks_push_without_touching_worktree_or_index() {
    let repo = Fixture::new();
    repo.add(&repo.a);
    repo.edit_and_commit(&repo.a, "A\n");
    repo.edit_and_commit(&repo.b, "B\n");
    git(&repo.b, &["push"]);
    fs::write(repo.a.join("code.txt"), "local changes\n").unwrap();
    let head = git(&repo.a, &["rev-parse", "HEAD"]);
    let status = git(&repo.a, &["status", "--porcelain"]);
    assert!(!repo.run(&["push", "device-a"]).status.success());
    assert_eq!(git(&repo.a, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&repo.a, &["status", "--porcelain"]), status);
    assert_eq!(
        fs::read_to_string(repo.a.join("code.txt")).unwrap(),
        "local changes\n"
    );
}

#[test]
fn failed_push_keeps_commit_and_retry_reuses_it() {
    let repo = Fixture::new();
    repo.add(&repo.a);
    let bad_destination = repo.temp.path().join("does not exist");
    git(
        &repo.a,
        &[
            "remote",
            "set-url",
            "--push",
            "origin",
            bad_destination.to_str().unwrap(),
        ],
    );
    fs::write(repo.a.join("code.txt"), "saved locally\n").unwrap();
    let failed = repo.run(&["push", "device-a"]);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stdout).contains("committed_pending_push"));
    let head = git(&repo.a, &["rev-parse", "HEAD"]);
    assert_eq!(git(&repo.a, &["show", "HEAD:code.txt"]), "saved locally\n");
    git(
        &repo.a,
        &[
            "remote",
            "set-url",
            "--push",
            "origin",
            repo.remote.to_str().unwrap(),
        ],
    );
    checked(repo.run(&["push", "device-a"]));
    assert_eq!(git(&repo.a, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(&repo.remote, &["rev-parse", "main"]), head);
}

#[test]
fn batch_pull_reports_blocked_repo_and_updates_other_repos() {
    let repo = Fixture::new();
    repo.add(&repo.a);
    repo.add(&repo.b);
    git(repo.temp.path(), &["clone", "remote.git", "device-c"]);
    let c = repo.temp.path().join("device-c");
    repo.add(&c);
    repo.edit_and_commit(&repo.b, "from B\n");
    git(&repo.b, &["push"]);
    fs::write(repo.a.join("code.txt"), "unsaved work\n").unwrap();
    let batch = repo.run(&["pull", "--all"]);
    assert!(!batch.status.success());
    assert_eq!(
        fs::read_to_string(repo.a.join("code.txt")).unwrap(),
        "unsaved work\n"
    );
    assert_eq!(fs::read_to_string(c.join("code.txt")).unwrap(), "from B\n");
}

#[test]
fn registry_deduplicates_subfolders_and_remove_keeps_repository() {
    let repo = Fixture::new();
    repo.add(&repo.a);
    let subfolder = repo.a.join("subfolder");
    fs::create_dir(&subfolder).unwrap();
    checked(repo.run(&["add", subfolder.to_str().unwrap()]));
    assert_eq!(checked(repo.run(&["list"])).lines().count(), 1);
    checked(repo.run(&["remove", "device-a"]));
    assert!(checked(repo.run(&["list"])).is_empty());
    assert!(repo.a.join("code.txt").is_file());
}

#[test]
fn push_checks_the_push_url_even_when_fetch_upstream_is_current() {
    let repo = Fixture::new();
    repo.add(&repo.a);
    git(
        repo.temp.path(),
        &["init", "--bare", "-b", "main", "publish.git"],
    );
    let publish = repo.temp.path().join("publish.git");
    git(
        &repo.a,
        &[
            "remote",
            "set-url",
            "--push",
            "origin",
            publish.to_str().unwrap(),
        ],
    );
    let first = checked(repo.run(&["push", "device-a"]));
    assert!(first.contains("\tpushed\t"), "{first}");
    assert_eq!(
        git(&publish, &["rev-parse", "main"]),
        git(&repo.a, &["rev-parse", "HEAD"])
    );
    assert!(checked(repo.run(&["push", "device-a"])).contains("\tup_to_date\t"));
}

#[test]
fn a_dirty_submodule_does_not_block_push_of_superproject_commits() {
    let repo = Fixture::new();
    repo.add(&repo.a);
    let root = repo.temp.path();
    git(root, &["init", "-b", "main", "library"]);
    let library = root.join("library");
    git(&library, &["config", "user.name", "sgit test"]);
    git(&library, &["config", "user.email", "test@example.invalid"]);
    fs::write(library.join("library.txt"), "committed\n").unwrap();
    git(&library, &["add", "library.txt"]);
    git(&library, &["commit", "-m", "library"]);
    git(
        &repo.a,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            library.to_str().unwrap(),
            "vendor",
        ],
    );
    git(&repo.a, &["commit", "-m", "add submodule"]);
    fs::write(repo.a.join("vendor/library.txt"), "local submodule work\n").unwrap();
    checked(repo.run(&["push", "device-a"]));
    assert_eq!(
        git(&repo.remote, &["rev-parse", "main"]),
        git(&repo.a, &["rev-parse", "HEAD"])
    );
    assert_eq!(
        fs::read_to_string(repo.a.join("vendor/library.txt")).unwrap(),
        "local submodule work\n"
    );
}

#[test]
fn fast_forward_does_not_overwrite_an_ignored_local_file() {
    let repo = Fixture::new();
    repo.add(&repo.a);
    fs::write(repo.a.join(".git/info/exclude"), "device.conf\n").unwrap();
    fs::write(repo.a.join("device.conf"), "local\n").unwrap();
    fs::write(repo.b.join("device.conf"), "remote\n").unwrap();
    git(&repo.b, &["add", "device.conf"]);
    git(&repo.b, &["commit", "-m", "add configuration"]);
    git(&repo.b, &["push"]);
    assert!(!repo.run(&["pull", "device-a"]).status.success());
    assert_eq!(
        fs::read_to_string(repo.a.join("device.conf")).unwrap(),
        "local\n"
    );
}
