use crate::{
    ChangedFile, FileAction, Operation, Outcome, RemoteInfo, Repository, RepositoryStatus, Result,
    SyncApproval, SyncResult, SyncReview, WorktreeChanges,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    hash::{DefaultHasher, Hash, Hasher},
    io::Write,
    path::{Component, Path, PathBuf},
    process::{Command, Output, Stdio},
};

struct Upstream {
    remote: String,
    source: String,
    tracking: String,
}

struct Prepared<'a> {
    path: &'a Path,
    branch: String,
    head: String,
    upstream: Upstream,
    remote_tip: String,
    changes: Changes,
    ahead: u64,
    behind: u64,
}

#[derive(Default)]
struct Changes {
    tracked: usize,
    tracked_status: u64,
    tracked_entries: Vec<Vec<u8>>,
    ignored: Vec<Vec<u8>>,
    untracked: Vec<Vec<u8>>,
    conflicts: bool,
}

fn command(path: &Path, args: &[&str]) -> Result<Output> {
    Command::new("git")
        .current_dir(path)
        .args([
            "--no-optional-locks",
            "-c",
            "color.ui=false",
            "-c",
            "core.quotePath=false",
        ])
        .args(args)
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_NAMESPACE")
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("Cannot run Git in {}: {error}", path.display()))
}

fn git(path: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = command(path, args)?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        let detail = String::from_utf8_lossy(&output.stderr);
        let detail = if detail.trim().is_empty() {
            String::from_utf8_lossy(&output.stdout).trim().to_owned()
        } else {
            detail.trim().to_owned()
        };
        Err(if detail.is_empty() {
            format!("Git exited with {}", output.status)
        } else {
            detail
        })
    }
}

fn line(path: &Path, args: &[&str]) -> Result<String> {
    let value = String::from_utf8(git(path, args)?)
        .map_err(|_| "Git returned a non-UTF-8 name".to_owned())?;
    Ok(value.strip_suffix('\n').unwrap_or(&value).to_owned())
}

pub(crate) fn root(path: &Path) -> Result<PathBuf> {
    let path = line(path, &["rev-parse", "--show-toplevel"])?;
    PathBuf::from(path)
        .canonicalize()
        .map_err(|error| format!("Cannot resolve repository directory: {error}"))
}

fn registered_root(repo: &Repository) -> Result<&Path> {
    let path = root(&repo.path)?;
    if path != repo.path {
        return Err("The registered folder is no longer the root of its Git worktree".into());
    }
    Ok(&repo.path)
}

pub(crate) fn clone(address: &str, destination: &Path) -> Result<()> {
    git(
        destination,
        &["clone", "--no-recurse-submodules", "--", address, "."],
    )?;
    Ok(())
}

pub(crate) fn trashable_root(repo: &Repository) -> Result<&Path> {
    let path = registered_root(repo)?;
    let metadata = fs::symlink_metadata(path.join(".git")).map_err(|error| error.to_string())?;
    if !metadata.is_dir() {
        return Err(
            "This repository uses linked Git metadata. Manage it with Git before moving its files."
                .into(),
        );
    }
    let worktrees = git(path, &["worktree", "list", "--porcelain", "-z"])?;
    if worktrees
        .split(|byte| *byte == 0)
        .filter(|field| field.starts_with(b"worktree "))
        .count()
        != 1
    {
        return Err("This repository has other Git worktrees. Remove them in Git before moving this folder.".into());
    }
    Ok(path)
}

pub(crate) fn remotes(repo: &Repository) -> Result<Vec<RemoteInfo>> {
    let path = registered_root(repo)?;
    line(path, &["remote"])?
        .lines()
        .map(|name| {
            let fetch = line(path, &["remote", "get-url", "--all", "--", name])?;
            let push = line(path, &["remote", "get-url", "--push", "--all", "--", name])?;
            let fetch_urls: Vec<_> = fetch.lines().map(display_url).collect();
            let push_urls: Vec<_> = push.lines().map(display_url).collect();
            Ok(RemoteInfo {
                name: name.into(),
                push_urls: if push_urls == fetch_urls {
                    Vec::new()
                } else {
                    push_urls
                },
                fetch_urls,
            })
        })
        .collect()
}

