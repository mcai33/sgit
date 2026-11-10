use crate::{Repository, RepositoryStatus, Result, Store, git};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
pub struct Group {
    pub id: u32,
    pub name: String,
}

#[derive(Default, Serialize, Deserialize)]
pub(crate) struct Registry {
    pub repositories: Vec<Repository>,
    pub groups: Vec<Group>,
}

#[derive(Deserialize)]
#[serde(untagged)]
pub(crate) enum SavedRegistry {
    Current(Registry),
    Legacy(Vec<Repository>),
}

impl SavedRegistry {
    pub fn into_registry(self) -> Registry {
        match self {
            Self::Current(registry) => registry,
            Self::Legacy(repositories) => Registry {
                repositories,
                groups: Vec::new(),
            },
        }
    }
}

#[derive(Serialize)]
pub struct Dashboard {
    pub groups: Vec<Group>,
    pub repositories: Vec<RepositoryStatus>,
}

impl Store {
    pub fn dashboard(&self) -> Result<Dashboard> {
        let Registry {
            repositories,
            groups,
        } = self.registry()?;
        Ok(Dashboard {
            groups,
            repositories: repositories.into_iter().map(git::inspect).collect(),
        })
    }

    pub fn create_group(&self, name: &str) -> Result<Group> {
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 60 || name.chars().any(char::is_control) {
            return Err("Enter a group name of 1 to 60 characters.".into());
        }
        let _lock = self.lock()?;
        let mut registry = self.registry()?;
        if registry
            .groups
            .iter()
            .any(|group| group.name.eq_ignore_ascii_case(name))
        {
            return Err("A group with that name already exists.".into());
        }
        let id = registry
            .groups
            .iter()
            .map(|group| group.id)
            .max()
            .unwrap_or_default()
            .checked_add(1)
            .ok_or("Cannot add another group.")?;
        let group = Group {
            id,
            name: name.into(),
        };
        registry.groups.push(group.clone());
        self.save(&registry)?;
        Ok(group)
    }

    pub fn assign_group(
        &self,
        repository: &Repository,
        group_id: Option<u32>,
    ) -> Result<Repository> {
        let _lock = self.lock()?;
        let mut registry = self.registry()?;
        let index = repository.target().index(&registry.repositories)?;
        if group_id.is_some_and(|id| !registry.groups.iter().any(|group| group.id == id)) {
            return Err("This group no longer exists. Refresh the repository list.".into());
        }
        registry.repositories[index].group_id = group_id;
        self.save(&registry)?;
        Ok(registry.repositories.swap_remove(index))
    }
}
