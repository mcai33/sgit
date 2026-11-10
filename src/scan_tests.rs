use super::*;
use std::process::Command;
use tempfile::tempdir;

fn git(directory: &Path, args: &[&str]) {
    let output = Command::new("git")
        .current_dir(directory)
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
}

fn repo(path: &Path) -> PathBuf {
    fs::create_dir_all(path).unwrap();
    git(path, &["init", "-b", "main"]);
    path.canonicalize().unwrap()
}

#[test]
fn scans_nested_worktrees_and_marks_existing_entries_without_registering() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("projects");
    let store = Store {
        directory: temp.path().join("config"),
    };
    let first = repo(&root.join("alpha"));
    let nested = repo(&first.join("nested"));
    let second = repo(&root.join("group/beta"));
    let linked = root.join("worktree");
    git(
        &first,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "Initial",
        ],
    );
    git(
        &first,
        &["worktree", "add", "-b", "linked", linked.to_str().unwrap()],
    );
    let linked = linked.canonicalize().unwrap();
    fs::create_dir(root.join("plain")).unwrap();
    git(&root, &["init", "--bare", "bare.git"]);

    store.add(&first).unwrap();
    let scan = store.scan(&[root, first.clone()]).unwrap();
    let candidates: Vec<_> = scan
        .candidates
        .iter()
        .map(|c| (c.path.clone(), c.registered))
        .collect();
    assert_eq!(
        candidates,
        vec![
            (first, true),
            (nested, false),
            (second.clone(), false),
            (linked, false)
        ]
    );
    assert_eq!(scan.skipped, 0);
    assert_eq!(store.list().unwrap().len(), 1);
    store.add_scanned(std::slice::from_ref(&second)).unwrap();
    let list = store.list().unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[1].path, second);
}

#[test]
fn selected_paths_are_revalidated_before_any_are_registered() {
    let temp = tempdir().unwrap();
    let store = Store {
        directory: temp.path().join("config"),
    };
    let parent = repo(&temp.path().join("parent"));
    let child = repo(&parent.join("child"));
    let other = repo(&temp.path().join("other"));
    fs::rename(child.join(".git"), child.join("old-metadata")).unwrap();

    assert!(store.add_scanned(&[other, child]).is_err());
    assert!(store.list().unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn directory_links_are_not_followed() {
    let temp = tempdir().unwrap();
    let root = temp.path().join("projects");
    let found = repo(&root.join("normal"));
    let outside = repo(&temp.path().join("outside"));
    let store = Store {
        directory: temp.path().join("config"),
    };
    std::os::unix::fs::symlink(&root, root.join("loop")).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("outside-link")).unwrap();
    let scan = store.scan(&[root]).unwrap();
    assert_eq!(scan.candidates.len(), 1);
    assert_eq!(scan.candidates[0].path, found);
}
