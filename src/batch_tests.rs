use super::*;
use std::process::Command;
use tempfile::{TempDir, tempdir};

fn git(path: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(path)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

struct Fixture {
    _temp: TempDir,
    remote: PathBuf,
    a: PathBuf,
    b: PathBuf,
    store: Store,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempdir().unwrap();
        let root = temp.path();
        git(root, &["init", "--bare", "-b", "main", "remote.git"]);
        git(root, &["clone", "remote.git", "a"]);
        let a = root.join("a");
        git(&a, &["config", "core.autocrlf", "false"]);
        git(&a, &["config", "user.name", "sgit test"]);
        git(&a, &["config", "user.email", "test@example.invalid"]);
        fs::write(a.join("code.txt"), "initial\n").unwrap();
        git(&a, &["add", "code.txt"]);
        git(&a, &["commit", "-m", "initial"]);
        git(&a, &["push", "-u", "origin", "main"]);
        git(root, &["clone", "remote.git", "b"]);
        let b = root.join("b");
        git(&b, &["config", "user.name", "sgit test"]);
        git(&b, &["config", "user.email", "test@example.invalid"]);
        let store = Store {
            directory: root.join("config"),
        };
        store.add(&a).unwrap();
        Self {
            remote: root.join("remote.git"),
            _temp: temp,
            a,
            b,
            store,
        }
    }

    fn advance_remote(&self) {
        fs::write(self.b.join("code.txt"), "incoming\n").unwrap();
        git(&self.b, &["commit", "-am", "incoming"]);
        git(&self.b, &["push"]);
    }
}

#[test]
fn manual_sync_only_fetches_and_pulls_selected_repositories() {
    let f = Fixture::new();
    let selected = f.store.list().unwrap();
    let other = f._temp.path().join("other");
    git(f._temp.path(), &["clone", "remote.git", "other"]);
    f.store.add(&other).unwrap();
    f.advance_remote();
    assert!(f.store.check_sync(&selected).unwrap().ready());
    assert_eq!(
        fs::read_to_string(f.a.join("code.txt")).unwrap(),
        "incoming\n"
    );
    assert_eq!(
        fs::read_to_string(other.join("code.txt")).unwrap(),
        "initial\n"
    );
    let tracked_tip = git(&other, &["rev-parse", "origin/main"]);
    assert_eq!(tracked_tip, git(&other, &["rev-parse", "HEAD"]));
}

#[test]
fn manual_sync_pushes_only_after_approval_and_only_includes_selected_new_files() {
    let f = Fixture::new();
    let remote_head = git(&f.remote, &["rev-parse", "main"]);
    fs::write(f.a.join("code.txt"), "changed\n").unwrap();
    fs::write(f.a.join("-include.txt"), "include\n").unwrap();
    fs::write(f.a.join("private.txt"), "keep\n").unwrap();
    fs::write(f.a.join(".git/info/exclude"), "ignored.txt\n").unwrap();
    fs::write(f.a.join("ignored.txt"), "ignored\n").unwrap();
    let check = f.store.check_sync(&f.store.list().unwrap()).unwrap();
    assert!(!check.ready());
    let review = check.items[0].review.clone().unwrap();
    assert_eq!(review.untracked, ["-include.txt", "private.txt"]);
    assert_eq!(git(&f.remote, &["rev-parse", "main"]), remote_head);
    assert_eq!(git(&f.a, &["rev-parse", "HEAD"]), remote_head);
    let approved = SyncApproval {
        repository: check.items[0].repository.clone(),
        review,
        include: vec!["-include.txt".into()],
    };
    let result = f
        .store
        .confirm_sync(&f.store.list().unwrap(), &[approved])
        .unwrap();
    assert!(result.ready(), "{:?}", result.items);
    let files = git(&f.remote, &["ls-tree", "--name-only", "main"]);
    assert_eq!(files, "-include.txt\ncode.txt");
    assert!(f.a.join("private.txt").exists());
}

#[test]
fn manual_sync_fast_forwards_clean_repositories_and_blocks_dirty_or_unreachable_ones() {
    let f = Fixture::new();
    f.advance_remote();
    fs::write(f.a.join("code.txt"), "local work\n").unwrap();
    let check = f.store.check_sync(&f.store.list().unwrap()).unwrap();
    assert!(!check.ready() && check.items[0].problem.is_some());
    assert_eq!(
        fs::read_to_string(f.a.join("code.txt")).unwrap(),
        "local work\n"
    );
    git(&f.a, &["restore", "code.txt"]);
    assert!(
        f.store
            .check_sync(&f.store.list().unwrap())
            .unwrap()
            .ready()
    );
    assert_eq!(
        fs::read_to_string(f.a.join("code.txt")).unwrap(),
        "incoming\n"
    );
    fs::rename(&f.remote, f.remote.with_extension("offline")).unwrap();
    assert!(
        !f.store
            .check_sync(&f.store.list().unwrap())
            .unwrap()
            .ready()
    );
}

