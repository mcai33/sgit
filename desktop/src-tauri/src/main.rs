#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod recycle;

use sgit::{
    Dashboard, FileAction, Group, RemoteInfo, Repository, Result, ScanResult, Store, SyncApproval,
    SyncReport, WorktreeChanges,
};
use std::path::PathBuf;
use tauri::{Manager, async_runtime::spawn_blocking};

const WINDOW_TITLE: &str = "仓库同步工具sgit";

fn display_title() -> String {
    format!("{WINDOW_TITLE} {}", sgit::DISPLAY_VERSION)
}

async fn with_store<T: Send + 'static>(
    action: impl FnOnce(Store) -> Result<T> + Send + 'static,
) -> Result<T> {
    spawn_blocking(move || action(Store::for_user()?))
        .await
        .map_err(|error| format!("Background task failed: {error}"))?
}

#[tauri::command]
async fn repository_status() -> Result<Dashboard> {
    with_store(|store| store.dashboard()).await
}

#[tauri::command]
async fn create_group(name: String) -> Result<Group> {
    with_store(move |store| store.create_group(&name)).await
}

#[tauri::command]
async fn assign_repository_group(
    repository: Repository,
    group_id: Option<u32>,
) -> Result<Repository> {
    with_store(move |store| store.assign_group(&repository, group_id)).await
}

#[tauri::command]
async fn repository_remotes(repository: Repository) -> Result<Vec<RemoteInfo>> {
    with_store(move |store| store.remotes(&repository)).await
}

#[tauri::command]
async fn repository_changes(repository: Repository) -> Result<WorktreeChanges> {
    with_store(move |store| store.worktree_changes(&repository)).await
}

#[tauri::command]
async fn apply_repository_file_action(
    repository: Repository,
    token: String,
    action: FileAction,
    file: Option<usize>,
) -> Result<WorktreeChanges> {
    with_store(move |store| store.apply_file_action(&repository, &token, action, file)).await
}

#[tauri::command]
async fn scan_repositories(paths: Vec<PathBuf>) -> Result<ScanResult> {
    with_store(move |store| store.scan(&paths)).await
}

#[tauri::command]
async fn add_scanned_repositories(paths: Vec<PathBuf>) -> Result<()> {
    with_store(move |store| store.add_scanned(&paths)).await
}

#[tauri::command]
async fn clone_repository(address: String, parent: PathBuf, name: String) -> Result<Repository> {
    with_store(move |store| store.clone_repository(&address, &parent, &name)).await
}

#[tauri::command]
async fn trash_repository(repository: Repository) -> Result<()> {
    with_store(move |store| store.trash_repository(&repository, recycle::move_to_trash)).await
}

#[tauri::command]
async fn remove_repository(repository: Repository) -> Result<()> {
    with_store(move |store| store.remove(repository.target())).await
}

#[tauri::command]
async fn sync_repositories(
    repositories: Vec<Repository>,
    approvals: Option<Vec<SyncApproval>>,
) -> Result<SyncReport> {
    with_store(move |store| match approvals {
        Some(approvals) => store.confirm_sync(&repositories, &approvals),
        None => store.check_sync(&repositories),
    })
    .await
}

fn main() {
    let builder = tauri::Builder::default();
    #[cfg(target_os = "macos")]
    let builder = builder.menu(|app| {
        use tauri::menu::{Menu, MenuItemKind};
        let menu = Menu::default(app)?;
        let items = menu.items()?;
        if let Some(MenuItemKind::Submenu(application)) = items.first() {
            application.set_text(display_title())?;
        }
        for item in items {
            if let MenuItemKind::Submenu(submenu) = item
                && submenu.text()? == "View"
            {
                // Tauri's default View menu only contains the full-screen action on macOS.
                menu.remove(&submenu)?;
                break;
            }
        }
        Ok(menu)
    });
    builder
        .setup(|app| {
            if let Some(window) = app.get_webview_window("main") {
                window.set_title(&display_title())?;
            }
            Ok(())
        })
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            repository_status,
            create_group,
            assign_repository_group,
            repository_remotes,
            repository_changes,
            apply_repository_file_action,
            scan_repositories,
            add_scanned_repositories,
            clone_repository,
            remove_repository,
            trash_repository,
            sync_repositories,
        ])
        .run(tauri::generate_context!())
        .expect("Unable to run sgit");
}