fn display_url(url: &str) -> String {
    let Some((scheme, tail)) = url.split_once("://") else {
        return url.into();
    };
    let host_end = tail.find(['/', '?', '#', '\\']).unwrap_or(tail.len());
    let (authority, path) = tail.split_at(host_end);
    let authority = match authority.rsplit_once('@') {
        Some((_, host))
            if scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https") =>
        {
            host
        }
        Some((userinfo, host)) if userinfo.contains(':') => host,
        _ => authority,
    };
    let path = path.split(['?', '#']).next().unwrap_or_default();
    format!("{scheme}://{authority}{path}")
}

fn branch(path: &Path) -> Result<String> {
    line(path, &["symbolic-ref", "--quiet", "HEAD"])
        .map_err(|_| "Check out a local branch before syncing (detached HEAD).".into())
}

fn upstream(path: &Path, branch: &str) -> Result<Upstream> {
    let fields = line(
        path,
        &[
            "for-each-ref",
            "--format=%(upstream:remotename)%00%(upstream:remoteref)%00%(upstream)",
            branch,
        ],
    )?;
    let fields: Vec<&str> = fields.split('\0').collect();
    if let [remote, source, tracking] = fields.as_slice()
        && !remote.is_empty()
        && *remote != "."
        && source.starts_with("refs/heads/")
        && tracking.starts_with("refs/remotes/")
    {
        return Ok(Upstream {
            remote: (*remote).into(),
            source: (*source).into(),
            tracking: (*tracking).into(),
        });
    }
    Err("Set an upstream remote branch for the current branch before syncing.".into())
}

fn changes(path: &Path, list_files: bool) -> Result<Changes> {
    read_changes(path, list_files, false)
}

fn read_changes(path: &Path, list_files: bool, include_ignored: bool) -> Result<Changes> {
    let mut args = vec![
        "status",
        "--porcelain=v1",
        "-z",
        "--no-renames",
        if list_files {
            "--untracked-files=all"
        } else {
            "--untracked-files=normal"
        },
        "--ignore-submodules=dirty",
    ];
    if include_ignored {
        args.push("--ignored=matching");
    }
    let output = git(path, &args)?;
    let mut changes = Changes::default();
    let mut tracked_status = DefaultHasher::new();
    for entry in output.split(|byte| *byte == 0) {
        match entry.get(..2) {
            Some(b"??") => changes.untracked.push(entry[3..].to_vec()),
            Some(b"!!") => changes.ignored.push(entry[3..].to_vec()),
            Some(code) => {
                changes.tracked += 1;
                if list_files {
                    entry.hash(&mut tracked_status);
                    if include_ignored {
                        changes.tracked_entries.push(entry.to_vec());
                    }
                }
                changes.conflicts |=
                    matches!(code, b"DD" | b"AU" | b"UD" | b"UA" | b"DU" | b"AA" | b"UU");
            }
            None => {}
        }
    }
    changes.tracked_status = tracked_status.finish();
    Ok(changes)
}

fn check_in_progress(path: &Path, changes: &Changes) -> Result<()> {
    let git_dir = PathBuf::from(line(path, &["rev-parse", "--absolute-git-dir"])?);
    let markers = [
        "MERGE_HEAD",
        "rebase-apply",
        "rebase-merge",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "sequencer",
        "BISECT_START",
    ];
    for marker in markers {
        if git_dir
            .join(marker)
            .try_exists()
            .map_err(|error| error.to_string())?
        {
            return Err("Finish the Git operation already in progress and try again.".into());
        }
    }
    if changes.conflicts {
        return Err("Resolve unmerged files and try again.".into());
    }
    Ok(())
}

const PROTECTED_PATH: &str =
    "This change overlaps an untracked or ignored path. Move that file aside before continuing.";

fn normalized_path(path: &[u8]) -> String {
    String::from_utf8_lossy(path)
        .trim_end_matches('/')
        .to_lowercase()
}

fn protected_paths(changes: &Changes) -> BTreeSet<String> {
    changes
        .ignored
        .iter()
        .chain(&changes.untracked)
        .map(|path| normalized_path(path))
        .collect()
}

fn has_related_path(name: &str, paths: &BTreeSet<String>) -> bool {
    let descendants = format!("{name}/");
    name.match_indices('/')
        .any(|(index, _)| paths.contains(&name[..index]))
        || paths
            .range(descendants.clone()..)
            .next()
            .is_some_and(|path| path.starts_with(&descendants))
}

fn is_protected(name: &str, protected: &BTreeSet<String>) -> bool {
    protected.contains(name) || has_related_path(name, protected)
}

