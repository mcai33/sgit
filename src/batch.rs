use crate::{Repository, Result, Store, git};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct SyncReview {
    pub branch: String,
    pub head: String,
    pub fingerprint: String,
    pub upstream: String,
    pub untracked: Vec<String>,
    pub needs_push: bool,
    pub tracked: usize,
    pub ahead: u64,
}

#[derive(Deserialize)]
pub struct SyncApproval {
    pub repository: Repository,
    pub review: SyncReview,
    pub include: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct SyncItem {
    pub repository: Repository,
    pub review: Option<SyncReview>,
    pub problem: Option<String>,
}

#[derive(Serialize)]
pub struct SyncReport {
    pub items: Vec<SyncItem>,
}

impl SyncReport {
    pub fn ready(&self) -> bool {
        self.items.is_empty()
    }
}

impl Store {
    pub fn check_sync(&self, selection: &[Repository]) -> Result<SyncReport> {
        let _lock = self.lock()?;
        Ok(check(&self.selected_repositories(selection)?, &[], false))
    }

    pub fn confirm_sync(
        &self,
        selection: &[Repository],
        approvals: &[SyncApproval],
    ) -> Result<SyncReport> {
        let _lock = self.lock()?;
        let repositories = self.selected_repositories(selection)?;
        let result = check(&repositories, approvals, false);
        if !result.ready() {
            return Ok(result);
        }
        // A later push in this batch can change the upstream of an earlier repository.
        Ok(check(&repositories, approvals, true))
    }
    fn selected_repositories(&self, selection: &[Repository]) -> Result<Vec<Repository>> {
        let registered = self.list()?;
        let indexes = selection
            .iter()
            .map(|repository| repository.target().index(&registered))
            .collect::<Result<BTreeSet<_>>>()?;
        Ok(registered
            .into_iter()
            .enumerate()
            .filter_map(|(index, repository)| indexes.contains(&index).then_some(repository))
            .collect())
    }
}

fn check(repositories: &[Repository], approvals: &[SyncApproval], verify_only: bool) -> SyncReport {
    let items = repositories
        .iter()
        .filter_map(|repository| {
            let approval = approvals.iter().find(|approval| {
                approval.repository.id == repository.id
                    && approval.repository.path == repository.path
            });
            let (review, problem) = match git::batch_step(repository, approval, verify_only) {
                Ok(None) => return None,
                Ok(review) => (review, None),
                Err(problem) => (None, Some(problem)),
            };
            Some(SyncItem {
                repository: repository.clone(),
                review,
                problem,
            })
        })
        .collect();
    SyncReport { items }
}
