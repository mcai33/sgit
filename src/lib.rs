mod batch;
mod git;
mod registry;
mod scan;

pub use batch::{SyncApproval, SyncItem, SyncReport, SyncReview};
pub use registry::{Dashboard, Group};
use registry::{Registry, SavedRegistry};
pub use scan::{ScanCandidate, ScanResult};

use serde::{Deserialize, Serialize};
use std::{
    env, fs,
    fs::{File, OpenOptions, TryLockError},
    io::{self, Write},
    path::{Component, Path, PathBuf},
};

pub const DISPLAY_VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION_MAJOR"),
    ".",
    env!("CARGO_PKG_VERSION_MINOR")
);

pub type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Repository {
    pub id: String,
    pub path: PathBuf,
    #[serde(default)]
    pub group_id: Option<u32>,
}

pub struct Target<'a> {
    pub id: &'a str,
    pub expected_path: Option<&'a Path>,
}

impl Repository {
    pub fn target(&self) -> Target<'_> {
        Target {
            id: &self.id,
            expected_path: Some(&self.path),
        }
    }
}

impl Target<'_> {
    fn index(&self, repositories: &[Repository]) -> Result<usize> {
        let index = repositories
            .iter()
            .position(|repo| repo.id == self.id)
            .ok_or_else(|| format!("Unknown repository: {}", self.id))?;
        if self
            .expected_path
            .is_some_and(|path| repositories[index].path != path)
        {
            return Err(format!(
                "The registration for {} changed. Refresh the repository list and try again.",
                self.id
            ));
        }
        Ok(index)
    }
}

#[derive(Serialize)]
pub struct RemoteInfo {
    pub name: String,
    pub fetch_urls: Vec<String>,
    pub push_urls: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct RepositoryStatus {
    pub repository: Repository,
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: Option<u64>,
    pub behind: Option<u64>,
    pub changes: usize,
    pub untracked: usize,
    pub problem: Option<String>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Pull,
    Push,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    UpToDate,
    Pulled,
    Pushed,
    Blocked,
    CommittedPendingPush,
}

impl Outcome {
    pub fn label(self) -> &'static str {
        match self {
            Self::UpToDate => "up_to_date",
            Self::Pulled => "pulled",
            Self::Pushed => "pushed",
            Self::Blocked => "blocked",
            Self::CommittedPendingPush => "committed_pending_push",
        }
    }

