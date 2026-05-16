use std::path::PathBuf;
use std::sync::Arc;
use tauri::State;

use crate::core::{
    central_repo, error::AppError, scenario_service, skill_store::SkillStore, sync_engine,
    sync_metadata, tool_adapters, tool_service, wsl_runtime,
};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct SkillToolToggleDto {
    pub tool: String,
    pub display_name: String,
    pub skills_dir: String,
    pub runtime_environment: String,
    pub wsl_distro_name: Option<String>,
    pub installed: bool,
    pub globally_enabled: bool,
    pub enabled: bool,
}

fn disabled_tools(store: &SkillStore) -> Vec<String> {
    tool_service::get_disabled_tools(store)
}

fn tool_globally_enabled(store: &SkillStore, tool: &str, disabled: &[String]) -> bool {
    wsl_runtime::parse_wsl_tool_key(tool)
        .map(|(distro_name, agent_key)| {
            wsl_runtime::agent_target_enabled(store, distro_name, agent_key)
        })
        .unwrap_or_else(|| !disabled.contains(&tool.to_string()))
}

fn sync_skill_to_tool_internal(
    store: &SkillStore,
    skill_id: &str,
    tool: &str,
) -> Result<(), AppError> {
    scenario_service::sync_single_skill_to_tool(store, skill_id, tool)
}

#[tauri::command]
pub async fn sync_skill_to_tool(
    skill_id: String,
    tool: String,
    store: State<'_, Arc<SkillStore>>,
) -> Result<(), AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        sync_skill_to_tool_internal(&store, &skill_id, &tool)?;

        if let Ok(Some(active_id)) = store.get_active_scenario_id() {
            let skill_ids = store
                .get_skill_ids_for_scenario(&active_id)
                .map_err(AppError::db)?;
            if skill_ids.contains(&skill_id) {
                let adapter_keys: Vec<String> = tool_adapters::enabled_installed_adapters(&store)
                    .iter()
                    .map(|a| a.key.clone())
                    .collect();
                store
                    .ensure_scenario_skill_tool_defaults(&active_id, &skill_id, &adapter_keys)
                    .map_err(AppError::db)?;
                store
                    .set_scenario_skill_tool_enabled(&active_id, &skill_id, &tool, true)
                    .map_err(AppError::db)?;
            }
        }

        Ok(())
    })
    .await?
}

#[tauri::command]
pub async fn unsync_skill_from_tool(
    skill_id: String,
    tool: String,
    store: State<'_, Arc<SkillStore>>,
) -> Result<(), AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let targets = store
            .get_targets_for_skill(&skill_id)
            .map_err(AppError::db)?;

        if let Some(target) = targets.iter().find(|t| t.tool == tool) {
            let target_path = PathBuf::from(&target.target_path);
            sync_engine::remove_target(&target_path).ok();
        }

        store
            .delete_target(&skill_id, &tool)
            .map_err(AppError::db)?;

        if let Ok(Some(active_id)) = store.get_active_scenario_id() {
            let skill_ids = store
                .get_skill_ids_for_scenario(&active_id)
                .map_err(AppError::db)?;
            if skill_ids.contains(&skill_id) {
                let adapter_keys: Vec<String> = tool_adapters::enabled_installed_adapters(&store)
                    .iter()
                    .map(|a| a.key.clone())
                    .collect();
                store
                    .ensure_scenario_skill_tool_defaults(&active_id, &skill_id, &adapter_keys)
                    .map_err(AppError::db)?;
                store
                    .set_scenario_skill_tool_enabled(&active_id, &skill_id, &tool, false)
                    .map_err(AppError::db)?;
            }
        }

        Ok(())
    })
    .await?
}