#[test]
fn a_new_untracked_file_invalidates_sync_approval() {
    let f = Fixture::new();
    fs::write(f.a.join("code.txt"), "local work\n").unwrap();
    let check = f.store.check_sync(&f.store.list().unwrap()).unwrap();
    let approved = SyncApproval {
        repository: check.items[0].repository.clone(),
        review: check.items[0].review.clone().unwrap(),
        include: vec![],
    };
    let head = git(&f.remote, &["rev-parse", "main"]);
    fs::write(f.a.join("late.txt"), "late\n").unwrap();
    let result = f
        .store
        .confirm_sync(&f.store.list().unwrap(), &[approved])
        .unwrap();
    assert!(!result.ready());
    assert_eq!(
        result.items[0].review.as_ref().unwrap().untracked,
        ["late.txt"]
    );
    assert_eq!(git(&f.remote, &["rev-parse", "main"]), head);
}

#[test]
fn replacing_a_reviewed_change_with_a_new_staged_path_requires_new_approval() {
    let f = Fixture::new();
    fs::write(f.a.join("code.txt"), "reviewed\n").unwrap();
    let check = f.store.check_sync(&f.store.list().unwrap()).unwrap();
    let approved = SyncApproval {
        repository: check.items[0].repository.clone(),
        review: check.items[0].review.clone().unwrap(),
        include: vec![],
    };
    let head = git(&f.remote, &["rev-parse", "main"]);
    git(&f.a, &["restore", "code.txt"]);
    fs::write(f.a.join("new-secret.txt"), "unreviewed\n").unwrap();
    git(&f.a, &["add", "new-secret.txt"]);
    assert!(
        !f.store
            .confirm_sync(&f.store.list().unwrap(), &[approved])
            .unwrap()
            .ready()
    );
    assert_eq!(git(&f.remote, &["rev-parse", "main"]), head);
}

#[test]
fn repository_details_show_remotes_without_embedded_credentials() {
    let f = Fixture::new();
    git(
        &f.a,
        &[
            "remote",
            "add",
            "preview",
            "https://user:secret@example.invalid/team/project.git?token=secret",
        ],
    );
    git(
        &f.a,
        &[
            "remote",
            "set-url",
            "--push",
            "preview",
            "git@example.invalid:team/project.git",
        ],
    );
    let registered = f.store.list().unwrap().remove(0);
    let remotes = f.store.remotes(&registered).unwrap();
    assert_eq!(remotes[0].name, "origin");
    assert!(remotes[0].push_urls.is_empty());
    assert_eq!(remotes[1].name, "preview");
    assert_eq!(
        remotes[1].fetch_urls,
        ["https://example.invalid/team/project.git"]
    );
    assert_eq!(
        remotes[1].push_urls,
        ["git@example.invalid:team/project.git"]
    );
    let stale = Repository {
        path: f.b.clone(),
        ..registered
    };
    assert!(f.store.remotes(&stale).is_err());
}

#[test]
fn clone_registers_a_new_checkout_and_refuses_existing_or_escaping_paths() {
    let f = Fixture::new();
    let parent = f._temp.path();
    let url = f.remote.to_str().unwrap();
    assert!(f.store.clone_repository(url, parent, "../escape").is_err());
    assert!(f.store.clone_repository(url, parent, "a").is_err());
    let clone = f
        .store
        .clone_repository(url, parent, "new checkout")
        .unwrap();
    assert_eq!(clone.id, "new checkout");
    let checked_out = fs::read_to_string(clone.path.join("code.txt")).unwrap();
    assert_eq!(checked_out.lines().collect::<Vec<_>>(), ["initial"]);
    assert_eq!(f.store.list().unwrap().len(), 2);
    assert!(
        f.store
            .clone_repository(
                parent.join("missing.git").to_str().unwrap(),
                parent,
                "failed"
            )
            .is_err()
    );
    assert!(!parent.join("failed").exists());
}

#[test]
fn trash_failure_restores_registration_and_success_moves_the_checkout() {
    let f = Fixture::new();
    let repo = f.store.list().unwrap().remove(0);
    assert!(
        f.store
            .trash_repository(&repo, |_| Err("trash unavailable".into()))
            .is_err()
    );
    assert_eq!(f.store.list().unwrap()[0].path, repo.path);
    assert!(repo.path.join("code.txt").is_file());

    let trashed = f._temp.path().join("simulated-trash");
    f.store
        .trash_repository(&repo, |path| {
            fs::rename(path, &trashed).map_err(|error| error.to_string())
        })
        .unwrap();
    assert!(f.store.list().unwrap().is_empty());
    assert!(!repo.path.exists());
    assert!(trashed.join("code.txt").is_file());
}

#[test]
fn trash_refuses_to_move_a_repository_with_other_linked_worktrees() {
    let f = Fixture::new();
    let repo = f.store.list().unwrap().remove(0);
    let linked = f._temp.path().join("linked");
    git(
        &f.a,
        &["worktree", "add", "-b", "other", linked.to_str().unwrap()],
    );
    let error = f
        .store
        .trash_repository(&repo, |_| panic!("must not move this checkout"))
        .unwrap_err();
    assert!(error.contains("worktree"));
    assert!(linked.join("code.txt").is_file());
    assert_eq!(f.store.list().unwrap().len(), 1);
}
