use sgit::{Operation, Result, Store, Target};
use std::{env, path::Path, process::ExitCode};

const HELP: &str = "sgit — sync local Git worktrees

  sgit add <folder>
  sgit remove <id>
  sgit list
  sgit status
  sgit pull <id...> | --all
  sgit push <id...> | --all

Use `sgit push -- <id>` for an ID beginning with a dash.";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match run(&args) {
        Ok(false) => ExitCode::SUCCESS,
        Ok(true) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("sgit: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &[String]) -> Result<bool> {
    let parts: Vec<&str> = args.iter().map(String::as_str).collect();
    match parts.as_slice() {
        [] | ["help" | "-h" | "--help"] => {
            println!("{HELP}");
            return Ok(false);
        }
        ["--version" | "-V"] => {
            println!("sgit {}", sgit::DISPLAY_VERSION);
            return Ok(false);
        }
        _ => {}
    }
    let store = Store::for_user()?;
    match parts.as_slice() {
        ["add", folder] => {
            let repo = store.add(Path::new(folder))?;
            println!("{}\t{}", repo.id, repo.path.display());
        }
        ["remove", id] => store.remove(Target {
            id,
            expected_path: None,
        })?,
        ["list"] => {
            for repo in store.list()? {
                println!("{}\t{}", repo.id, repo.path.display());
            }
        }
        ["status"] => {
            for row in store.status()? {
                let branch = row.branch.as_deref().unwrap_or("—");
                let upstream = row.upstream.as_deref().unwrap_or("—");
                let distance = row
                    .ahead
                    .zip(row.behind)
                    .map(|(ahead, behind)| format!(" (ahead {ahead}, behind {behind})"))
                    .unwrap_or_default();
                println!(
                    "{}\t{} → {}{}\t{} changed, {} untracked",
                    row.repository.id, branch, upstream, distance, row.changes, row.untracked,
                );
                if let Some(problem) = row.problem {
                    println!("  {problem}");
                }
            }
        }
        [action @ ("pull" | "push"), selection @ ..] if !selection.is_empty() => {
            let ids = match selection {
                ["--all"] => None,
                ["--", ids @ ..] if !ids.is_empty() => Some(ids),
                ids if ids.iter().all(|id| !id.starts_with('-')) => Some(ids),
                _ => return Err(format!("Invalid repository selection.\n\n{HELP}")),
            };
            let targets = ids.map(|ids| {
                ids.iter()
                    .map(|id| Target {
                        id,
                        expected_path: None,
                    })
                    .collect::<Vec<_>>()
            });
            let operation = if *action == "pull" {
                Operation::Pull
            } else {
                Operation::Push
            };
            let mut failed = false;
            store.sync(targets.as_deref(), operation, |result| {
                failed |= result.outcome.failed();
                let commit = result
                    .commit
                    .map(|hash| format!(" [{hash}]"))
                    .unwrap_or_default();
                println!(
                    "{}\t{}{}\t{}",
                    result.id,
                    result.outcome.label(),
                    commit,
                    result.detail
                );
            })?;
            return Ok(failed);
        }
        _ => return Err(HELP.into()),
    }
    Ok(false)
}