fn single_file_block(
    name: &str,
    tracked: &BTreeSet<String>,
    protected: &BTreeSet<String>,
) -> Option<&'static str> {
    if is_protected(name, protected) {
        Some(PROTECTED_PATH)
    } else if has_related_path(name, tracked) {
        Some("This path is part of a file and directory replacement. Use the bulk action.")
    } else {
        None
    }
}

fn file_action_token(path: &Path, head: Option<&str>, changes: &Changes) -> String {
    let mut hasher = DefaultHasher::new();
    branch(path).ok().hash(&mut hasher);
    head.hash(&mut hasher);
    changes.tracked_status.hash(&mut hasher);
    changes.untracked.hash(&mut hasher);
    changes.ignored.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn is_untracked_file(name: &[u8]) -> bool {
    !name.ends_with(b"/")
}

pub(crate) fn worktree_changes(repo: &Repository) -> Result<WorktreeChanges> {
    let path = registered_root(repo)?;
    let changes = read_changes(path, true, true)?;
    let head = line(path, &["rev-parse", "--verify", "HEAD^{commit}"]).ok();
    let tracked: BTreeSet<_> = changes
        .tracked_entries
        .iter()
        .map(|entry| normalized_path(&entry[3..]))
        .collect();
    let protected = protected_paths(&changes);
    let commit_blocked = if tracked.is_empty() {
        None
    } else {
        check_in_progress(path, &changes).err()
    };
    let clean_blocked = if tracked.is_empty() {
        None
    } else if let Some(error) = &commit_blocked {
        Some(error.clone())
    } else if head.is_none() {
        Some("This repository has no commit to restore from.".into())
    } else if tracked.iter().any(|name| is_protected(name, &protected)) {
        Some(PROTECTED_PATH.into())
    } else {
        None
    };
    Ok(WorktreeChanges {
        tracked: changes
            .tracked_entries
            .iter()
            .map(|entry| ChangedFile {
                path: String::from_utf8_lossy(&entry[3..]).into_owned(),
                status: String::from_utf8_lossy(&entry[..2]).into_owned(),
                action_blocked: single_file_block(
                    &normalized_path(&entry[3..]),
                    &tracked,
                    &protected,
                ),
            })
            .collect(),
        untracked: changes
            .untracked
            .iter()
            .map(|name| String::from_utf8_lossy(name).into_owned())
            .collect(),
        token: file_action_token(path, head.as_deref(), &changes),
        has_head: head.is_some(),
        clean_blocked,
        commit_blocked,
    })
}

fn run_with_file_paths(path: &Path, options: &[&str], entries: &[&[u8]]) -> Result<()> {
    if entries.is_empty() {
        return Ok(());
    }
    let mut paths = tempfile::NamedTempFile::new().map_err(|error| error.to_string())?;
    for entry in entries {
        paths
            .write_all(entry)
            .and_then(|_| paths.write_all(b"\0"))
            .map_err(|error| error.to_string())?;
    }
    let file = paths
        .path()
        .to_str()
        .ok_or("Cannot encode the temporary file path.")?;
    let input = format!("--pathspec-from-file={file}");
    let mut args = vec!["--literal-pathspecs"];
    args.extend_from_slice(options);
    args.extend([&input, "--pathspec-file-nul"]);
    git(path, &args)?;
    Ok(())
}

fn restore_files(path: &Path, head: &str, entries: &[&[u8]]) -> Result<()> {
    run_with_file_paths(
        path,
        &[
            "restore",
            "--staged",
            "--worktree",
            "--no-recurse-submodules",
            &format!("--source={head}"),
        ],
        entries,
    )
}

fn auto_commit_message<'a>(paths: impl Iterator<Item = &'a [u8]>) -> String {
    let names: Vec<_> = paths
        .map(|path| {
            String::from_utf8_lossy(path)
                .replace('\\', "\\\\")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
                .replace('\t', "\\t")
        })
        .collect();
    format!("Sgit Auto: {}", names.join(", "))
}

