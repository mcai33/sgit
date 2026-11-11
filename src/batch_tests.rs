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
    assert_eq!(
        git(&f.remote, &["log", "-1", "--format=%s", "main"]),
        "Sgit Auto: -include.txt, code.txt"
    );
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

#[test]
fn restore_discards_reviewed_tracked_edits_and_keeps_untracked_files() {
    let f = Fixture::new();
    let repo = f.store.list().unwrap().remove(0);
    fs::write(f.a.join("code.txt"), "staged\n").unwrap();
    git(&f.a, &["add", "code.txt"]);
    fs::write(f.a.join("code.txt"), "unstaged\n").unwrap();
    fs::write(f.a.join("[new].txt"), "staged new file\n").unwrap();
    git(&f.a, &["--literal-pathspecs", "add", "--", "[new].txt"]);
    fs::write(f.a.join("private.txt"), "keep\n").unwrap();

    let preview = f.store.worktree_changes(&repo).unwrap();
    assert_eq!(preview.untracked, ["private.txt"]);
    assert_eq!(
        preview
            .tracked
            .iter()
            .map(|file| (file.path.as_str(), file.status.as_str()))
            .collect::<Vec<_>>(),
        [("[new].txt", "A "), ("code.txt", "MM")]
    );
    f.store
        .apply_file_action(&repo, &preview.token, FileAction::Clean, None)
        .unwrap();
    assert_eq!(
        fs::read_to_string(f.a.join("code.txt")).unwrap(),
        "initial\n"
    );
    assert!(!f.a.join("[new].txt").exists());
    assert_eq!(
        fs::read_to_string(f.a.join("private.txt")).unwrap(),
        "keep\n"
    );
    assert_eq!(git(&f.a, &["status", "--porcelain"]), "?? private.txt");
}

#[test]
fn restore_refuses_to_overwrite_a_recreated_staged_deletion() {
    let f = Fixture::new();
    let repo = f.store.list().unwrap().remove(0);
    git(&f.a, &["rm", "code.txt"]);
    let preview = f.store.worktree_changes(&repo).unwrap();
    let token = preview.token;
    fs::write(f.a.join("code.txt"), "keep this\n").unwrap();
    assert!(
        f.store
            .apply_file_action(&repo, &token, FileAction::Clean, None)
            .is_err()
    );
    assert_eq!(
        fs::read_to_string(f.a.join("code.txt")).unwrap(),
        "keep this\n"
    );
    fs::write(f.a.join(".git/info/exclude"), "code.txt\n").unwrap();
    assert!(
        f.store
            .apply_file_action(&repo, &token, FileAction::Clean, None)
            .is_err()
    );
    assert_eq!(
        fs::read_to_string(f.a.join("code.txt")).unwrap(),
        "keep this\n"
    );
}

#[test]
fn file_actions_apply_only_to_the_selected_paths() {
    let f = Fixture::new();
    let repo = f.store.list().unwrap().remove(0);
    fs::write(f.a.join("code.txt"), "edited\n").unwrap();
    fs::write(f.a.join("[new].txt"), "stage this\n").unwrap();
    fs::write(f.a.join("other.txt"), "keep this\n").unwrap();

    let preview = f.store.worktree_changes(&repo).unwrap();
    let after_add = f
        .store
        .apply_file_action(&repo, &preview.token, FileAction::Add, Some(0))
        .unwrap();
    assert_eq!(after_add.untracked, ["other.txt"]);
    assert_eq!(git(&f.a, &["diff", "--cached", "--name-only"]), "[new].txt");

    f.store
        .apply_file_action(&repo, &after_add.token, FileAction::Delete, Some(0))
        .unwrap();
    assert_eq!(
        fs::read_to_string(f.a.join("code.txt")).unwrap(),
        "edited\n"
    );
    assert!(!f.a.join("other.txt").exists());
    assert!(f.a.join("[new].txt").is_file());
}

#[test]
fn deleting_all_untracked_checks_the_reviewed_list_and_preserves_ignored_files() {
    let f = Fixture::new();
    let repo = f.store.list().unwrap().remove(0);
    fs::write(f.a.join("code.txt"), "tracked change\n").unwrap();
    fs::write(f.a.join("draft.txt"), "draft\n").unwrap();
    fs::write(f.a.join(".git/info/exclude"), "private.txt\n").unwrap();
    fs::write(f.a.join("private.txt"), "ignored\n").unwrap();
    let preview = f.store.worktree_changes(&repo).unwrap();
    fs::create_dir(f.a.join("new")).unwrap();
    fs::write(f.a.join("new/later.txt"), "unreviewed\n").unwrap();
    assert!(
        f.store
            .apply_file_action(&repo, &preview.token, FileAction::Delete, None)
            .is_err()
    );
    assert!(f.a.join("draft.txt").is_file());
    assert!(f.a.join("new/later.txt").is_file());

    let preview = f.store.worktree_changes(&repo).unwrap();
    let after = f
        .store
        .apply_file_action(&repo, &preview.token, FileAction::Delete, None)
        .unwrap();
    assert_eq!(after.tracked[0].path, "code.txt");
    assert!(after.untracked.is_empty());
    assert_eq!(
        fs::read_to_string(f.a.join("code.txt")).unwrap(),
        "tracked change\n"
    );
    assert_eq!(
        fs::read_to_string(f.a.join("private.txt")).unwrap(),
        "ignored\n"
    );
    assert!(!f.a.join("new/later.txt").exists());
}

