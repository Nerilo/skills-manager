use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

use super::skill_store::SkillStore;

const SETTINGS_KEY: &str = "wsl_runtime_environments";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WslRuntimeEnvironment {
    pub distro_name: String,
    pub library_replica_path: String,
    #[serde(default)]
    pub agent_targets: Vec<WslAgentTargetConfig>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WslRuntimeEnvironmentStatus {
    pub distro_name: String,
    pub library_replica_path: String,
    pub reachable: bool,
    pub agent_targets: Vec<WslAgentTargetConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WslAgentTargetConfig {
    pub key: String,
    #[serde(default = "default_agent_target_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub skills_dir: Option<String>,
}

fn default_agent_target_enabled() -> bool {
    true
}

pub fn normalize_library_replica_path(distro_name: &str, input: &str) -> Result<String> {
    let distro_name = normalize_distro_name(distro_name)?;
    let raw = input.trim();
    if raw.is_empty() {
        bail!("Library Replica path is required");
    }

    if raw.starts_with(r"\\") || raw.starts_with("//") {
        return normalize_unc_path(&distro_name, raw);
    }

    if let Some((path_distro, linux_path)) = raw.split_once(':') {
        let path_distro = normalize_distro_name(path_distro)?;
        ensure_same_distro(&distro_name, &path_distro)?;
        if !linux_path.starts_with('/') {
            bail!(
                "Library Replica path must use Distro:/home/user/... or \\\\wsl.localhost\\Distro\\..."
            );
        }
        let replica = linux_path.trim_start_matches('/').replace('/', r"\");
        if replica.is_empty() {
            bail!("Library Replica path must include a directory inside the distro");
        }
        return Ok(format!(r"\\wsl.localhost\{distro_name}\{replica}"));
    }

    bail!("Library Replica path must use Distro:/home/user/... or \\\\wsl.localhost\\Distro\\...")
}

pub fn add_runtime_environment(
    store: &SkillStore,
    distro_name: &str,
    library_replica_path: &str,
) -> Result<WslRuntimeEnvironmentStatus> {
    let distro_name = normalize_distro_name(distro_name)?;
    let library_replica_path = normalize_library_replica_path(&distro_name, library_replica_path)?;
    let mut environments = load_environments(store)?;
    if let Some(existing) = environments
        .iter_mut()
        .find(|env| env.distro_name.eq_ignore_ascii_case(&distro_name))
    {
        existing.distro_name = distro_name.clone();
        existing.library_replica_path = library_replica_path.clone();
    } else {
        environments.push(WslRuntimeEnvironment {
            distro_name: distro_name.clone(),
            library_replica_path: library_replica_path.clone(),
            agent_targets: vec![],
        });
    }
    save_environments(store, &environments)?;
    Ok(to_status(WslRuntimeEnvironment {
        distro_name,
        library_replica_path,
        agent_targets: vec![],
    }))
}

pub fn list_runtime_environments(store: &SkillStore) -> Result<Vec<WslRuntimeEnvironmentStatus>> {
    Ok(load_environments(store)?
        .into_iter()
        .map(to_status)
        .collect())
}

pub fn get_runtime_environment(
    store: &SkillStore,
    distro_name: &str,
) -> Result<WslRuntimeEnvironment> {
    let distro_name = normalize_distro_name(distro_name)?;
    load_environments(store)?
        .into_iter()
        .find(|env| env.distro_name.eq_ignore_ascii_case(&distro_name))
        .ok_or_else(|| {
            anyhow::anyhow!("WSL runtime environment \"{distro_name}\" is not configured")
        })
}

pub fn remove_runtime_environment(store: &SkillStore, distro_name: &str) -> Result<()> {
    let distro_name = normalize_distro_name(distro_name)?;
    let mut environments = load_environments(store)?;
    environments.retain(|env| !env.distro_name.eq_ignore_ascii_case(&distro_name));
    save_environments(store, &environments)
}

pub fn wsl_tool_key(distro_name: &str, agent_key: &str) -> String {
    format!("wsl:{distro_name}:{agent_key}")
}

pub fn parse_wsl_tool_key(key: &str) -> Option<(&str, &str)> {
    let rest = key.strip_prefix("wsl:")?;
    rest.split_once(':')
}

pub fn resolve_agent_target_path(
    runtime: &WslRuntimeEnvironment,
    relative_skills_dir: &str,
) -> Result<String> {
    let relative = relative_skills_dir
        .trim()
        .trim_start_matches(['/', '\\'])
        .replace('/', r"\");
    if relative.is_empty() {
        bail!("Agent Target default path requires a relative skills directory");
    }
    let base = default_agent_target_base_from_library_replica(runtime)?;
    Ok(format!(r"{base}\{relative}"))
}

pub fn configured_agent_target(
    runtime: &WslRuntimeEnvironment,
    agent_key: &str,
) -> Option<WslAgentTargetConfig> {
    runtime
        .agent_targets
        .iter()
        .find(|target| target.key == agent_key)
        .cloned()
}

pub fn agent_target_has_path_override(
    store: &SkillStore,
    distro_name: &str,
    agent_key: &str,
) -> bool {
    get_runtime_environment(store, distro_name)
        .ok()
        .and_then(|runtime| configured_agent_target(&runtime, agent_key))
        .and_then(|target| target.skills_dir)
        .is_some()
}

pub fn agent_target_enabled(store: &SkillStore, distro_name: &str, agent_key: &str) -> bool {
    get_runtime_environment(store, distro_name)
        .ok()
        .and_then(|runtime| configured_agent_target(&runtime, agent_key))
        .map(|target| target.enabled)
        .unwrap_or(false)
}

pub fn set_agent_target_enabled(
    store: &SkillStore,
    distro_name: &str,
    agent_key: &str,
    enabled: bool,
) -> Result<()> {
    update_agent_target(store, distro_name, agent_key, |target| {
        target.enabled = enabled;
        Ok(())
    })
}

pub fn set_agent_target_path(
    store: &SkillStore,
    distro_name: &str,
    agent_key: &str,
    path: &str,
) -> Result<()> {
    let distro_name = normalize_distro_name(distro_name)?;
    let path = normalize_agent_target_path(&distro_name, path)?;
    update_agent_target(store, &distro_name, agent_key, |target| {
        target.skills_dir = Some(path);
        Ok(())
    })
}

pub fn reset_agent_target_path(
    store: &SkillStore,
    distro_name: &str,
    agent_key: &str,
) -> Result<()> {
    update_agent_target(store, distro_name, agent_key, |target| {
        target.skills_dir = None;
        Ok(())
    })
}

fn update_agent_target<F>(
    store: &SkillStore,
    distro_name: &str,
    agent_key: &str,
    update: F,
) -> Result<()>
where
    F: FnOnce(&mut WslAgentTargetConfig) -> Result<()>,
{
    let distro_name = normalize_distro_name(distro_name)?;
    let agent_key = agent_key.trim();
    if agent_key.is_empty() {
        bail!("Agent Target key is required");
    }

    let mut environments = load_environments(store)?;
    let runtime = environments
        .iter_mut()
        .find(|env| env.distro_name.eq_ignore_ascii_case(&distro_name))
        .ok_or_else(|| {
            anyhow::anyhow!("WSL runtime environment \"{distro_name}\" is not configured")
        })?;
    let index = runtime
        .agent_targets
        .iter()
        .position(|target| target.key == agent_key);
    let target = if let Some(index) = index {
        &mut runtime.agent_targets[index]
    } else {
        runtime.agent_targets.push(WslAgentTargetConfig {
            key: agent_key.to_string(),
            enabled: true,
            skills_dir: None,
        });
        runtime
            .agent_targets
            .last_mut()
            .expect("target was inserted")
    };
    update(target)?;
    save_environments(store, &environments)
}

fn normalize_distro_name(input: &str) -> Result<String> {
    let name = input.trim();
    if name.is_empty() {
        bail!("Distro name is required");
    }
    if name
        .chars()
        .any(|ch| matches!(ch, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
    {
        bail!("Distro name cannot contain path separators or Windows path reserved characters");
    }
    Ok(name.to_string())
}

fn normalize_unc_path(distro_name: &str, raw: &str) -> Result<String> {
    let path = raw.replace('/', r"\");
    let prefix = r"\\wsl.localhost\";
    let Some(rest) = path.strip_prefix(prefix) else {
        bail!("UNC Library Replica path must start with \\\\wsl.localhost\\Distro\\");
    };
    let Some((path_distro, replica)) = rest.split_once('\\') else {
        bail!("Library Replica path must include a directory inside the distro");
    };
    let path_distro = normalize_distro_name(path_distro)?;
    ensure_same_distro(distro_name, &path_distro)?;
    let replica = replica.trim_matches('\\');
    if replica.is_empty() {
        bail!("Library Replica path must include a directory inside the distro");
    }
    Ok(format!(r"\\wsl.localhost\{distro_name}\{replica}"))
}

fn normalize_agent_target_path(distro_name: &str, raw: &str) -> Result<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        bail!("Agent Target path is required");
    }
    if raw.starts_with(r"\\") || raw.starts_with("//") || raw.contains(':') {
        return normalize_library_replica_path(distro_name, raw);
    }
    if raw.starts_with('/') {
        let replica = raw.trim_start_matches('/').replace('/', r"\");
        if replica.is_empty() {
            bail!("Agent Target path must include a directory inside the distro");
        }
        return Ok(format!(r"\\wsl.localhost\{distro_name}\{replica}"));
    }
    bail!("Agent Target path must use /home/user/..., Distro:/home/user/..., or \\\\wsl.localhost\\Distro\\...")
}

fn default_agent_target_base_from_library_replica(
    runtime: &WslRuntimeEnvironment,
) -> Result<String> {
    let normalized =
        normalize_library_replica_path(&runtime.distro_name, &runtime.library_replica_path)?;
    let prefix = format!(r"\\wsl.localhost\{}\", runtime.distro_name);
    let Some(rest) = normalized.strip_prefix(&prefix) else {
        bail!("Library Replica path must be inside the configured distro");
    };
    let mut parts = rest.split('\\');
    let first = parts.next().unwrap_or_default();
    let second = parts.next().unwrap_or_default();
    if first == "home" && !second.is_empty() {
        return Ok(format!(r"{prefix}home\{second}"));
    }
    let Some((parent, _replica_name)) = normalized.rsplit_once('\\') else {
        bail!(
            "Library Replica path must include a parent directory to resolve default Agent Targets"
        );
    };
    Ok(parent.to_string())
}

fn ensure_same_distro(expected: &str, actual: &str) -> Result<()> {
    if !expected.eq_ignore_ascii_case(actual) {
        bail!("Library Replica path distro \"{actual}\" does not match configured distro \"{expected}\"");
    }
    Ok(())
}

fn load_environments(store: &SkillStore) -> Result<Vec<WslRuntimeEnvironment>> {
    Ok(store
        .get_setting(SETTINGS_KEY)?
        .and_then(|value| serde_json::from_str::<Vec<WslRuntimeEnvironment>>(&value).ok())
        .unwrap_or_default())
}

fn save_environments(store: &SkillStore, environments: &[WslRuntimeEnvironment]) -> Result<()> {
    let json = serde_json::to_string(environments)?;
    store.set_setting(SETTINGS_KEY, &json)
}

fn to_status(environment: WslRuntimeEnvironment) -> WslRuntimeEnvironmentStatus {
    let reachable = Path::new(&environment.library_replica_path).exists();
    WslRuntimeEnvironmentStatus {
        distro_name: environment.distro_name,
        library_replica_path: environment.library_replica_path,
        reachable,
        agent_targets: environment.agent_targets,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn normalizes_unc_library_replica_path_for_display() {
        let normalized = normalize_library_replica_path(
            "Ubuntu-24.04",
            r"\\wsl.localhost\Ubuntu-24.04\home\me\.codex\skills",
        )
        .unwrap();

        assert_eq!(
            normalized,
            r"\\wsl.localhost\Ubuntu-24.04\home\me\.codex\skills"
        );
    }

    #[test]
    fn normalizes_distro_prefixed_linux_path_to_unc_display_path() {
        let normalized =
            normalize_library_replica_path("Ubuntu", "Ubuntu:/home/me/.codex/skills").unwrap();

        assert_eq!(normalized, r"\\wsl.localhost\Ubuntu\home\me\.codex\skills");
    }

    #[test]
    fn rejects_distro_prefixed_path_for_a_different_distro() {
        let err =
            normalize_library_replica_path("Ubuntu", "Debian:/home/me/.codex/skills").unwrap_err();

        assert!(err.to_string().contains("does not match"));
    }

    #[test]
    fn rejects_paths_without_a_replica_directory() {
        let err = normalize_library_replica_path("Ubuntu", "Ubuntu:/").unwrap_err();

        assert!(err.to_string().contains("Library Replica path"));
    }

    #[test]
    fn persists_runtime_environment_with_normalized_path() {
        let tmp = tempdir().unwrap();
        let store = SkillStore::new(&tmp.path().join("wsl.db")).unwrap();

        let saved = add_runtime_environment(&store, "Ubuntu", "Ubuntu:/home/me/.codex/skills")
            .expect("runtime should save");
        let listed = list_runtime_environments(&store).expect("runtime should list");

        assert_eq!(saved.distro_name, "Ubuntu");
        assert_eq!(
            saved.library_replica_path,
            r"\\wsl.localhost\Ubuntu\home\me\.codex\skills"
        );
        assert!(saved.agent_targets.is_empty());
        assert_eq!(listed, vec![saved]);
    }

    #[test]
    fn persists_runtime_agent_target_overrides_without_windows_settings() {
        let tmp = tempdir().unwrap();
        let store = SkillStore::new(&tmp.path().join("wsl-targets.db")).unwrap();
        add_runtime_environment(&store, "Ubuntu", "Ubuntu:/home/me/skills").unwrap();
        store
            .set_setting("disabled_tools", r#"["codex"]"#)
            .expect("Windows disabled tools should save");

        set_agent_target_enabled(&store, "Ubuntu", "codex", false).unwrap();
        set_agent_target_path(&store, "Ubuntu", "codex", "/home/me/wsl-codex").unwrap();

        let runtime = get_runtime_environment(&store, "Ubuntu").unwrap();
        assert_eq!(
            runtime.agent_targets,
            vec![WslAgentTargetConfig {
                key: "codex".to_string(),
                enabled: false,
                skills_dir: Some(r"\\wsl.localhost\Ubuntu\home\me\wsl-codex".to_string()),
            }]
        );
        assert_eq!(
            store.get_setting("disabled_tools").unwrap().unwrap(),
            r#"["codex"]"#
        );
        assert!(store.get_setting("custom_tool_paths").unwrap().is_none());
    }

    #[test]
    fn resolves_default_agent_target_under_home_when_replica_is_under_home() {
        let runtime = WslRuntimeEnvironment {
            distro_name: "Ubuntu".to_string(),
            library_replica_path: r"\\wsl.localhost\Ubuntu\home\me\.skills-manager".to_string(),
            agent_targets: vec![],
        };

        let resolved = resolve_agent_target_path(&runtime, ".agents/skills").unwrap();

        assert_eq!(resolved, r"\\wsl.localhost\Ubuntu\home\me\.agents\skills");
    }

    #[test]
    fn resolves_default_agent_target_next_to_replica_when_replica_is_outside_home() {
        let runtime = WslRuntimeEnvironment {
            distro_name: "Ubuntu".to_string(),
            library_replica_path: r"\\wsl.localhost\Ubuntu\mnt\d\skills".to_string(),
            agent_targets: vec![],
        };

        let resolved = resolve_agent_target_path(&runtime, ".agents/skills").unwrap();

        assert_eq!(resolved, r"\\wsl.localhost\Ubuntu\mnt\d\.agents\skills");
    }

    #[test]
    fn rejects_empty_default_agent_target_relative_path() {
        let runtime = WslRuntimeEnvironment {
            distro_name: "Ubuntu".to_string(),
            library_replica_path: r"\\wsl.localhost\Ubuntu\home\me\skills".to_string(),
            agent_targets: vec![],
        };

        let err = resolve_agent_target_path(&runtime, "").unwrap_err();

        assert!(err.to_string().contains("relative skills directory"));
    }

    #[test]
    fn unconfigured_wsl_agent_targets_are_disabled_by_default() {
        let tmp = tempdir().unwrap();
        let store = SkillStore::new(&tmp.path().join("wsl-disabled-default.db")).unwrap();
        add_runtime_environment(&store, "Ubuntu", "Ubuntu:/home/me/skills").unwrap();

        assert!(!agent_target_enabled(&store, "Ubuntu", "codex"));
    }
}
