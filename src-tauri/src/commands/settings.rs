use semver::Version;
use std::process::Command;
use std::sync::Arc;
use tauri::State;

use crate::core::{
    central_repo, error::AppError, skill_store::SkillStore, skillssh_api, sync_engine, wsl_runtime,
};

#[derive(serde::Serialize)]
pub struct AppUpdateInfo {
    pub has_update: bool,
    pub current_version: String,
    pub latest_version: String,
    pub release_url: String,
}

#[derive(serde::Serialize)]
pub struct WslRuntimeEnvironmentDto {
    pub distro_name: String,
    pub library_replica_path: String,
    pub reachable: bool,
}

#[tauri::command]
pub async fn get_settings(
    key: String,
    store: State<'_, Arc<SkillStore>>,
) -> Result<Option<String>, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || store.get_setting(&key).map_err(AppError::db))
        .await?
}

#[tauri::command]
pub async fn set_settings(
    app: tauri::AppHandle,
    key: String,
    value: String,
    store: State<'_, Arc<SkillStore>>,
) -> Result<(), AppError> {
    let store = store.inner().clone();
    let key_for_store = key.clone();
    let value_for_store = value.clone();
    tauri::async_runtime::spawn_blocking(move || {
        store
            .set_setting(&key_for_store, &value_for_store)
            .map_err(AppError::db)?;
        if key_for_store == "show_tray_icon" {
            let tray_enabled = matches!(
                value_for_store.trim().to_ascii_lowercase().as_str(),
                "true" | "1" | "yes" | "on"
            );
            if !tray_enabled {
                store
                    .set_setting("close_action", "close")
                    .map_err(AppError::db)?;
            }
        }
        Ok::<(), AppError>(())
    })
    .await??;

    if key == "show_tray_icon" {
        let enabled = matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "true" | "1" | "yes" | "on"
        );
        crate::set_tray_icon_enabled(&app, enabled).map_err(AppError::io)?;
    }
    Ok(())
}

#[tauri::command]
pub fn get_central_repo_path() -> String {
    central_repo::base_dir().to_string_lossy().to_string()
}

#[tauri::command]
pub fn get_central_repo_path_override() -> Option<String> {
    central_repo::configured_base_dir().map(|path| path.to_string_lossy().to_string())
}

#[tauri::command]
pub async fn set_central_repo_path(path: Option<String>) -> Result<String, AppError> {
    tauri::async_runtime::spawn_blocking(move || {
        central_repo::set_base_dir_override(path)
            .map(|resolved| resolved.to_string_lossy().to_string())
            .map_err(AppError::io)
    })
    .await?
}

#[tauri::command]
pub async fn list_wsl_runtime_environments(
    store: State<'_, Arc<SkillStore>>,
) -> Result<Vec<WslRuntimeEnvironmentDto>, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        wsl_runtime::list_runtime_environments(&store)
            .map(|items| {
                items
                    .into_iter()
                    .map(|item| WslRuntimeEnvironmentDto {
                        distro_name: item.distro_name,
                        library_replica_path: item.library_replica_path,
                        reachable: item.reachable,
                    })
                    .collect()
            })
            .map_err(AppError::internal)
    })
    .await?
}

#[tauri::command]
pub async fn add_wsl_runtime_environment(
    distro_name: String,
    library_replica_path: String,
    store: State<'_, Arc<SkillStore>>,
) -> Result<WslRuntimeEnvironmentDto, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        wsl_runtime::add_runtime_environment(&store, &distro_name, &library_replica_path)
            .map(|item| WslRuntimeEnvironmentDto {
                distro_name: item.distro_name,
                library_replica_path: item.library_replica_path,
                reachable: item.reachable,
            })
            .map_err(|err| AppError::invalid_input(err.to_string()))
    })
    .await?
}

#[tauri::command]
pub async fn remove_wsl_runtime_environment(
    distro_name: String,
    store: State<'_, Arc<SkillStore>>,
) -> Result<(), AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        remove_wsl_runtime_environment_internal(&store, &distro_name)
    })
    .await?
}