#[test]
fn ignoring_files_adds_literal_rules_and_preserves_the_files() {
    let f = Fixture::new();
    let repo = f.store.list().unwrap().remove(0);
    fs::write(f.a.join(".gitignore"), "# existing rule\n/old.txt").unwrap();
    fs::write(f.a.join("[draft].txt"), "keep\n").unwrap();
    fs::write(f.a.join("d.txt"), "unrelated\n").unwrap();
    let preview = f.store.worktree_changes(&repo).unwrap();
    let after = f
        .store
        .apply_file_action(&repo, &preview.token, FileAction::Ignore, Some(1))
        .unwrap();
    assert_eq!(after.untracked, [".gitignore", "d.txt"]);
    assert!(f.a.join("[draft].txt").is_file());
    assert!(
        fs::read_to_string(f.a.join(".gitignore"))
            .unwrap()
            .starts_with("# existing rule\n/old.txt\n")
    );

    let after = f
        .store
        .apply_file_action(&repo, &after.token, FileAction::Ignore, None)
        .unwrap();
    assert_eq!(after.untracked, [".gitignore"]);
    assert!(f.a.join("d.txt").is_file());
}

#[test]
fn local_commit_includes_tracked_edits_and_staged_files_only() {
    let f = Fixture::new();
    let repo = f.store.list().unwrap().remove(0);
    let remote_head = git(&f.remote, &["rev-parse", "main"]);
    fs::write(f.a.join("code.txt"), "tracked change\n").unwrap();
    fs::write(f.a.join("include.txt"), "stage me\n").unwrap();
    git(&f.a, &["add", "include.txt"]);
    fs::write(f.a.join("vanished.txt"), "removed before commit\n").unwrap();
    git(&f.a, &["add", "vanished.txt"]);
    fs::remove_file(f.a.join("vanished.txt")).unwrap();
    fs::write(f.a.join("untracked.txt"), "keep local\n").unwrap();

    let preview = f.store.worktree_changes(&repo).unwrap();
    let after = f
        .store
        .apply_file_action(&repo, &preview.token, FileAction::Commit, None)
        .unwrap();
    assert!(after.tracked.is_empty());
    assert_eq!(after.untracked, ["untracked.txt"]);
    assert_eq!(
        git(&f.a, &["log", "-1", "--format=%s"]),
        "Sgit Auto: code.txt, include.txt"
    );
    assert_eq!(
        git(
            &f.a,
            &["diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"]
        ),
        "code.txt\ninclude.txt"
    );
    assert_eq!(git(&f.remote, &["rev-parse", "main"]), remote_head);
}

#[test]
fn single_tracked_file_actions_preserve_other_edits_and_staged_files() {
    let f = Fixture::new();
    let repo = f.store.list().unwrap().remove(0);
    fs::write(f.a.join("code.txt"), "staged version\n").unwrap();
    git(&f.a, &["add", "code.txt"]);
    fs::write(f.a.join("code.txt"), "latest version\n").unwrap();
    fs::write(f.a.join("[new].txt"), "selected\n").unwrap();
    fs::write(f.a.join("other.txt"), "other staged file\n").unwrap();
    git(
        &f.a,
        &["--literal-pathspecs", "add", "--", "[new].txt", "other.txt"],
    );

    let preview = f.store.worktree_changes(&repo).unwrap();
    assert_eq!(preview.tracked[0].path, "[new].txt");
    let after = f
        .store
        .apply_file_action(&repo, &preview.token, FileAction::Commit, Some(0))
        .unwrap();
    assert_eq!(
        git(
            &f.a,
            &["diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"]
        ),
        "[new].txt"
    );
    assert_eq!(git(&f.a, &["show", ":code.txt"]), "staged version");
    assert_eq!(
        fs::read_to_string(f.a.join("code.txt")).unwrap(),
        "latest version\n"
    );
    assert_eq!(after.tracked[0].path, "code.txt");

    f.store
        .apply_file_action(&repo, &after.token, FileAction::Clean, Some(0))
        .unwrap();
    assert_eq!(
        fs::read_to_string(f.a.join("code.txt")).unwrap(),
        "initial\n"
    );
    assert_eq!(git(&f.a, &["diff", "--cached", "--name-only"]), "other.txt");
    assert_eq!(
        git(&f.a, &["log", "-1", "--format=%s"]),
        "Sgit Auto: [new].txt"
    );
}

#[test]
fn single_commit_does_not_capture_an_untracked_replacement() {
    let f = Fixture::new();
    let repo = f.store.list().unwrap().remove(0);
    git(&f.a, &["rm", "code.txt"]);
    fs::write(f.a.join("code.txt"), "private replacement\n").unwrap();
    let head = git(&f.a, &["rev-parse", "HEAD"]);
    let preview = f.store.worktree_changes(&repo).unwrap();

    assert!(
        f.store
            .apply_file_action(&repo, &preview.token, FileAction::Commit, Some(0))
            .is_err()
    );
    assert_eq!(git(&f.a, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        git(&f.a, &["diff", "--cached", "--name-status"]),
        "D\tcode.txt"
    );
    assert_eq!(
        fs::read_to_string(f.a.join("code.txt")).unwrap(),
        "private replacement\n"
    );
}