fn auto_commit_all(path: &Path) -> Result<()> {
    // Ask Git for the index it would commit, including additions staged just before syncing.
    let preview = git(
        path,
        &[
            "-c",
            "status.renames=false",
            "commit",
            "--all",
            "--dry-run",
            "--porcelain",
            "-z",
            "--untracked-files=no",
        ],
    )?;
    let paths = preview
        .split(|byte| *byte == 0)
        .filter_map(|entry| match entry.first() {
            Some(b'A' | b'D' | b'M' | b'T') => entry.get(3..),
            _ => None,
        });
    let mut message = tempfile::NamedTempFile::new().map_err(|error| error.to_string())?;
    writeln!(message, "{}", auto_commit_message(paths)).map_err(|error| error.to_string())?;
    let message_path = message
        .path()
        .to_str()
        .ok_or("Cannot encode the commit message file path.")?;
    git(
        path,
        &[
            "commit",
            "--all",
            "--cleanup=verbatim",
            "--file",
            message_path,
        ],
    )?;
    Ok(())
}

fn untracked_file_path(root: &Path, name: &[u8]) -> Result<PathBuf> {
    #[cfg(unix)]
    let relative = {
        use std::os::unix::ffi::OsStrExt;
        Path::new(std::ffi::OsStr::from_bytes(name))
    };
    #[cfg(not(unix))]
    let relative = Path::new(std::str::from_utf8(name).map_err(|error| error.to_string())?);

    let mut path = root.to_path_buf();
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(part) = component else {
            return Err("Git reported an invalid file path.".into());
        };
        if part.eq_ignore_ascii_case(".git") {
            return Err("Cannot operate on Git metadata.".into());
        }
        path.push(part);
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("{}: {error}", relative.display()))?;
        let kind = metadata.file_type();
        let valid = if components.peek().is_some() {
            kind.is_dir()
                && path
                    .join(".git")
                    .symlink_metadata()
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
        } else {
            kind.is_file() || kind.is_symlink()
        };
        if !valid {
            return Err(format!(
                "{} is no longer an untracked file.",
                relative.display()
            ));
        }
    }
    Ok(path)
}

fn select_paths<'a, 'b>(paths: &'a [&'b [u8]], file: Option<usize>) -> Result<&'a [&'b [u8]]> {
    match file {
        Some(index) => paths
            .get(index)
            .map(std::slice::from_ref)
            .ok_or_else(|| "The selected file is no longer in the list.".into()),
        None => Ok(paths),
    }
}

fn can_ignore(name: &[u8]) -> bool {
    let basename = name.rsplit(|byte| *byte == b'/').next().unwrap_or(name);
    !basename.eq_ignore_ascii_case(b".gitignore")
        && !name.contains(&b'\n')
        && !name.contains(&b'\r')
}

fn ignore_files(root: &Path, files: &[(&[u8], PathBuf)]) -> Result<()> {
    let mut rules_by_file = BTreeMap::<PathBuf, Vec<u8>>::new();
    for (name, file) in files {
        // Use the closest existing ignore file so its new rule takes precedence.
        let mut directory = file.parent().unwrap_or(root);
        let mut prefix_len = name.len();
        let (ignore_path, relative_name) = loop {
            if directory == root {
                break (root.join(".gitignore"), *name);
            }
            let Some(slash) = name[..prefix_len].iter().rposition(|byte| *byte == b'/') else {
                return Err("Git reported an invalid file path.".into());
            };
            let candidate = directory.join(".gitignore");
            if candidate
                .symlink_metadata()
                .is_ok_and(|metadata| metadata.is_file())
            {
                break (candidate, &name[slash + 1..]);
            }
            prefix_len = slash;
            directory = directory.parent().unwrap_or(root);
        };
        let rules = rules_by_file.entry(ignore_path).or_default();
        rules.push(b'/');
        for &byte in relative_name {
            if matches!(byte, b'\\' | b'[' | b']' | b'*' | b'?' | b' ') {
                rules.push(b'\\');
            }
            rules.push(byte);
        }
        rules.push(b'\n');
    }

    for (path, rules) in rules_by_file {
        let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap_or(root))
            .map_err(|error| error.to_string())?;
        match path.symlink_metadata() {
            Ok(metadata) if metadata.is_file() => {
                let content = fs::read(&path).map_err(|error| error.to_string())?;
                file.write_all(&content)
                    .map_err(|error| error.to_string())?;
                if !content.is_empty() && !content.ends_with(b"\n") {
                    file.write_all(b"\n").map_err(|error| error.to_string())?;
                }
                file.as_file()
                    .set_permissions(metadata.permissions())
                    .map_err(|error| error.to_string())?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            _ => {
                return Err(format!(
                    "Cannot edit {}: expected a regular file.",
                    path.display()
                ));
            }
        }
        file.write_all(&rules).map_err(|error| error.to_string())?;
        file.persist(&path)
            .map_err(|error| format!("Cannot update {}: {error}", path.display()))?;
    }
    Ok(())
}

