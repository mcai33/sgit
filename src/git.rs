use crate::{
    Operation, Outcome, RemoteInfo, Repository, RepositoryStatus, Result, SyncApproval, SyncResult,
    SyncReview,
};
use std::{
    fs,
    hash::{DefaultHasher, Hash, Hasher},
    path::{Path, PathBuf},
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
    let output = git(
        path,
        &[
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
        ],
    )?;
    let mut changes = Changes::default();
    let mut tracked_status = DefaultHasher::new();
    for entry in output.split(|byte| *byte == 0) {
        match entry.get(..2) {
            Some(b"??") => changes.untracked.push(entry[3..].to_vec()),
            Some(code) => {
                changes.tracked += 1;
                if list_files {
                    entry.hash(&mut tracked_status);
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
            return Err("Finish the Git operation already in progress before syncing.".into());
        }
    }
    if changes.conflicts {
        return Err("Resolve unmerged files before syncing.".into());
    }
    Ok(())
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
        let message = format!(
            "sgit: sync {}",
            prepared.branch.trim_start_matches("refs/heads/")
        );
        git(path, &["commit", "--all", "--message", &message])?;
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