fn remove_wsl_runtime_environment_internal(
    store: &SkillStore,
    distro_name: &str,
) -> Result<(), AppError> {
    let targets = store.get_all_targets().map_err(AppError::db)?;
    for target in targets {
        let Some((target_distro, _agent_key)) = wsl_runtime::parse_wsl_tool_key(&target.tool)
        else {
            continue;
        };
        if !target_distro.eq_ignore_ascii_case(distro_name) {
            continue;
        }

        sync_engine::remove_target(std::path::Path::new(&target.target_path)).ok();
        store
            .delete_target(&target.skill_id, &target.tool)
            .map_err(AppError::db)?;
    }

    wsl_runtime::remove_runtime_environment(store, distro_name)
        .map_err(|err| AppError::invalid_input(err.to_string()))
}

#[tauri::command]
pub async fn open_central_repo_folder() -> Result<(), AppError> {
    tauri::async_runtime::spawn_blocking(|| {
        let repo_path = central_repo::base_dir();

        #[cfg(target_os = "macos")]
        let mut cmd = Command::new("open");
        #[cfg(target_os = "windows")]
        let mut cmd = {
            let mut c = Command::new("explorer");
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x08000000); // CREATE_NO_WINDOW
            c
        };
        #[cfg(target_os = "linux")]
        let mut cmd = Command::new("xdg-open");

        let status = cmd
            .arg(&repo_path)
            .status()
            .map_err(|e| AppError::io(format!("Failed to open folder: {e}")))?;

        // Windows explorer.exe returns exit code 1 even on success
        #[cfg(not(target_os = "windows"))]
        if !status.success() {
            return Err(AppError::io(format!(
                "File manager exited with status: {status}"
            )));
        }

        let _ = status;
        Ok(())
    })
    .await?
}

#[tauri::command]
pub async fn check_app_update(
    app: tauri::AppHandle,
    store: State<'_, Arc<SkillStore>>,
) -> Result<AppUpdateInfo, AppError> {
    let current_version = app.config().version.clone().unwrap_or_default();
    let proxy_url = store.proxy_url();
    tauri::async_runtime::spawn_blocking(move || {
        let client = skillssh_api::build_http_client(proxy_url.as_deref(), 15);

        let resp: serde_json::Value = client
            .get("https://api.github.com/repos/xingkongliang/skills-manager/releases/latest")
            .send()
            .map_err(|e| AppError::network(format!("Network error: {e}")))?
            .json()
            .map_err(|e| AppError::network(format!("Failed to parse response: {e}")))?;

        let tag = resp["tag_name"]
            .as_str()
            .ok_or_else(|| AppError::network("No tag_name in response"))?;
        let latest_version = tag.strip_prefix('v').unwrap_or(tag).to_string();
        let release_url = resp["html_url"]
            .as_str()
            .unwrap_or("https://github.com/xingkongliang/skills-manager/releases")
            .to_string();

        let has_update = version_gt(&latest_version, &current_version);

        Ok(AppUpdateInfo {
            has_update,
            current_version,
            latest_version,
            release_url,
        })
    })
    .await?
}

#[tauri::command]
pub async fn app_exit(app: tauri::AppHandle) {
    let app_for_main = app.clone();
    if let Err(err) = app.run_on_main_thread(move || crate::quit_app(&app_for_main)) {
        log::error!("Failed to schedule app_exit on main thread: {err}");
        crate::quit_app(&app);
    }
}

#[tauri::command]
pub async fn hide_to_tray(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    store: State<'_, Arc<SkillStore>>,
) -> Result<(), AppError> {
    let show_tray_icon = {
        let store = store.inner().clone();
        tauri::async_runtime::spawn_blocking(move || {
            let value = store.get_setting("show_tray_icon").map_err(AppError::db)?;
            Ok::<bool, AppError>(!matches!(
                value.as_deref().map(str::trim).map(str::to_ascii_lowercase),
                Some(v) if matches!(v.as_str(), "false" | "0" | "no" | "off")
            ))
        })
        .await??
    };

    if !show_tray_icon {
        crate::quit_app(&app);
        return Ok(());
    }

    window.hide().map_err(|e| AppError::io(e.to_string()))?;
    // On macOS, avoid app.hide() (app-level hidden state can block restore in tray flow).
    // Keep app running and hide only the window + Dock icon.
    #[cfg(target_os = "macos")]
    {
        app.set_dock_visibility(false)
            .map_err(|e| AppError::io(format!("Failed to hide Dock icon on macOS: {e}")))?;
        app.set_activation_policy(tauri::ActivationPolicy::Accessory)
            .map_err(|e| {
                AppError::io(format!("Failed to set activation policy to Accessory: {e}"))
            })?;
    }
    #[cfg(not(target_os = "macos"))]
    let _ = app;
    Ok(())
}