pub(crate) fn apply_file_action(
    repo: &Repository,
    token: &str,
    action: FileAction,
    file: Option<usize>,
) -> Result<WorktreeChanges> {
    let path = registered_root(repo)?;
    let changes = read_changes(path, true, true)?;
    let head = line(path, &["rev-parse", "--verify", "HEAD^{commit}"]).ok();
    if file_action_token(path, head.as_deref(), &changes) != token {
        return Err("The repository changed. Review the file list again.".into());
    }

    match action {
        FileAction::Clean | FileAction::Commit => {
            let tracked: Vec<_> = changes
                .tracked_entries
                .iter()
                .map(|entry| &entry[3..])
                .collect();
            let selected = select_paths(&tracked, file)?;
            if !selected.is_empty() {
                check_in_progress(path, &changes)?;
                if file.is_some() {
                    let tracked = tracked.iter().map(|name| normalized_path(name)).collect();
                    if let Some(error) = single_file_block(
                        &normalized_path(selected[0]),
                        &tracked,
                        &protected_paths(&changes),
                    ) {
                        return Err(error.into());
                    }
                }
                if matches!(action, FileAction::Commit) {
                    if file.is_some() {
                        let message = auto_commit_message(selected.iter().copied());
                        run_with_file_paths(
                            path,
                            &[
                                "commit",
                                "--only",
                                "--cleanup=verbatim",
                                "--message",
                                &message,
                            ],
                            selected,
                        )?;
                    } else {
                        auto_commit_all(path)?;
                    }
                } else {
                    let head = head.ok_or("This repository has no commit to restore from.")?;
                    if file.is_none() {
                        let protected = protected_paths(&changes);
                        if selected
                            .iter()
                            .any(|name| is_protected(&normalized_path(name), &protected))
                        {
                            return Err(PROTECTED_PATH.into());
                        }
                    }
                    restore_files(path, &head, selected)?;
                }
            }
        }
        FileAction::Add | FileAction::Delete | FileAction::Ignore => {
            let untracked: Vec<_> = changes.untracked.iter().map(Vec::as_slice).collect();
            let selected: Vec<_> = select_paths(&untracked, file)?
                .iter()
                .copied()
                .filter(|name| is_untracked_file(name))
                .filter(|name| !matches!(action, FileAction::Ignore) || can_ignore(name))
                .collect();
            if file.is_some() && selected.is_empty() {
                return Err("This action is not available for the selected path.".into());
            }
            let files: Vec<_> = selected
                .iter()
                .map(|&name| Ok((name, untracked_file_path(path, name)?)))
                .collect::<Result<_>>()?;
            match action {
                FileAction::Add => {
                    run_with_file_paths(path, &["add", "--ignore-removal"], &selected)?
                }
                FileAction::Delete => {
                    for (name, _) in files {
                        let file = untracked_file_path(path, name)?;
                        delete_untracked_file(&file)?;
                    }
                }
                FileAction::Ignore => ignore_files(path, &files)?,
                _ => unreachable!(),
            }
        }
    }
    worktree_changes(repo)
}

fn delete_untracked_file(path: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileTypeExt;
        let metadata = path.symlink_metadata().map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink_dir() {
            return fs::remove_dir(path)
                .map_err(|error| format!("Cannot delete {}: {error}", path.display()));
        }
    }
    fs::remove_file(path).map_err(|error| format!("Cannot delete {}: {error}", path.display()))
}

fn distance(path: &Path, upstream: &str) -> Result<(u64, u64)> {
    let range = format!("HEAD...{upstream}");
    let counts = line(path, &["rev-list", "--left-right", "--count", &range])?;
    let (ahead, behind) = counts
        .split_once('\t')
        .ok_or("Invalid ahead/behind count from Git")?;
    Ok((
        ahead.parse().map_err(|_| "Invalid ahead count from Git")?,
        behind
            .parse()
            .map_err(|_| "Invalid behind count from Git")?,
    ))
}

