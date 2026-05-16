use std::path::PathBuf;

use super::{
    error::AppError,
    skill_store::{SkillRecord, SkillStore},
    sync_engine, wsl_runtime,
};

pub fn source_for_target(
    store: &SkillStore,
    skill: &SkillRecord,
    tool_key: &str,
) -> Result<PathBuf, AppError> {
    let central_source = PathBuf::from(&skill.central_path);
    let Some((distro_name, _agent_key)) = wsl_runtime::parse_wsl_tool_key(tool_key) else {
        return Ok(central_source);
    };

    let runtime = wsl_runtime::get_runtime_environment(store, distro_name)
        .map_err(|err| AppError::invalid_input(err.to_string()))?;
    Ok(PathBuf::from(runtime.library_replica_path)
        .join(sync_engine::target_dir_name(&central_source, &skill.name)))
}
