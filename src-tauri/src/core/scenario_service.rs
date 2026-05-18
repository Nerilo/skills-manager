use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;

use super::{
    central_repo,
    error::AppError,
    skill_distribution,
    skill_store::{ScenarioRecord, SkillStore, SkillTargetRecord},
    sync_engine, tool_adapters, tool_service, wsl_runtime,
};

#[derive(Debug, Clone)]
pub struct ScenarioSyncTarget {
    pub skill_id: String,
    pub skill_name: String,
    pub tool: String,
    pub source: PathBuf,
    pub target: PathBuf,
    pub mode: sync_engine::SyncMode,
    /// Current content hash of the central skill source, copied from
    /// `SkillRecord.content_hash`. Compared against the previously
    /// synced `SkillTargetRecord.source_hash` to skip redundant
    /// Copy-mode resyncs at startup (issue #153).
    pub source_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SyncPreviewTarget {
    pub skill_id: String,
    pub skill_name: String,
    pub tool: String,
    pub target_path: String,
    pub mode: String,
    pub runtime_environment: String,
    pub wsl_distro_name: Option<String>,
}

pub fn ensure_scenario_exists(store: &SkillStore, scenario_id: &str) -> Result<(), AppError> {
    let exists = store
        .get_all_scenarios()
        .map_err(AppError::db)?
        .iter()
        .any(|s| s.id == scenario_id);
    if !exists {
        return Err(AppError::not_found("Scenario not found"));
    }
    Ok(())
}

pub fn enabled_installed_adapters_for_scenario_skill(
    store: &SkillStore,
    scenario_id: &str,
    skill_id: &str,
) -> Result<Vec<tool_adapters::ToolAdapter>, AppError> {
    let adapters = tool_adapters::enabled_installed_adapters(store);
    let adapter_keys: Vec<String> = adapters.iter().map(|a| a.key.clone()).collect();

    store
        .ensure_scenario_skill_tool_defaults(scenario_id, skill_id, &adapter_keys)
        .map_err(AppError::db)?;

    let enabled = store
        .get_enabled_tools_for_scenario_skill(scenario_id, skill_id)
        .map_err(AppError::db)?;
    let enabled_set: HashSet<String> = enabled.into_iter().collect();

    Ok(adapters
        .into_iter()
        .filter(|adapter| enabled_set.contains(&adapter.key))
        .collect())
}

pub fn collect_scenario_sync_targets(
    store: &SkillStore,
    scenario_id: &str,
) -> Result<Vec<ScenarioSyncTarget>, AppError> {
    let skills = store
        .get_skills_for_scenario(scenario_id)
        .map_err(AppError::db)?;
    let configured_mode = store.get_setting("sync_mode").map_err(AppError::db)?;
    let mut targets = Vec::new();

    for skill in &skills {
        let central_source = PathBuf::from(&skill.central_path);
        let target_name = sync_engine::target_dir_name(&central_source, &skill.name);
        let adapters =
            enabled_installed_adapters_for_scenario_skill(store, scenario_id, &skill.id)?;
        for adapter in &adapters {
            let source = skill_distribution::source_for_target(store, skill, &adapter.key)?;
            let target = adapter.skills_dir().join(&target_name);
            let mode = sync_engine::sync_mode_for_tool(&adapter.key, configured_mode.as_deref());
            targets.push(ScenarioSyncTarget {
                skill_id: skill.id.clone(),
                skill_name: skill.name.clone(),
                tool: adapter.key.clone(),
                source: source.clone(),
                target,
                mode,
                source_hash: skill.content_hash.clone(),
            });
        }
    }

    Ok(targets)
}

pub fn preview_scenario_sync(
    store: &SkillStore,
    scenario_id: &str,
) -> Result<Vec<SyncPreviewTarget>, AppError> {
    collect_scenario_sync_targets(store, scenario_id).map(|targets| {
        targets
            .into_iter()
            .map(|target| SyncPreviewTarget {
                runtime_environment: wsl_runtime::parse_wsl_tool_key(&target.tool)
                    .map(|_| "wsl".to_string())
                    .unwrap_or_else(|| "windows".to_string()),
                wsl_distro_name: wsl_runtime::parse_wsl_tool_key(&target.tool)
                    .map(|(distro_name, _)| distro_name.to_string()),
                skill_id: target.skill_id,
                skill_name: target.skill_name,
                tool: target.tool,
                target_path: target.target.to_string_lossy().to_string(),
                mode: target.mode.as_str().to_string(),
            })
            .collect()
    })
}

fn sync_wsl_library_replica_for_tool(store: &SkillStore, tool: &str) -> Result<(), AppError> {
    let Some((distro_name, _agent_key)) = wsl_runtime::parse_wsl_tool_key(tool) else {
        return Ok(());
    };
    let runtime = wsl_runtime::get_runtime_environment(store, distro_name)
        .map_err(|err| AppError::invalid_input(err.to_string()))?;
    sync_engine::sync_library_replica(
        &central_repo::skills_dir(),
        &PathBuf::from(runtime.library_replica_path),
    )
    .map_err(|err| AppError::io(format!("Failed to sync WSL Library Replica: {err}")))
}

fn tool_globally_enabled(store: &SkillStore, tool: &str) -> bool {
    wsl_runtime::parse_wsl_tool_key(tool)
        .map(|(distro_name, agent_key)| {
            wsl_runtime::agent_target_enabled(store, distro_name, agent_key)
        })
        .unwrap_or_else(|| !tool_service::get_disabled_tools(store).contains(&tool.to_string()))
}

fn sync_wsl_library_replicas_for_targets(
    store: &SkillStore,
    desired_targets: &[ScenarioSyncTarget],
) -> HashMap<String, String> {
    let mut synced = HashSet::new();
    let mut failed = HashMap::new();
    for target in desired_targets {
        let Some((distro_name, _agent_key)) = wsl_runtime::parse_wsl_tool_key(&target.tool) else {
            continue;
        };
        if synced.insert(distro_name.to_string()) {
            if let Err(err) = sync_wsl_library_replica_for_tool(store, &target.tool) {
                log::warn!("Failed to sync WSL Library Replica for {distro_name}: {err}");
                failed.insert(distro_name.to_string(), err.to_string());
            }
        }
    }
    failed
}

/// Decide which `SyncMode` `is_target_current` should compare against, or
/// `None` if the existing target's mode is incompatible with the desired
/// mode and the skip path must be refused.
///
/// Returns `Some(existing)` when both modes match exactly. Also returns
/// `Some(Copy)` when the existing record is `"copy"` but the desired
/// mode is `Symlink` — this is the Windows fallback case (issue #153):
/// `symlink_dir()` failed on a prior run and we landed in copy mode, so
/// every subsequent startup would re-attempt symlink, fail again, and
/// trigger a full recursive copy. Treating the existing copy as
/// compatible lets the hash gate skip when the source hasn't changed.
///
/// The reverse direction (existing `"symlink"`, desired `Copy`) returns
/// `None` because the user actively changed the `sync_mode` setting and
/// the on-disk symlink doesn't reflect that intent.
fn skip_check_mode(
    existing_mode: &str,
    desired: sync_engine::SyncMode,
) -> Option<sync_engine::SyncMode> {
    match (existing_mode, desired) {
        ("symlink", sync_engine::SyncMode::Symlink) => Some(sync_engine::SyncMode::Symlink),
        ("symlink", sync_engine::SyncMode::WslSymlink) => Some(sync_engine::SyncMode::WslSymlink),
        ("copy", sync_engine::SyncMode::Copy) => Some(sync_engine::SyncMode::Copy),
        ("copy", sync_engine::SyncMode::Symlink) => Some(sync_engine::SyncMode::Copy),
        _ => None,
    }
}

pub fn sync_desired_targets(
    store: &SkillStore,
    desired_targets: &[ScenarioSyncTarget],
) -> Result<(), AppError> {
    let failed_wsl_replicas = sync_wsl_library_replicas_for_targets(store, desired_targets);
    let batch_start = Instant::now();
    let existing_targets: HashMap<(String, String), SkillTargetRecord> = store
        .get_all_targets()
        .map_err(AppError::db)?
        .into_iter()
        .map(|target| ((target.skill_id.clone(), target.tool.clone()), target))
        .collect();

    let mut synced_count = 0usize;
    let mut skipped_count = 0usize;
    let mut failed_count = 0usize;

    for desired in desired_targets {
        let target_start = Instant::now();
        if let Some((distro_name, _agent_key)) = wsl_runtime::parse_wsl_tool_key(&desired.tool) {
            if failed_wsl_replicas.contains_key(distro_name) {
                log::warn!(
                    "Skipping WSL target {} because its Library Replica failed to sync",
                    desired.target.display()
                );
                failed_count += 1;
                continue;
            }
        }

        let key = (desired.skill_id.clone(), desired.tool.clone());
        if let Some(existing) = existing_targets.get(&key) {
            let target_path = PathBuf::from(&existing.target_path);
            if target_path != desired.target {
                if let Err(e) = sync_engine::remove_target(&target_path) {
                    log::warn!(
                        "Failed to remove stale target {}: {e}",
                        target_path.display()
                    );
                }
                if let Err(e) = store.delete_target(&desired.skill_id, &desired.tool) {
                    log::warn!(
                        "Failed to delete stale target record for skill {}, tool {}: {e}",
                        desired.skill_id,
                        desired.tool
                    );
                }
            } else if existing.status == "ok" {
                if let Some(check_mode) = skip_check_mode(&existing.mode, desired.mode) {
                    if sync_engine::is_target_current(
                        &desired.source,
                        &desired.target,
                        check_mode,
                        existing.source_hash.as_deref(),
                        desired.source_hash.as_deref(),
                    ) {
                        // Surface the Windows fallback case in logs so operators
                        // can tell when a target is permanently on Copy because
                        // an earlier symlink_dir() failed (issue #153). Helpful
                        // when a user later enables Developer Mode and wonders
                        // why Symlink isn't being re-attempted.
                        if existing.mode == "copy"
                            && matches!(desired.mode, sync_engine::SyncMode::Symlink)
                        {
                            log::debug!(
                                "sync_desired_targets: skill {} ({}) staying on copy fallback for {} (content unchanged); trigger a manual resync to retry symlink",
                                desired.skill_id,
                                desired.skill_name,
                                desired.tool
                            );
                        }
                        skipped_count += 1;
                        continue;
                    }
                }
            }
        }

        match sync_engine::sync_skill(&desired.source, &desired.target, desired.mode) {
            Ok(actual_mode) => {
                let now = chrono::Utc::now().timestamp_millis();
                let target_record = SkillTargetRecord {
                    id: uuid::Uuid::new_v4().to_string(),
                    skill_id: desired.skill_id.clone(),
                    tool: desired.tool.clone(),
                    target_path: desired.target.to_string_lossy().to_string(),
                    mode: actual_mode.as_str().to_string(),
                    status: "ok".to_string(),
                    synced_at: Some(now),
                    last_error: None,
                    // Record the hash that was just synced so the next
                    // run of this loop can short-circuit when the central
                    // skill content has not changed (issue #153).
                    source_hash: desired.source_hash.clone(),
                };
                if let Err(e) = store.insert_target(&target_record) {
                    log::warn!(
                        "Failed to insert sync target for skill {}: {e}",
                        desired.skill_id
                    );
                }
                synced_count += 1;
                let elapsed = target_start.elapsed().as_millis();
                if elapsed >= 200 {
                    log::warn!(
                        "sync_desired_targets: slow sync ({elapsed} ms, mode={}) for skill {} ({}) -> {}",
                        actual_mode.as_str(),
                        desired.skill_id,
                        desired.skill_name,
                        desired.target.display()
                    );
                }
            }
            Err(e) => {
                failed_count += 1;
                log::warn!(
                    "Failed to sync skill {} ({}) to {} after {} ms: {e}",
                    desired.skill_id,
                    desired.skill_name,
                    desired.target.display(),
                    target_start.elapsed().as_millis()
                );
            }
        }
    }

    if !failed_wsl_replicas.is_empty() {
        let mut failures: Vec<String> = failed_wsl_replicas
            .into_iter()
            .map(|(distro, err)| format!("{distro}: {err}"))
            .collect();
        failures.sort();
        return Err(AppError::io(format!(
            "Failed to sync WSL Library Replica: {}",
            failures.join("; ")
        )));
    }

    log::info!(
        "sync_desired_targets: {} targets in {} ms (synced={synced_count}, skipped={skipped_count}, failed={failed_count})",
        desired_targets.len(),
        batch_start.elapsed().as_millis()
    );

    Ok(())
}

pub fn unsync_obsolete_scenario_targets(
    store: &SkillStore,
    old_scenario_id: &str,
    desired_targets: &[ScenarioSyncTarget],
) -> Result<(), AppError> {
    let desired_paths: HashMap<(String, String), PathBuf> = desired_targets
        .iter()
        .map(|target| {
            (
                (target.skill_id.clone(), target.tool.clone()),
                target.target.clone(),
            )
        })
        .collect();

    let old_skill_ids = store
        .get_skill_ids_for_scenario(old_scenario_id)
        .map_err(AppError::db)?;
    for skill_id in &old_skill_ids {
        let targets = store.get_targets_for_skill(skill_id).unwrap_or_default();
        for target in &targets {
            let path = PathBuf::from(&target.target_path);
            let key = (skill_id.clone(), target.tool.clone());
            if desired_paths.get(&key) == Some(&path) {
                continue;
            }

            if let Err(e) = sync_engine::remove_target(&path) {
                log::warn!("Failed to remove sync target {}: {e}", path.display());
            }
            if let Err(e) = store.delete_target(skill_id, &target.tool) {
                log::warn!(
                    "Failed to delete target record for skill {skill_id}, tool {}: {e}",
                    target.tool
                );
            }
        }
    }

    Ok(())
}

pub fn unsync_scenario_skills(store: &SkillStore, scenario_id: &str) -> Result<(), AppError> {
    let skill_ids = store
        .get_skill_ids_for_scenario(scenario_id)
        .map_err(AppError::db)?;

    for skill_id in &skill_ids {
        let targets = store.get_targets_for_skill(skill_id).unwrap_or_default();
        for target in &targets {
            let path = PathBuf::from(&target.target_path);
            if let Err(e) = sync_engine::remove_target(&path) {
                log::warn!("Failed to remove sync target {}: {e}", path.display());
            }
            if let Err(e) = store.delete_target(skill_id, &target.tool) {
                log::warn!(
                    "Failed to delete target record for skill {skill_id}, tool {}: {e}",
                    target.tool
                );
            }
        }
    }

    Ok(())
}

pub fn sync_scenario_skills(store: &SkillStore, scenario_id: &str) -> Result<(), AppError> {
    let desired_targets = collect_scenario_sync_targets(store, scenario_id)?;
    sync_desired_targets(store, &desired_targets)
}

pub fn apply_scenario_to_default(store: &SkillStore, scenario_id: &str) -> Result<(), AppError> {
    ensure_scenario_exists(store, scenario_id)?;
    let desired_targets = collect_scenario_sync_targets(store, scenario_id)?;

    if let Ok(Some(old_id)) = store.get_active_scenario_id() {
        if old_id != scenario_id {
            unsync_obsolete_scenario_targets(store, &old_id, &desired_targets)?;
        }
    }

    store
        .set_active_scenario(scenario_id)
        .map_err(AppError::db)?;
    sync_desired_targets(store, &desired_targets)
}

pub fn sync_skill_to_active_scenario(
    store: &SkillStore,
    scenario_id: &str,
    skill_id: &str,
) -> Result<(), AppError> {
    if let Ok(Some(active_id)) = store.get_active_scenario_id() {
        if active_id == scenario_id {
            let adapters =
                enabled_installed_adapters_for_scenario_skill(store, scenario_id, skill_id)?;
            let configured_mode = store.get_setting("sync_mode").map_err(AppError::db)?;
            let Ok(Some(skill)) = store.get_skill_by_id(skill_id) else {
                return Ok(());
            };
            let central_source = PathBuf::from(&skill.central_path);
            let target_name = sync_engine::target_dir_name(&central_source, &skill.name);
            let old_targets = store.get_targets_for_skill(skill_id).unwrap_or_default();
            for adapter in &adapters {
                if let Some(old) = old_targets.iter().find(|t| t.tool == adapter.key) {
                    let old_path = PathBuf::from(&old.target_path);
                    if old_path != adapter.skills_dir().join(&target_name) {
                        if let Err(e) = sync_engine::remove_target(&old_path) {
                            log::warn!("Failed to remove stale target {}: {e}", old_path.display());
                        }
                        let _ = store.delete_target(skill_id, &adapter.key);
                    }
                }

                let target = adapter.skills_dir().join(&target_name);
                if let Err(err) = sync_wsl_library_replica_for_tool(store, &adapter.key) {
                    log::warn!(
                        "Failed to sync WSL Library Replica for {} before syncing {}: {err}",
                        adapter.key,
                        target.display()
                    );
                    continue;
                }
                let source = skill_distribution::source_for_target(store, &skill, &adapter.key)?;
                let mode =
                    sync_engine::sync_mode_for_tool(&adapter.key, configured_mode.as_deref());
                match sync_engine::sync_skill(&source, &target, mode) {
                    Ok(actual_mode) => {
                        let now = chrono::Utc::now().timestamp_millis();
                        let target_record = super::skill_store::SkillTargetRecord {
                            id: uuid::Uuid::new_v4().to_string(),
                            skill_id: skill_id.to_string(),
                            tool: adapter.key.clone(),
                            target_path: target.to_string_lossy().to_string(),
                            mode: actual_mode.as_str().to_string(),
                            status: "ok".to_string(),
                            synced_at: Some(now),
                            last_error: None,
                            source_hash: skill.content_hash.clone(),
                        };
                        if let Err(e) = store.insert_target(&target_record) {
                            log::warn!("Failed to insert sync target for skill {skill_id}: {e}");
                        }
                    }
                    Err(e) => {
                        log::warn!(
                            "Failed to sync skill {skill_id} to {}: {e}",
                            target.display()
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

pub fn ensure_default_startup_scenario(store: &SkillStore) -> Result<(), AppError> {
    let mut scenarios = store.get_all_scenarios().map_err(AppError::db)?;
    if scenarios.is_empty() {
        let now = chrono::Utc::now().timestamp_millis();
        let default_scenario = ScenarioRecord {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Default".to_string(),
            description: Some("Default startup scenario".to_string()),
            icon: None,
            sort_order: 0,
            created_at: now,
            updated_at: now,
        };
        store
            .insert_scenario(&default_scenario)
            .map_err(AppError::db)?;
        scenarios.push(default_scenario);
    }

    let current_active = store.get_active_scenario_id().map_err(AppError::db)?;
    let preferred_default = store.get_setting("default_scenario").ok().flatten();

    let desired_active = preferred_default
        .filter(|id| scenarios.iter().any(|scenario| scenario.id == *id))
        .or_else(|| {
            current_active
                .clone()
                .filter(|id| scenarios.iter().any(|scenario| scenario.id == *id))
        })
        .unwrap_or_else(|| scenarios[0].id.clone());

    if current_active.as_deref() != Some(desired_active.as_str()) {
        if let Some(old_active) = current_active.as_deref() {
            unsync_scenario_skills(store, old_active)?;
        }
        store
            .set_active_scenario(&desired_active)
            .map_err(AppError::db)?;
    }

    sync_scenario_skills(store, &desired_active)
}

pub fn ensure_cli_scenario_state(store: &SkillStore) -> Result<(), AppError> {
    let mut scenarios = store.get_all_scenarios().map_err(AppError::db)?;
    if scenarios.is_empty() {
        let now = chrono::Utc::now().timestamp_millis();
        let default_scenario = ScenarioRecord {
            id: uuid::Uuid::new_v4().to_string(),
            name: "Default".to_string(),
            description: Some("Default startup scenario".to_string()),
            icon: None,
            sort_order: 0,
            created_at: now,
            updated_at: now,
        };
        store
            .insert_scenario(&default_scenario)
            .map_err(AppError::db)?;
        scenarios.push(default_scenario);
    }

    let current_active = store.get_active_scenario_id().map_err(AppError::db)?;
    if current_active
        .as_deref()
        .is_some_and(|id| scenarios.iter().any(|scenario| scenario.id == id))
    {
        return Ok(());
    }

    let preferred_default = store.get_setting("default_scenario").ok().flatten();
    let desired_active = preferred_default
        .filter(|id| scenarios.iter().any(|scenario| scenario.id == *id))
        .unwrap_or_else(|| scenarios[0].id.clone());

    store
        .set_active_scenario(&desired_active)
        .map_err(AppError::db)
}

pub fn restore_all_skills_sync_included(store: &SkillStore) -> Result<bool, AppError> {
    let mut changed = false;
    for skill in store.get_all_skills().map_err(AppError::db)? {
        if !skill.enabled {
            store
                .update_skill_enabled(&skill.id, true)
                .map_err(AppError::db)?;
            changed = true;
        }
    }
    Ok(changed)
}

pub fn sync_active_scenario_to_tool(store: &SkillStore, tool_key: &str) {
    if let Ok(Some(active_id)) = store.get_active_scenario_id() {
        let Ok(skill_ids) = store.get_skill_ids_for_scenario(&active_id) else {
            return;
        };
        for skill_id in skill_ids {
            if let Ok(adapters) =
                enabled_installed_adapters_for_scenario_skill(store, &active_id, &skill_id)
            {
                if adapters.iter().any(|adapter| adapter.key == tool_key) {
                    let _ = sync_skill_to_active_scenario(store, &active_id, &skill_id);
                }
            }
        }
    }
}

pub fn sync_single_skill_to_tool(
    store: &SkillStore,
    skill_id: &str,
    tool: &str,
) -> Result<(), AppError> {
    let adapter = tool_adapters::find_adapter_with_store(store, tool)
        .ok_or_else(|| AppError::not_found(format!("Unknown tool: {}", tool)))?;

    if !adapter.is_installed() {
        return Err(AppError::not_found(format!(
            "{} is not installed",
            adapter.display_name
        )));
    }

    if !tool_globally_enabled(store, tool) {
        return Err(AppError::invalid_input(format!(
            "{} is disabled",
            adapter.display_name
        )));
    }

    let skill = store
        .get_skill_by_id(skill_id)
        .map_err(AppError::db)?
        .ok_or_else(|| AppError::not_found("Skill not found"))?;

    sync_wsl_library_replica_for_tool(store, tool)?;
    let source = skill_distribution::source_for_target(store, &skill, tool)?;
    let target = adapter
        .skills_dir()
        .join(sync_engine::target_dir_name(&source, &skill.name));
    let configured_mode = store.get_setting("sync_mode").map_err(AppError::db)?;
    let mode = sync_engine::sync_mode_for_tool(tool, configured_mode.as_deref());
    let actual_mode = sync_engine::sync_skill(&source, &target, mode).map_err(AppError::io)?;

    let now = chrono::Utc::now().timestamp_millis();
    let target_record = SkillTargetRecord {
        id: uuid::Uuid::new_v4().to_string(),
        skill_id: skill_id.to_string(),
        tool: tool.to_string(),
        target_path: target.to_string_lossy().to_string(),
        mode: actual_mode.as_str().to_string(),
        status: "ok".to_string(),
        synced_at: Some(now),
        last_error: None,
        source_hash: skill.content_hash.clone(),
    };

    store.insert_target(&target_record).map_err(AppError::db)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::skill_store::{ScenarioRecord, SkillRecord};
    use std::fs;
    use tempfile::tempdir;

    fn sample_skill(central_path: &std::path::Path) -> SkillRecord {
        SkillRecord {
            id: "skill-1".to_string(),
            name: "demo-skill".to_string(),
            description: None,
            source_type: "import".to_string(),
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

    fn sample_scenario() -> ScenarioRecord {
        ScenarioRecord {
            id: "default".to_string(),
            name: "Default".to_string(),
            description: None,
            icon: None,
            sort_order: 0,
            created_at: 1,
            updated_at: 1,
        }
    }

    #[test]
    fn wsl_target_sync_refreshes_library_replica_before_linking_agent() {
        let _guard = central_repo::test_base_dir_lock();
        let tmp = tempdir().unwrap();
        central_repo::set_test_base_dir_override(Some(tmp.path().join("center")));
        let store = SkillStore::new(&tmp.path().join("wsl-sync.db")).unwrap();
        let central_skill = central_repo::skills_dir().join("demo-skill");
        let replica_root = tmp.path().join(".skills-manager");
        let target_root = tmp.path().join("wsl-agent-skills");
        fs::create_dir_all(&central_skill).unwrap();
        fs::create_dir_all(&target_root).unwrap();
        fs::write(central_skill.join("SKILL.md"), "# primary").unwrap();
        store.insert_skill(&sample_skill(&central_skill)).unwrap();
        store
            .set_setting(
                "wsl_runtime_environments",
                &serde_json::json!([{
                    "distro_name": "Ubuntu",
                    "library_replica_path": replica_root.to_string_lossy(),
                    "agent_targets": [{
                        "key": "codex",
                        "enabled": true,
                        "skills_dir": target_root.to_string_lossy()
                    }]
                }])
                .to_string(),
            )
            .unwrap();
        store.set_setting("sync_mode", "copy").unwrap();

        sync_single_skill_to_tool(&store, "skill-1", "wsl:Ubuntu:codex").unwrap();

        assert_eq!(
            fs::read_to_string(target_root.join("demo-skill").join("SKILL.md")).unwrap(),
            "# primary"
        );
        assert_eq!(
            fs::read_to_string(replica_root.join("demo-skill").join("SKILL.md")).unwrap(),
            "# primary"
        );
        central_repo::set_test_base_dir_override(None);
    }

    #[test]
    fn direct_wsl_target_sync_respects_runtime_agent_disabled_state() {
        let _guard = central_repo::test_base_dir_lock();
        let tmp = tempdir().unwrap();
        central_repo::set_test_base_dir_override(Some(tmp.path().join("center")));
        let store = SkillStore::new(&tmp.path().join("wsl-disabled-direct-sync.db")).unwrap();
        let central_skill = central_repo::skills_dir().join("demo-skill");
        let replica_root = tmp.path().join(".skills-manager");
        let target_root = tmp.path().join("wsl-agent-skills");
        fs::create_dir_all(&central_skill).unwrap();
        fs::create_dir_all(&target_root).unwrap();
        fs::write(central_skill.join("SKILL.md"), "# primary").unwrap();
        store.insert_skill(&sample_skill(&central_skill)).unwrap();
        store
            .set_setting(
                "wsl_runtime_environments",
                &serde_json::json!([{
                    "distro_name": "Ubuntu",
                    "library_replica_path": replica_root.to_string_lossy(),
                    "agent_targets": [{
                        "key": "codex",
                        "enabled": false,
                        "skills_dir": target_root.to_string_lossy()
                    }]
                }])
                .to_string(),
            )
            .unwrap();
        store.set_setting("sync_mode", "copy").unwrap();

        let err = sync_single_skill_to_tool(&store, "skill-1", "wsl:Ubuntu:codex").unwrap_err();

        assert!(err.to_string().contains("disabled"), "{err}");
        assert!(!target_root.join("demo-skill").exists());
        central_repo::set_test_base_dir_override(None);
    }

    #[test]
    fn scenario_sync_reports_error_after_continuing_non_wsl_targets_when_wsl_replica_fails() {
        let _guard = central_repo::test_base_dir_lock();
        let tmp = tempdir().unwrap();
        central_repo::set_test_base_dir_override(Some(tmp.path().join("center")));
        let store = SkillStore::new(&tmp.path().join("wsl-failure.db")).unwrap();
        let central_skill = central_repo::skills_dir().join("demo-skill");
        let custom_target_root = tmp.path().join("custom-agent-skills");
        let wsl_target_root = tmp.path().join("wsl-agent-skills");
        let blocked_parent = tmp.path().join("blocked-parent");
        fs::create_dir_all(&central_skill).unwrap();
        fs::create_dir_all(&custom_target_root).unwrap();
        fs::create_dir_all(&wsl_target_root).unwrap();
        fs::write(central_skill.join("SKILL.md"), "# primary").unwrap();
        fs::write(&blocked_parent, "not a directory").unwrap();

        store.insert_skill(&sample_skill(&central_skill)).unwrap();
        store.insert_scenario(&sample_scenario()).unwrap();
        store.add_skill_to_scenario("default", "skill-1").unwrap();
        store
            .set_setting(
                "custom_tools",
                &serde_json::json!([{
                    "key": "test_agent",
                    "display_name": "Test Agent",
                    "skills_dir": custom_target_root.to_string_lossy()
                }])
                .to_string(),
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
        store
            .set_setting(
                "wsl_runtime_environments",
                &serde_json::json!([{
                    "distro_name": "Ubuntu",
                    "library_replica_path": blocked_parent.join(".skills-manager").to_string_lossy(),
                    "agent_targets": [{
                        "key": "codex",
                        "enabled": true,
                        "skills_dir": wsl_target_root.to_string_lossy()
                    }]
                }])
                .to_string(),
            )
            .unwrap();
        store.set_setting("sync_mode", "copy").unwrap();

        let err = sync_scenario_skills(&store, "default").unwrap_err();

        assert_eq!(
            fs::read_to_string(custom_target_root.join("demo-skill").join("SKILL.md")).unwrap(),
            "# primary"
        );
        assert!(!wsl_target_root.join("demo-skill").exists());
        assert!(
            err.to_string()
                .contains("Failed to sync WSL Library Replica"),
            "{err}"
        );
        central_repo::set_test_base_dir_override(None);
    }

    #[test]
    fn scenario_preview_identifies_wsl_target_runtime_and_location() {
        let tmp = tempdir().unwrap();
        let store = SkillStore::new(&tmp.path().join("wsl-preview.db")).unwrap();
        let central_skill = tmp.path().join("primary").join("demo-skill");
        let replica_root = tmp.path().join("replica");
        let target_root = tmp.path().join("wsl-agent-skills");
        fs::create_dir_all(&central_skill).unwrap();
        fs::create_dir_all(&target_root).unwrap();
        fs::write(central_skill.join("SKILL.md"), "# primary").unwrap();

        store.insert_skill(&sample_skill(&central_skill)).unwrap();
        store.insert_scenario(&sample_scenario()).unwrap();
        store.add_skill_to_scenario("default", "skill-1").unwrap();
        store
            .set_setting(
                "custom_tool_paths",
                r#"{"codex":"C:\\Users\\me\\codex-skills"}"#,
            )
            .unwrap();
        store
            .set_setting(
                "wsl_runtime_environments",
                &serde_json::json!([{
                    "distro_name": "Ubuntu",
                    "library_replica_path": replica_root.to_string_lossy(),
                    "agent_targets": [{
                        "key": "codex",
                        "enabled": true,
                        "skills_dir": target_root.to_string_lossy()
                    }]
                }])
                .to_string(),
            )
            .unwrap();
        store.set_setting("sync_mode", "copy").unwrap();

        let preview = preview_scenario_sync(&store, "default").unwrap();
        let wsl_target = preview
            .iter()
            .find(|target| target.tool == "wsl:Ubuntu:codex")
            .expect("WSL Codex target should be previewed");

        assert_eq!(wsl_target.runtime_environment, "wsl");
        assert_eq!(wsl_target.wsl_distro_name.as_deref(), Some("Ubuntu"));
        assert!(
            wsl_target
                .target_path
                .contains(&target_root.to_string_lossy().to_string()),
            "{}",
            wsl_target.target_path
        );
        assert!(
            !wsl_target.target_path.contains(r"C:\Users\me\codex-skills"),
            "{}",
            wsl_target.target_path
        );
    }
}

#[cfg(test)]
mod sync_desired_targets_tests {
    use super::*;
    use crate::core::skill_store::{SkillRecord, SkillStore, SkillTargetRecord};
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn copy_fallback_target_with_matching_hash_is_skipped() {
        let _lock = central_repo::test_base_dir_lock();
        let tmp = tempdir().unwrap();
        let base = tmp.path().join("repo");
        central_repo::set_test_base_dir_override(Some(base.clone()));
        fs::create_dir_all(central_repo::skills_dir()).unwrap();
        let store = SkillStore::new(&base.join("test.db")).unwrap();

        let source = central_repo::skills_dir().join("skill-a");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("SKILL.md"), "real source").unwrap();

        let target = tmp.path().join("agent-skills").join("skill-a");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("MARKER.txt"), "do not wipe me").unwrap();

        let skill = SkillRecord {
            id: "skill-a".to_string(),
            name: "skill-a".to_string(),
            description: None,
            source_type: "import".to_string(),
            source_ref: Some(source.to_string_lossy().to_string()),
            source_ref_resolved: None,
            source_subpath: None,
            source_branch: None,
            source_revision: None,
            remote_revision: None,
            central_path: source.to_string_lossy().to_string(),
            content_hash: Some("h1".to_string()),
            enabled: true,
            created_at: 1,
            updated_at: 1,
            status: "ok".to_string(),
            update_status: "local_only".to_string(),
            last_checked_at: None,
            last_check_error: None,
        };
        store.insert_skill(&skill).unwrap();

        store
            .insert_target(&SkillTargetRecord {
                id: "target-1".to_string(),
                skill_id: "skill-a".to_string(),
                tool: "claude-code".to_string(),
                target_path: target.to_string_lossy().to_string(),
                mode: "copy".to_string(),
                status: "ok".to_string(),
                synced_at: Some(1),
                last_error: None,
                source_hash: Some("h1".to_string()),
            })
            .unwrap();

        let desired = vec![ScenarioSyncTarget {
            skill_id: "skill-a".to_string(),
            skill_name: "skill-a".to_string(),
            tool: "claude-code".to_string(),
            source: source.clone(),
            target: target.clone(),
            mode: sync_engine::SyncMode::Symlink,
            source_hash: Some("h1".to_string()),
        }];

        sync_desired_targets(&store, &desired).unwrap();

        assert!(
            target.join("MARKER.txt").exists(),
            "target dir was wiped, skip did not fire"
        );
        assert!(
            !target.join("SKILL.md").exists(),
            "SKILL.md appeared, sync ran instead of skipping"
        );

        central_repo::set_test_base_dir_override(None);
    }

    #[test]
    fn deleted_target_with_matching_hash_forces_resync() {
        let _lock = central_repo::test_base_dir_lock();
        let tmp = tempdir().unwrap();
        let base = tmp.path().join("repo");
        central_repo::set_test_base_dir_override(Some(base.clone()));
        fs::create_dir_all(central_repo::skills_dir()).unwrap();
        let store = SkillStore::new(&base.join("test.db")).unwrap();

        let source = central_repo::skills_dir().join("skill-b");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("SKILL.md"), "real source").unwrap();
        let target = tmp.path().join("agent-skills").join("skill-b");

        let skill = SkillRecord {
            id: "skill-b".to_string(),
            name: "skill-b".to_string(),
            description: None,
            source_type: "import".to_string(),
            source_ref: Some(source.to_string_lossy().to_string()),
            source_ref_resolved: None,
            source_subpath: None,
            source_branch: None,
            source_revision: None,
            remote_revision: None,
            central_path: source.to_string_lossy().to_string(),
            content_hash: Some("h1".to_string()),
            enabled: true,
            created_at: 1,
            updated_at: 1,
            status: "ok".to_string(),
            update_status: "local_only".to_string(),
            last_checked_at: None,
            last_check_error: None,
        };
        store.insert_skill(&skill).unwrap();

        store
            .insert_target(&SkillTargetRecord {
                id: "target-2".to_string(),
                skill_id: "skill-b".to_string(),
                tool: "claude-code".to_string(),
                target_path: target.to_string_lossy().to_string(),
                mode: "copy".to_string(),
                status: "ok".to_string(),
                synced_at: Some(1),
                last_error: None,
                source_hash: Some("h1".to_string()),
            })
            .unwrap();

        let desired = vec![ScenarioSyncTarget {
            skill_id: "skill-b".to_string(),
            skill_name: "skill-b".to_string(),
            tool: "claude-code".to_string(),
            source: source.clone(),
            target: target.clone(),
            mode: sync_engine::SyncMode::Copy,
            source_hash: Some("h1".to_string()),
        }];

        sync_desired_targets(&store, &desired).unwrap();

        assert!(
            target.join("SKILL.md").exists(),
            "missing target was not re-synced"
        );

        central_repo::set_test_base_dir_override(None);
    }
}

#[cfg(test)]
mod skip_check_mode_tests {
    use super::skip_check_mode;
    use super::sync_engine::SyncMode;

    #[test]
    fn matching_modes_are_compatible() {
        assert!(matches!(
            skip_check_mode("symlink", SyncMode::Symlink),
            Some(SyncMode::Symlink)
        ));
        assert!(matches!(
            skip_check_mode("symlink", SyncMode::WslSymlink),
            Some(SyncMode::WslSymlink)
        ));
        assert!(matches!(
            skip_check_mode("copy", SyncMode::Copy),
            Some(SyncMode::Copy)
        ));
    }

    #[test]
    fn copy_existing_with_symlink_desired_treated_as_copy() {
        assert!(matches!(
            skip_check_mode("copy", SyncMode::Symlink),
            Some(SyncMode::Copy)
        ));
    }

    #[test]
    fn symlink_existing_with_copy_desired_is_incompatible() {
        assert!(skip_check_mode("symlink", SyncMode::Copy).is_none());
    }

    #[test]
    fn unknown_existing_mode_is_incompatible() {
        assert!(skip_check_mode("garbage", SyncMode::Symlink).is_none());
        assert!(skip_check_mode("", SyncMode::Copy).is_none());
    }
}