pub(crate) fn inspect(repository: Repository) -> RepositoryStatus {
    let mut status = RepositoryStatus {
        repository,
        branch: None,
        upstream: None,
        ahead: None,
        behind: None,
        changes: 0,
        untracked: 0,
        problem: None,
    };
    let mut read = || -> Result<()> {
        let path = registered_root(&status.repository)?;
        let changes = changes(path, false)?;
        status.changes = changes.tracked;
        status.untracked = changes.untracked.len();
        let branch = branch(path)?;
        status.branch = Some(branch.trim_start_matches("refs/heads/").into());
        let upstream = upstream(path, &branch)?;
        status.upstream = Some(upstream.tracking.trim_start_matches("refs/remotes/").into());
        if let Ok((ahead, behind)) = distance(path, &upstream.tracking) {
            status.ahead = Some(ahead);
            status.behind = Some(behind);
        }
        check_in_progress(path, &changes)?;
        Ok(())
    };
    status.problem = read().err();
    status
}

pub(crate) fn sync(repo: &Repository, operation: Operation) -> SyncResult {
    let mut result = SyncResult {
        id: repo.id.clone(),
        outcome: Outcome::Blocked,
        commit: None,
        detail: String::new(),
    };
    match execute(repo, operation, &mut result.commit) {
        Ok((outcome, detail)) => {
            result.outcome = outcome;
            result.detail = detail.into();
        }
        Err(detail) => {
            if result.commit.is_some() {
                result.outcome = Outcome::CommittedPendingPush;
            }
            result.detail = detail;
        }
    }
    result
}

fn prepare(repo: &Repository, list_files: bool) -> Result<Prepared<'_>> {
    let path = registered_root(repo)?;
    let current_branch = branch(path)?;
    let upstream = upstream(path, &current_branch)?;
    check_in_progress(path, &changes(path, false)?)?;

    let refspec = format!("+{}:{}", upstream.source, upstream.tracking);
    git(
        path,
        &[
            "fetch",
            "--no-tags",
            "--no-recurse-submodules",
            "--no-write-fetch-head",
            "--refmap=",
            "--",
            &upstream.remote,
            &refspec,
        ],
    )?;
    let remote_tip = line(path, &["rev-parse", "--verify", &upstream.tracking])?;
    let changes = changes(path, list_files)?;
    check_in_progress(path, &changes)?;
    if branch(path)? != current_branch {
        return Err("The checked-out branch changed during the fetch. Try again.".into());
    }
    let head = line(path, &["rev-parse", "--verify", &current_branch])?;
    let (ahead, behind) = distance(path, &remote_tip)?;
    Ok(Prepared {
        path,
        branch: current_branch,
        head,
        upstream,
        remote_tip,
        changes,
        ahead,
        behind,
    })
}

const DIVERGED: &str =
    "Local and remote branches have diverged. Reconcile them in Git, then retry.";

fn ensure_same_head(prepared: &Prepared<'_>) -> Result<()> {
    if branch(prepared.path)? != prepared.branch
        || line(prepared.path, &["rev-parse", "--verify", "HEAD"])? != prepared.head
    {
        return Err("The checked-out branch or commit changed. Check again before syncing.".into());
    }
    Ok(())
}

fn fast_forward(prepared: &Prepared<'_>) -> Result<()> {
    ensure_same_head(prepared)?;
    git(
        prepared.path,
        &[
            "-c",
            "submodule.recurse=false",
            "merge",
            "--ff-only",
            "--no-autostash",
            "--no-overwrite-ignore",
            &prepared.remote_tip,
        ],
    )?;
    Ok(())
}

fn push(
    prepared: &Prepared<'_>,
    commit_changes: bool,
    commit: &mut Option<String>,
) -> Result<(Outcome, &'static str)> {
    ensure_same_head(prepared)?;
    let path = prepared.path;
    if commit_changes {
        auto_commit_all(path)?;
        *commit = Some(line(path, &["rev-parse", "--verify", &prepared.branch])?);
    }
    let tip = commit.as_deref().unwrap_or(&prepared.head);
    let refspec = format!("{tip}:{}", prepared.upstream.source);
    let disable_mirror = format!("remote.{}.mirror=false", prepared.upstream.remote);
    let pushed = line(
        path,
        &[
            "-c",
            &disable_mirror,
            "push",
            "--porcelain",
            "--no-follow-tags",
            "--recurse-submodules=no",
            "--",
            &prepared.upstream.remote,
            &refspec,
        ],
    )?;
    let mut statuses = pushed.lines().filter_map(|line| line.split_once('\t'));
    if statuses.next().is_some_and(|(flag, _)| flag == "=") && statuses.all(|(flag, _)| flag == "=")
    {
        Ok((Outcome::UpToDate, "No outgoing commits."))
    } else {
        Ok((Outcome::Pushed, "Pushed to the configured upstream branch."))
    }
}