#[tauri::command]
pub async fn get_skill_tool_toggles(
    skill_id: String,
    scenario_id: String,
    store: State<'_, Arc<SkillStore>>,
) -> Result<Vec<SkillToolToggleDto>, AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let skill_ids = store
            .get_skill_ids_for_scenario(&scenario_id)
            .map_err(AppError::db)?;
        if !skill_ids.contains(&skill_id) {
            return Err(AppError::not_found("Skill is not enabled in this scenario"));
        }

        let disabled = disabled_tools(&store);
        let all_adapters = tool_adapters::all_tool_adapters(&store);
        let default_enabled_keys: Vec<String> = all_adapters
            .iter()
            .filter(|adapter| {
                adapter.is_installed() && tool_globally_enabled(&store, &adapter.key, &disabled)
            })
            .map(|adapter| adapter.key.clone())
            .collect();
        store
            .ensure_scenario_skill_tool_defaults(&scenario_id, &skill_id, &default_enabled_keys)
            .map_err(AppError::db)?;

        let toggles = store
            .get_scenario_skill_tool_toggles(&scenario_id, &skill_id)
            .map_err(AppError::db)?;
        let enabled_map: std::collections::HashMap<String, bool> = toggles
            .into_iter()
            .map(|toggle| (toggle.tool, toggle.enabled))
            .collect();

        Ok(all_adapters
            .into_iter()
            .map(|adapter| {
                let wsl_key = wsl_runtime::parse_wsl_tool_key(&adapter.key);
                let globally_enabled = tool_globally_enabled(&store, &adapter.key, &disabled);
                let available = adapter.is_installed() && globally_enabled;
                SkillToolToggleDto {
                    // Unavailable tools are always presented as disabled in UI.
                    enabled: if available {
                        enabled_map.get(&adapter.key).copied().unwrap_or(false)
                    } else {
                        false
                    },
                    tool: adapter.key.clone(),
                    display_name: adapter.display_name.clone(),
                    skills_dir: adapter.skills_dir().to_string_lossy().to_string(),
                    runtime_environment: wsl_key
                        .map(|_| "wsl".to_string())
                        .unwrap_or_else(|| "windows".to_string()),
                    wsl_distro_name: wsl_key.map(|(distro_name, _)| distro_name.to_string()),
                    installed: adapter.is_installed(),
                    globally_enabled,
                }
            })
            .collect())
    })
    .await?
}

#[tauri::command]
pub async fn set_skill_tool_toggle(
    skill_id: String,
    scenario_id: String,
    tool: String,
    enabled: bool,
    store: State<'_, Arc<SkillStore>>,
) -> Result<(), AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let skill_ids = store
            .get_skill_ids_for_scenario(&scenario_id)
            .map_err(AppError::db)?;
        if !skill_ids.contains(&skill_id) {
            return Err(AppError::not_found("Skill is not enabled in this scenario"));
        }

        let adapter = tool_adapters::find_adapter_with_store(&store, &tool)
            .ok_or_else(|| AppError::not_found(format!("Unknown tool: {}", tool)))?;
        let disabled = disabled_tools(&store);
        let globally_enabled = tool_globally_enabled(&store, &tool, &disabled);

        if enabled {
            if !adapter.is_installed() {
                return Err(AppError::not_found(format!(
                    "{} is not installed",
                    adapter.display_name
                )));
            }
            if !globally_enabled {
                return Err(AppError::invalid_input(format!(
                    "{} is disabled",
                    adapter.display_name
                )));
            }
        }

        sync_metadata::with_repo_lock("set skill tool toggle", || {
            store.set_scenario_skill_tool_enabled(&scenario_id, &skill_id, &tool, enabled)?;
            sync_metadata::write_all_from_db_unlocked(&store)
        })
        .map_err(AppError::db)?;

        let is_active = store
            .get_active_scenario_id()
            .map_err(AppError::db)?
            .as_deref()
            == Some(scenario_id.as_str());
        if is_active {
            if enabled {
                sync_skill_to_tool_internal(&store, &skill_id, &tool)?;
            } else {
                let targets = store
                    .get_targets_for_skill(&skill_id)
                    .map_err(AppError::db)?;
                if let Some(target) = targets.iter().find(|target| target.tool == tool) {
                    // Safe because the app currently guarantees a single active scenario.
                    sync_engine::remove_target(&PathBuf::from(&target.target_path)).ok();
                }
                store
                    .delete_target(&skill_id, &tool)
                    .map_err(AppError::db)?;
            }
        }

        Ok(())
    })
    .await?
}