fn version_gt(a: &str, b: &str) -> bool {
    // Prefer strict SemVer comparison (supports pre-release/build metadata).
    if let (Ok(a_ver), Ok(b_ver)) = (Version::parse(a), Version::parse(b)) {
        return a_ver > b_ver;
    }

    // Fallback for non-SemVer tags.
    let parse = |s: &str| -> Vec<u64> { s.split('.').filter_map(|p| p.parse().ok()).collect() };
    parse(a) > parse(b)
}

#[cfg(test)]
mod tests {
    use super::remove_wsl_runtime_environment_internal;
    use crate::core::skill_store::{SkillRecord, SkillStore, SkillTargetRecord};

    fn sample_skill(central_path: &std::path::Path) -> SkillRecord {
        SkillRecord {
            id: "skill-1".to_string(),
            name: "demo-skill".to_string(),
            description: None,
            source_type: "local".to_string(),
            source_ref: None,
            source_ref_resolved: None,
            source_subpath: None,
            source_branch: None,
            source_revision: None,
            remote_revision: None,
            central_path: central_path.to_string_lossy().to_string(),
            content_hash: None,
            enabled: true,
            created_at: 1,
            updated_at: 1,
            status: "ok".to_string(),
            update_status: "local_only".to_string(),
            last_checked_at: None,
            last_check_error: None,
        }
    }

    fn sample_target(tool: &str, path: &std::path::Path) -> SkillTargetRecord {
        SkillTargetRecord {
            id: format!("target-{tool}"),
            skill_id: "skill-1".to_string(),
            tool: tool.to_string(),
            target_path: path.to_string_lossy().to_string(),
            mode: "copy".to_string(),
            status: "ok".to_string(),
            synced_at: Some(1),
            last_error: None,
        }
    }

    #[test]
    fn removing_wsl_runtime_unsyncs_matching_targets() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SkillStore::new(&tmp.path().join("settings.db")).unwrap();
        let central = tmp.path().join("center").join("demo-skill");
        let ubuntu_target = tmp.path().join("ubuntu-target");
        let debian_target = tmp.path().join("debian-target");
        let windows_target = tmp.path().join("windows-target");
        std::fs::create_dir_all(&central).unwrap();
        std::fs::create_dir_all(&ubuntu_target).unwrap();
        std::fs::create_dir_all(&debian_target).unwrap();
        std::fs::create_dir_all(&windows_target).unwrap();
        std::fs::write(central.join("SKILL.md"), "# demo").unwrap();

        store.insert_skill(&sample_skill(&central)).unwrap();
        store
            .set_setting(
                "wsl_runtime_environments",
                &serde_json::json!([
                    {
                        "distro_name": "Ubuntu",
                        "library_replica_path": tmp.path().join("ubuntu-replica").to_string_lossy(),
                        "agent_targets": [{ "key": "codex", "enabled": true, "skills_dir": ubuntu_target.to_string_lossy() }]
                    },
                    {
                        "distro_name": "Debian",
                        "library_replica_path": tmp.path().join("debian-replica").to_string_lossy(),
                        "agent_targets": [{ "key": "codex", "enabled": true, "skills_dir": debian_target.to_string_lossy() }]
                    }
                ])
                .to_string(),
            )
            .unwrap();
        store
            .insert_target(&sample_target("wsl:Ubuntu:codex", &ubuntu_target))
            .unwrap();
        store
            .insert_target(&sample_target("wsl:Debian:codex", &debian_target))
            .unwrap();
        store
            .insert_target(&sample_target("codex", &windows_target))
            .unwrap();

        remove_wsl_runtime_environment_internal(&store, "Ubuntu").unwrap();

        let targets = store.get_all_targets().unwrap();
        assert!(!targets
            .iter()
            .any(|target| target.tool == "wsl:Ubuntu:codex"));
        assert!(targets
            .iter()
            .any(|target| target.tool == "wsl:Debian:codex"));
        assert!(targets.iter().any(|target| target.tool == "codex"));
        assert!(!ubuntu_target.exists());
        assert!(debian_target.exists());
        assert!(windows_target.exists());
    }
}