fn execute(
    repo: &Repository,
    operation: Operation,
    commit: &mut Option<String>,
) -> Result<(Outcome, &'static str)> {
    let prepared = prepare(repo, false)?;
    match operation {
        Operation::Pull => {
            if prepared.behind > 0 && prepared.ahead > 0 {
                return Err(DIVERGED.into());
            }
            if prepared.behind == 0 {
                return Ok((Outcome::UpToDate, "No incoming commits."));
            }
            if prepared.changes.tracked > 0 {
                return Err("Commit or stash tracked changes before pulling.".into());
            }
            fast_forward(&prepared)?;
            Ok((Outcome::Pulled, "Fast-forwarded to the upstream commit."))
        }
        Operation::Push => {
            if prepared.behind > 0 {
                return Err(if prepared.ahead > 0 {
                    DIVERGED
                } else {
                    "The upstream branch has newer commits. Pull before pushing."
                }
                .into());
            }
            push(&prepared, prepared.changes.tracked > 0, commit)
        }
    }
}

pub(crate) fn batch_step(
    repo: &Repository,
    approval: Option<&SyncApproval>,
    verify_only: bool,
) -> Result<Option<SyncReview>> {
    let mut prepared = prepare(repo, true)?;
    let tracked = prepared.changes.tracked;
    let mut untracked = std::mem::take(&mut prepared.changes.untracked)
        .into_iter()
        .map(String::from_utf8)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| {
            "An untracked path is not valid UTF-8. Handle it in Git and try again.".to_owned()
        })?;
    untracked.sort_unstable();
    let needs_push = prepared.ahead > 0 || tracked > 0;
    if prepared.behind > 0 {
        if needs_push || !untracked.is_empty() {
            return Err("Both the local worktree and upstream have changes. Reconcile them in Git, then retry.".into());
        }
        fast_forward(&prepared)?;
        return Ok(None);
    }

    if !needs_push && untracked.is_empty() {
        return Ok(None);
    }

    let mut fingerprint = DefaultHasher::new();
    prepared.changes.tracked_status.hash(&mut fingerprint);
    prepared.upstream.source.hash(&mut fingerprint);
    prepared.upstream.remote.hash(&mut fingerprint);
    for args in [
        vec![
            "remote",
            "get-url",
            "--all",
            "--",
            &prepared.upstream.remote,
        ],
        vec![
            "remote",
            "get-url",
            "--push",
            "--all",
            "--",
            &prepared.upstream.remote,
        ],
    ] {
        git(prepared.path, &args)?.hash(&mut fingerprint);
    }
    let review = SyncReview {
        fingerprint: format!("{:016x}", fingerprint.finish()),
        upstream: prepared
            .upstream
            .tracking
            .trim_start_matches("refs/remotes/")
            .into(),
        branch: prepared.branch.clone(),
        head: prepared.head.clone(),
        untracked,
        needs_push,
        tracked,
        ahead: prepared.ahead,
    };
    let Some(approval) = approval else {
        return Ok(Some(review));
    };
    if verify_only {
        let only_approved_local_files_remain = !review.needs_push
            && review.branch == approval.review.branch
            && review.untracked.iter().all(|file| {
                approval.review.untracked.contains(file) && !approval.include.contains(file)
            });
        return Ok((!only_approved_local_files_remain).then_some(review));
    }
    if approval.review != review {
        return Ok(Some(review));
    }
    if approval
        .include
        .iter()
        .any(|file| !review.untracked.contains(file) || file.ends_with('/'))
    {
        return Err("A selected path is no longer an individual untracked file. Handle it in Git or keep it local.".into());
    }
    if !approval.include.is_empty() {
        ensure_same_head(&prepared)?;
        let mut args = vec!["--literal-pathspecs", "add", "--"];
        args.extend(approval.include.iter().map(String::as_str));
        git(prepared.path, &args)?;
    }
    if needs_push || !approval.include.is_empty() {
        let mut commit = None;
        if let Err(error) = push(
            &prepared,
            tracked > 0 || !approval.include.is_empty(),
            &mut commit,
        ) {
            return Err(match commit {
                Some(hash) => format!("Created local commit {hash}, but the push failed.\n{error}"),
                None => error,
            });
        }
    }
    Ok(None)
}