#[tauri::command]
pub async fn sync_wsl_library_replica(
    distro_name: String,
    store: State<'_, Arc<SkillStore>>,
) -> Result<(), AppError> {
    let store = store.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let runtime = wsl_runtime::get_runtime_environment(&store, &distro_name)
            .map_err(|err| AppError::invalid_input(err.to_string()))?;
        let primary_library = central_repo::skills_dir();
        let library_replica = PathBuf::from(runtime.library_replica_path);
        sync_engine::sync_library_replica(&primary_library, &library_replica)
            .map_err(|err| AppError::io(format!("Failed to sync Library Replica: {err}")))
    })
    .await?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::skill_store::SkillRecord;
    use crate::core::tool_adapters::CustomToolDef;
    use std::fs;
    use tempfile::tempdir;

    fn sample_skill(id: &str, name: &str, central_path: &std::path::Path) -> SkillRecord {
        SkillRecord {
            id: id.to_string(),
            name: name.to_string(),
            description: None,
            source_type: "import".to_string(),
            source_ref: Some(central_path.to_string_lossy().to_string()),
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

    fn write_skill_dir(base: &std::path::Path, dir_name: &str, marker: &str) -> PathBuf {
        let dir = base.join(dir_name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {dir_name}\n---\n"),
        )
        .unwrap();
        fs::write(dir.join("unique.txt"), marker).unwrap();
        dir
    }

    fn configure_single_custom_tool(store: &SkillStore, target_base: &std::path::Path) {
        let custom_tools = vec![CustomToolDef {
            key: "test_agent".to_string(),
            display_name: "Test Agent".to_string(),
            skills_dir: target_base.to_string_lossy().to_string(),
            project_relative_skills_dir: None,
        }];
        store
            .set_setting(
                "custom_tools",
                &serde_json::to_string(&custom_tools).unwrap(),
            )
            .unwrap();
        let disabled_builtin_tools: Vec<String> = tool_adapters::default_tool_adapters()
            .into_iter()
            .map(|adapter| adapter.key)
            .collect();
        store
            .set_setting(
                "disabled_tools",
                &serde_json::to_string(&disabled_builtin_tools).unwrap(),
            )
            .unwrap();
        store.set_setting("sync_mode", "copy").unwrap();
    }

    #[test]
    fn sync_skill_to_tool_keeps_duplicate_skill_names_separate() {
        let tmp = tempdir().unwrap();
        let store = SkillStore::new(&tmp.path().join("test.db")).unwrap();
        let source_base = tmp.path().join("central");
        let target_base = tmp.path().join("agent-skills");
        fs::create_dir_all(&source_base).unwrap();
        fs::create_dir_all(&target_base).unwrap();
        configure_single_custom_tool(&store, &target_base);

        let first_dir = write_skill_dir(&source_base, "skill123", "first");
        let second_dir = write_skill_dir(&source_base, "skill123-2", "second");
        store
            .insert_skill(&sample_skill("first", "skill123", &first_dir))
            .unwrap();
        store
            .insert_skill(&sample_skill("second", "skill123", &second_dir))
            .unwrap();

        sync_skill_to_tool_internal(&store, "first", "test_agent").unwrap();
        sync_skill_to_tool_internal(&store, "second", "test_agent").unwrap();

        assert_eq!(
            fs::read_to_string(target_base.join("skill123/unique.txt")).unwrap(),
            "first"
        );
        assert_eq!(
            fs::read_to_string(target_base.join("skill123-2/unique.txt")).unwrap(),
            "second"
        );
    }
}
