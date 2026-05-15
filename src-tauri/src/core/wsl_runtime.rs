use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

use super::skill_store::SkillStore;

const SETTINGS_KEY: &str = "wsl_runtime_environments";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WslRuntimeEnvironment {
    pub distro_name: String,
    pub library_replica_path: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WslRuntimeEnvironmentStatus {
    pub distro_name: String,
    pub library_replica_path: String,
    pub reachable: bool,
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
        });
    }
    save_environments(store, &environments)?;
    Ok(to_status(WslRuntimeEnvironment {
        distro_name,
        library_replica_path,
    }))
}

pub fn list_runtime_environments(store: &SkillStore) -> Result<Vec<WslRuntimeEnvironmentStatus>> {
    Ok(load_environments(store)?.into_iter().map(to_status).collect())
}

pub fn remove_runtime_environment(store: &SkillStore, distro_name: &str) -> Result<()> {
    let distro_name = normalize_distro_name(distro_name)?;
    let mut environments = load_environments(store)?;
    environments.retain(|env| !env.distro_name.eq_ignore_ascii_case(&distro_name));
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
        assert_eq!(listed, vec![saved]);
    }
}
