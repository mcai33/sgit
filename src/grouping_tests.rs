use super::*;
use tempfile::tempdir;

#[test]
fn groups_and_assignments_survive_reopening_a_legacy_registry() {
    let temp = tempdir().unwrap();
    let store = Store {
        directory: temp.path().to_owned(),
    };
    let path = temp.path().join("existing");
    fs::write(
        temp.path().join("repositories.json"),
        serde_json::to_vec(&serde_json::json!([{ "id": "existing", "path": path }])).unwrap(),
    )
    .unwrap();

    let original = store.list().unwrap().remove(0);
    assert_eq!(original.group_id, None);
    let group = store.create_group(" 工作 ").unwrap();
    assert_eq!(group.name, "工作");
    assert!(store.create_group("工作").is_err());
    assert!(store.assign_group(&original, Some(group.id + 1)).is_err());
    store.assign_group(&original, Some(group.id)).unwrap();

    let reopened = Store {
        directory: temp.path().to_owned(),
    };
    let snapshot = reopened.dashboard().unwrap();
    assert_eq!(snapshot.groups[0].name, "工作");
    assert_eq!(snapshot.repositories[0].repository.group_id, Some(group.id));
    let stale = Repository {
        path: temp.path().join("replacement"),
        ..original.clone()
    };
    assert!(reopened.assign_group(&stale, None).is_err());
    reopened.assign_group(&original, None).unwrap();
    assert_eq!(reopened.list().unwrap()[0].group_id, None);
    reopened.remove(original.target()).unwrap();
    assert_eq!(reopened.dashboard().unwrap().groups.len(), 1);
}

#[test]
#[cfg(unix)]
fn dropping_the_guard_releases_the_lock_even_with_an_inherited_descriptor() {
    let temp = tempdir().unwrap();
    let store = Store {
        directory: temp.path().to_owned(),
    };
    let guard = store.lock().unwrap();
    let _inherited_descriptor = guard.0.try_clone().unwrap();
    assert!(store.lock().is_err());
    drop(guard);
    assert!(store.lock().is_ok());
}