    pub fn failed(self) -> bool {
        matches!(self, Self::Blocked | Self::CommittedPendingPush)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct SyncResult {
    pub id: String,
    pub outcome: Outcome,
    pub commit: Option<String>,
    pub detail: String,
}

struct StoreLock(File);

impl Drop for StoreLock {
    fn drop(&mut self) {
        // A concurrently spawned Git process may still hold an inherited descriptor.
        let _ = self.0.unlock();
    }
}

pub struct Store {
    directory: PathBuf,
}

impl Store {
    pub fn for_user() -> Result<Self> {
        let directory = match env::var_os("SGIT_CONFIG_DIR") {
            Some(path) => PathBuf::from(path),
            None => config_directory()?,
        };
        if !directory.is_absolute() {
            return Err("SGIT_CONFIG_DIR must be an absolute path".into());
        }
        Ok(Self { directory })
    }

    pub fn list(&self) -> Result<Vec<Repository>> {
        Ok(self.registry()?.repositories)
    }

    fn registry(&self) -> Result<Registry> {
        match fs::read(self.directory.join("repositories.json")) {
            Ok(bytes) => serde_json::from_slice::<SavedRegistry>(&bytes)
                .map(SavedRegistry::into_registry)
                .map_err(|error| format!("Cannot read repository list: {error}")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Registry::default()),
            Err(error) => Err(format!("Cannot read repository list: {error}")),
        }
    }

    pub fn add(&self, path: &Path) -> Result<Repository> {
        let path = git::root(path)?;
        let _lock = self.lock()?;
        let mut registry = self.registry()?;
        let count = registry.repositories.len();
        let repository = register(&mut registry.repositories, path);
        if registry.repositories.len() != count {
            self.save(&registry)?;
        }
        Ok(repository)
    }

    pub fn clone_repository(&self, address: &str, parent: &Path, name: &str) -> Result<Repository> {
        let address = address.trim();
        let name = name.trim();
        let mut components = Path::new(name).components();
        if !matches!(components.next(), Some(Component::Normal(_)))
            || components.next().is_some()
            || name.contains(['/', '\\', ':', '\0'])
            || name.eq_ignore_ascii_case(".git")
        {
            return Err("Enter a single folder name for the clone.".into());
        }
        if address.is_empty() {
            return Err("Enter a Git repository address.".into());
        }
        if !parent.is_absolute() {
            return Err("Choose an absolute destination directory.".into());
        }
        let parent = parent.canonicalize().map_err(|error| {
            format!(
                "Cannot open destination directory {}: {error}",
                parent.display()
            )
        })?;
        let destination = parent.join(name);
        fs::create_dir(&destination)
            .map_err(|error| format!("Cannot create {}: {error}", destination.display()))?;
        if let Err(error) = git::clone(address, &destination) {
            // Only remove an empty directory. Git or another process may have left files here.
            let _ = fs::remove_dir(&destination);
            return Err(error);
        }
        self.add(&destination).map_err(|error| {
            format!(
                "Cloned to {}, but could not add it to sgit: {error}",
                destination.display()
            )
        })
    }

    pub fn scan(&self, paths: &[PathBuf]) -> Result<ScanResult> {
        scan::scan(paths, &self.list()?)
    }

    pub fn add_scanned(&self, paths: &[PathBuf]) -> Result<()> {
        let _lock = self.lock()?;
        let mut registry = self.registry()?;
        let count = registry.repositories.len();
        for path in paths {
            let root = git::root(path).map_err(|error| format!("{}: {error}", path.display()))?;
            if root != *path {
                return Err(format!(
                    "The repository at {} has changed. Scan again or uncheck it.",
                    path.display()
                ));
            }
            register(&mut registry.repositories, root);
        }
        if registry.repositories.len() != count {
            self.save(&registry)?;
        }
        Ok(())
    }

    pub fn remove(&self, target: Target<'_>) -> Result<()> {
        let _lock = self.lock()?;
        let mut registry = self.registry()?;
        let index = target.index(&registry.repositories)?;
        registry.repositories.remove(index);
        self.save(&registry)
    }

    pub fn trash_repository(
        &self,
        repository: &Repository,
        move_to_trash: impl FnOnce(&Path) -> Result<()>,
    ) -> Result<()> {
        let _lock = self.lock()?;
        let mut registry = self.registry()?;
        let index = repository.target().index(&registry.repositories)?;
        let path = git::trashable_root(&registry.repositories[index])?;
        let config = self
            .directory
            .canonicalize()
            .map_err(|error| error.to_string())?;
        if config.starts_with(path) {
            return Err("This folder contains the sgit configuration directory.".into());
        }
        if registry
            .repositories
            .iter()
            .enumerate()
            .any(|(other, repo)| other != index && repo.path.starts_with(path))
        {
            return Err(
                "This folder contains another registered repository. Handle that repository first."
                    .into(),
            );
        }

        let removed = registry.repositories.remove(index);
        self.save(&registry)?;
        let result =
            move_to_trash(&removed.path).and_then(|()| match fs::symlink_metadata(&removed.path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(format!(
                    "Could not verify that the folder was moved: {error}"
                )),
                Ok(_) => Err("The folder still exists at its original location.".into()),
            });
        if let Err(error) = result {
            registry.repositories.insert(index, removed);
            self.save(&registry).map_err(|save_error| {
                format!("Could not move the folder to the trash: {error}. Could not restore its list entry: {save_error}")
            })?;
            return Err(error);
        }
        Ok(())
    }

    pub fn remotes(&self, repository: &Repository) -> Result<Vec<RemoteInfo>> {
        let registered = self.list()?;
        let index = repository.target().index(&registered)?;
        git::remotes(&registered[index])
    }

    pub fn status(&self) -> Result<Vec<RepositoryStatus>> {
        Ok(self.list()?.into_iter().map(git::inspect).collect())
    }

    pub fn sync(
        &self,
        targets: Option<&[Target<'_>]>,
        operation: Operation,
        mut report: impl FnMut(SyncResult),
    ) -> Result<()> {
        let _lock = self.lock()?;
        let repositories = self.list()?;
        let mut selected = vec![targets.is_none(); repositories.len()];
        if let Some(targets) = targets {
            for target in targets {
                selected[target.index(&repositories)?] = true;
            }
        }
        for (repo, selected) in repositories.iter().zip(selected) {
            if selected {
                report(git::sync(repo, operation));
            }
        }
        Ok(())
    }

    fn lock(&self) -> Result<StoreLock> {
        fs::create_dir_all(&self.directory)
            .map_err(|error| format!("Cannot create config directory: {error}"))?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.directory.join("sgit.lock"))
            .map_err(|error| format!("Cannot open sgit lock: {error}"))?;
        file.try_lock().map_err(|error| match error {
            TryLockError::WouldBlock => {
                "Another sgit operation is running. Try again when it finishes.".into()
            }
            TryLockError::Error(error) => format!("Cannot acquire sgit lock: {error}"),
        })?;
        Ok(StoreLock(file))
    }

    fn save(&self, registry: &Registry) -> Result<()> {
        let write = || -> std::result::Result<(), Box<dyn std::error::Error>> {
            let mut file = tempfile::NamedTempFile::new_in(&self.directory)?;
            serde_json::to_writer_pretty(&mut file, registry)?;
            file.write_all(b"\n")?;
            file.as_file().sync_all()?;
            file.persist(self.directory.join("repositories.json"))?;
            Ok(())
        };
        write().map_err(|error| format!("Cannot save repository list: {error}"))
    }
}

fn register(repositories: &mut Vec<Repository>, path: PathBuf) -> Repository {
    if let Some(existing) = repositories.iter().find(|repo| repo.path == path) {
        return existing.clone();
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("repo");
    let mut id = name.to_owned();
    let mut suffix = 2;
    while repositories.iter().any(|repo| repo.id == id) {
        id = format!("{name}-{suffix}");
        suffix += 1;
    }
    let repository = Repository {
        id,
        path,
        group_id: None,
    };
    repositories.push(repository.clone());
    repository
}

fn config_directory() -> Result<PathBuf> {
    if cfg!(windows) {
        return env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|path| path.join("sgit"))
            .ok_or_else(|| "APPDATA is not set".into());
    }
    if cfg!(target_os = "macos") {
        return env::home_dir()
            .map(|path| path.join("Library/Application Support/sgit"))
            .ok_or_else(|| "Cannot locate the home directory".into());
    }
    if let Some(path) = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        return Ok(path.join("sgit"));
    }
    env::home_dir()
        .map(|path| path.join(".config/sgit"))
        .ok_or_else(|| "Cannot locate the home directory".into())
}

#[cfg(test)]
mod scan_tests;

#[cfg(test)]
mod batch_tests;

#[cfg(test)]
mod grouping_tests;
