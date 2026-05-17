use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Refuse to copy when `dst` would land inside `src` (or equal `src`).
/// Otherwise the recursive copy walks into the freshly-created `dst` and
/// produces unbounded `<dst>/<dst>/<dst>/...` nesting (issue #61).
pub(crate) fn ensure_dst_not_inside_src(src: &Path, dst: &Path) -> Result<()> {
    let Ok(src_canon) = src.canonicalize() else {
        return Ok(());
    };
    let dst_canon: Option<PathBuf> = dst.canonicalize().ok().or_else(|| {
        let parent = dst.parent()?.canonicalize().ok()?;
        let name = dst.file_name()?;
        Some(parent.join(name))
    });
    if let Some(dst_canon) = dst_canon {
        if dst_canon.starts_with(&src_canon) {
            anyhow::bail!(
                "Destination {:?} is inside source {:?}; refusing to copy to avoid infinite recursion",
                dst,
                src
            );
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub enum SyncMode {
    Symlink,
    WslSymlink,
    Copy,
}

impl SyncMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            SyncMode::Symlink => "symlink",
            SyncMode::WslSymlink => "symlink",
            SyncMode::Copy => "copy",
        }
    }
}

pub fn sync_mode_for_tool(tool_key: &str, configured_mode: Option<&str>) -> SyncMode {
    match configured_mode {
        Some("copy") => SyncMode::Copy,
        Some("symlink") if tool_key.starts_with("wsl:") => SyncMode::WslSymlink,
        Some("symlink") => SyncMode::Symlink,
        _ if tool_key.starts_with("wsl:") => SyncMode::WslSymlink,
        _ => SyncMode::Symlink,
    }
}

pub fn target_dir_name(central_path: &Path, skill_name: &str) -> String {
    central_path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| skill_name.to_string())
}

pub fn sync_skill(source: &Path, target: &Path, mode: SyncMode) -> Result<SyncMode> {
    if is_target_current(source, target, mode) {
        return Ok(mode);
    }

    let wsl_link = if matches!(mode, SyncMode::WslSymlink) {
        Some(prepare_wsl_symlink(source, target)?)
    } else {
        None
    };

    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create parent dir {:?}", parent))?;
    }

    ensure_dst_not_inside_src(source, target)?;

    // Remove existing target
    if matches!(mode, SyncMode::WslSymlink) {
        remove_target(target)
            .with_context(|| format!("Failed to remove existing target {:?}", target))?;
    } else {
        remove_target(target).ok();
    }

    match mode {
        SyncMode::Symlink => {
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(source, target).with_context(|| {
                    format!("Failed to create symlink {:?} -> {:?}", target, source)
                })?;
                Ok(SyncMode::Symlink)
            }
            #[cfg(windows)]
            {
                match std::os::windows::fs::symlink_dir(source, target) {
                    Ok(()) => Ok(SyncMode::Symlink),
                    Err(err) => {
                        // Typical causes: missing SeCreateSymbolicLinkPrivilege,
                        // Developer Mode disabled, or non-NTFS target volume.
                        log::warn!(
                            "symlink_dir {:?} -> {:?} failed, falling back to copy: {err}",
                            target,
                            source
                        );
                        copy_dir_recursive(source, target)?;
                        Ok(SyncMode::Copy)
                    }
                }
            }
            #[cfg(all(not(unix), not(windows)))]
            {
                copy_dir_recursive(source, target)?;
                Ok(SyncMode::Copy)
            }
        }
        SyncMode::WslSymlink => {
            let wsl_link = wsl_link.expect("WSL symlink command should be prepared");
            create_wsl_symlink(&wsl_link)?;
            Ok(SyncMode::WslSymlink)
        }
        SyncMode::Copy => {
            copy_dir_recursive(source, target)?;
            Ok(SyncMode::Copy)
        }
    }
}

#[derive(Debug)]
struct WslSymlinkCommand {
    distro_name: String,
    source: String,
    target: String,
    target_parent: String,
}

fn prepare_wsl_symlink(source: &Path, target: &Path) -> Result<WslSymlinkCommand> {
    let source_raw = source.to_string_lossy();
    let target_raw = target.to_string_lossy();
    let source_distro = wsl_distro_from_unc(&source_raw)?;
    let target_distro = wsl_distro_from_unc(&target_raw)?;
    if !source_distro.eq_ignore_ascii_case(&target_distro) {
        anyhow::bail!(
            "WSL symlink mode requires source and target to be inside the same WSL distro"
        );
    }
    let source = wsl_linux_path_from_unc(&source_distro, &source_raw)?;
    let target = wsl_linux_path_from_unc(&source_distro, &target_raw)?;
    let target_parent = target
        .rsplit_once('/')
        .map(|(parent, _)| {
            if parent.is_empty() {
                "/".to_string()
            } else {
                parent.to_string()
            }
        })
        .unwrap_or_else(|| "/".to_string());

    Ok(WslSymlinkCommand {
        distro_name: source_distro,
        source,
        target,
        target_parent,
    })
}

fn create_wsl_symlink(link: &WslSymlinkCommand) -> Result<()> {
    run_wsl_fixed(
        &link.distro_name,
        &["mkdir", "-p", link.target_parent.as_str()],
        "create WSL target parent directory",
    )?;
    run_wsl_fixed(
        &link.distro_name,
        &wsl_symlink_args(link),
        "create WSL symlink",
    )
}

fn wsl_symlink_args(link: &WslSymlinkCommand) -> Vec<&str> {
    vec!["ln", "-sT", link.source.as_str(), link.target.as_str()]
}

fn run_wsl_fixed(distro_name: &str, command_args: &[&str], action: &str) -> Result<()> {
    let output = Command::new("wsl.exe")
        .arg("-d")
        .arg(distro_name)
        .arg("--")
        .args(command_args)
        .output()
        .with_context(|| format!("Failed to start wsl.exe to {action}"))?;
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    anyhow::bail!(
        "Failed to {action} in WSL distro {distro_name}: {}{}",
        stderr.trim(),
        stdout.trim()
    )
}

fn wsl_distro_from_unc(path: &str) -> Result<String> {
    let normalized = path.replace('/', r"\");
    let prefix = r"\\wsl.localhost\";
    let Some(rest) = normalized.strip_prefix(prefix) else {
        anyhow::bail!(
            "WSL symlink mode requires \\\\wsl.localhost\\Distro\\... paths for both source and target"
        );
    };
    let Some((distro_name, _)) = rest.split_once('\\') else {
        anyhow::bail!("WSL symlink mode requires a path inside a configured WSL distro");
    };
    if distro_name.trim().is_empty() {
        anyhow::bail!("WSL symlink mode requires a WSL distro name");
    }
    Ok(distro_name.to_string())
}

fn wsl_linux_path_from_unc(distro_name: &str, path: &str) -> Result<String> {
    let normalized = path.replace('/', r"\");
    let prefix = r"\\wsl.localhost\";
    let Some(rest) = normalized.strip_prefix(prefix) else {
        anyhow::bail!(
            "WSL symlink mode requires \\\\wsl.localhost\\Distro\\... paths for both source and target"
        );
    };
    let Some((path_distro, linux_path)) = rest.split_once('\\') else {
        anyhow::bail!("WSL symlink mode requires a path inside a configured WSL distro");
    };
    if !path_distro.eq_ignore_ascii_case(distro_name) {
        anyhow::bail!("WSL symlink mode requires source and target paths in the same WSL distro");
    }
    let linux_path = linux_path.trim_matches('\\').replace('\\', "/");
    if linux_path.is_empty() {
        anyhow::bail!("WSL symlink mode requires a path inside the distro");
    }
    Ok(format!("/{linux_path}"))
}

pub fn sync_library_replica(primary_library: &Path, library_replica: &Path) -> Result<()> {
    ensure_library_replica_target(library_replica)?;

    if let Some(parent) = library_replica.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create parent dir {:?}", parent))?;
    }

    ensure_dst_not_inside_src(primary_library, library_replica)?;
    remove_target(library_replica)
        .with_context(|| format!("Failed to remove existing target {:?}", library_replica))?;
    copy_dir_recursive(primary_library, library_replica)
}

fn ensure_library_replica_target(library_replica: &Path) -> Result<()> {
    let name = library_replica
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if name != ".skills-manager" {
        anyhow::bail!("Library Replica target must be named .skills-manager");
    }
    Ok(())
}

pub fn is_target_current(source: &Path, target: &Path, mode: SyncMode) -> bool {
    match mode {
        SyncMode::Symlink | SyncMode::WslSymlink => symlink_points_to(target, source),
        // Copy mode intentionally refreshes the target because there is no cheap
        // metadata-backed freshness check for arbitrary skill directory contents.
        SyncMode::Copy => false,
    }
}

fn symlink_points_to(target: &Path, source: &Path) -> bool {
    let Ok(metadata) = std::fs::symlink_metadata(target) else {
        return false;
    };
    if !metadata.file_type().is_symlink() {
        return false;
    }

    let Ok(link_target) = std::fs::read_link(target) else {
        return false;
    };
    let resolved_link_target = if link_target.is_absolute() {
        link_target
    } else {
        target
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(link_target)
    };

    if resolved_link_target == source {
        return true;
    }

    match (resolved_link_target.canonicalize(), source.canonicalize()) {
        (Ok(link), Ok(src)) => link == src,
        _ => false,
    }
}

/// Check whether a path is a WSL UNC path (\\wsl.localhost\Distro\...).
pub(crate) fn is_wsl_unc_path(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.starts_with(r"\\wsl.localhost\") || s.starts_with("//wsl.localhost/")
}

/// Remove a target inside a WSL distro using `wsl.exe rm -rf`.
/// `rm -rf` on a non-existent path succeeds with exit code 0.
fn remove_wsl_target(target: &Path) -> Result<()> {
    let raw = target.to_string_lossy();
    let normalized = raw.replace('/', r"\");
    let distro = wsl_distro_from_unc(&normalized)?;
    let linux_path = wsl_linux_path_from_unc(&distro, &normalized)?;

    run_wsl_fixed(&distro, &["rm", "-rf", &linux_path], "remove WSL target")
}

pub fn remove_target(target: &Path) -> Result<()> {
    if is_wsl_unc_path(target) {
        return remove_wsl_target(target);
    }
    let metadata = match std::fs::symlink_metadata(target) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };

    if metadata.file_type().is_symlink() {
        #[cfg(windows)]
        {
            if target.is_dir() {
                std::fs::remove_dir(target)?;
            } else {
                std::fs::remove_file(target)?;
            }
        }
        #[cfg(not(windows))]
        {
            std::fs::remove_file(target)?;
        }
    } else if metadata.is_dir() {
        std::fs::remove_dir_all(target)?;
    } else {
        std::fs::remove_file(target)?;
    }
    Ok(())
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let dest_path = dst.join(entry.file_name());
        if ft.is_dir() {
            let name = entry.file_name();
            if name == ".git" || name == ".skills-manager" {
                continue;
            }
            copy_dir_recursive(&entry.path(), &dest_path)?;
        } else {
            std::fs::copy(entry.path(), &dest_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    // ── sync_mode_for_tool ──

    #[test]
    fn sync_mode_defaults_to_symlink() {
        assert!(matches!(
            sync_mode_for_tool("claude-code", None),
            SyncMode::Symlink
        ));
    }

    #[test]
    fn sync_mode_cursor_defaults_to_symlink() {
        assert!(matches!(
            sync_mode_for_tool("cursor", None),
            SyncMode::Symlink
        ));
    }

    #[test]
    fn sync_mode_explicit_copy_overrides_default() {
        assert!(matches!(
            sync_mode_for_tool("claude-code", Some("copy")),
            SyncMode::Copy
        ));
    }

    #[test]
    fn sync_mode_explicit_symlink_overrides_cursor_default() {
        assert!(matches!(
            sync_mode_for_tool("cursor", Some("symlink")),
            SyncMode::Symlink
        ));
    }

    #[test]
    fn sync_mode_unknown_config_falls_back_to_tool_default() {
        assert!(matches!(
            sync_mode_for_tool("cursor", Some("invalid")),
            SyncMode::Symlink
        ));
        assert!(matches!(
            sync_mode_for_tool("claude-code", Some("invalid")),
            SyncMode::Symlink
        ));
    }

    #[test]
    fn sync_mode_uses_wsl_symlink_for_configured_wsl_targets() {
        assert!(matches!(
            sync_mode_for_tool("wsl:Ubuntu:codex", Some("symlink")),
            SyncMode::WslSymlink
        ));
    }

    #[test]
    fn sync_mode_keeps_copy_for_wsl_targets_when_configured() {
        assert!(matches!(
            sync_mode_for_tool("wsl:Ubuntu:codex", Some("copy")),
            SyncMode::Copy
        ));
    }

    #[test]
    fn sync_mode_as_str() {
        assert_eq!(SyncMode::Symlink.as_str(), "symlink");
        assert_eq!(SyncMode::WslSymlink.as_str(), "symlink");
        assert_eq!(SyncMode::Copy.as_str(), "copy");
    }

    #[test]
    fn wsl_linux_path_from_unc_accepts_same_distro_unc_path() {
        assert_eq!(
            wsl_linux_path_from_unc("Ubuntu", r"\\wsl.localhost\Ubuntu\home\me\.skills-manager")
                .unwrap(),
            "/home/me/.skills-manager"
        );
    }

    #[test]
    fn wsl_linux_path_from_unc_rejects_other_distros() {
        let err = wsl_linux_path_from_unc("Ubuntu", r"\\wsl.localhost\Debian\home\me\skills")
            .unwrap_err();

        assert!(err.to_string().contains("same WSL distro"), "{err}");
    }

    #[test]
    fn sync_skill_wsl_symlink_rejects_unsupported_paths_without_removing_target() {
        let tmp = tempdir().unwrap();
        let src = tmp.path().join("source");
        let tgt = tmp.path().join("target");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&tgt).unwrap();
        fs::write(src.join("SKILL.md"), "# source").unwrap();
        fs::write(tgt.join("keep.txt"), "keep").unwrap();

        let err = sync_skill(&src, &tgt, SyncMode::WslSymlink).unwrap_err();

        assert!(err.to_string().contains("WSL symlink mode"), "{err}");
        assert_eq!(fs::read_to_string(tgt.join("keep.txt")).unwrap(), "keep");
    }

    #[test]
    fn wsl_symlink_command_treats_target_as_link_path_not_directory() {
        let link = WslSymlinkCommand {
            distro_name: "Ubuntu".to_string(),
            source: "/home/me/.skills-manager/demo".to_string(),
            target: "/home/me/.agents/skills/demo".to_string(),
            target_parent: "/home/me/.agents/skills".to_string(),
        };

        assert_eq!(
            wsl_symlink_args(&link),
            vec![
                "ln",
                "-sT",
                "/home/me/.skills-manager/demo",
                "/home/me/.agents/skills/demo"
            ]
        );
    }

    #[test]
    fn target_dir_name_uses_central_directory_name() {
        let central_path = Path::new("/central/skill123-2");

        assert_eq!(target_dir_name(central_path, "skill123"), "skill123-2");
    }

    #[test]
    fn target_dir_name_falls_back_to_skill_name() {
        assert_eq!(target_dir_name(Path::new(""), "skill123"), "skill123");
    }

    // ── sync_skill (filesystem) ──

    #[test]
    fn sync_skill_copy_creates_directory_with_files() {
        let tmp = tempdir().unwrap();
        let src = tmp.path().join("source");
        let tgt = tmp.path().join("target");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("SKILL.md"), "# hello").unwrap();

        let mode = sync_skill(&src, &tgt, SyncMode::Copy).unwrap();
        assert!(matches!(mode, SyncMode::Copy));
        assert!(tgt.join("SKILL.md").exists());
        assert_eq!(fs::read_to_string(tgt.join("SKILL.md")).unwrap(), "# hello");
    }

    #[cfg(unix)]
    #[test]
    fn sync_skill_symlink_creates_symlink() {
        let tmp = tempdir().unwrap();
        let src = tmp.path().join("source");
        let tgt = tmp.path().join("target");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("SKILL.md"), "# hello").unwrap();

        let mode = sync_skill(&src, &tgt, SyncMode::Symlink).unwrap();
        assert!(matches!(mode, SyncMode::Symlink));
        assert!(tgt.is_symlink());
    }

    #[cfg(windows)]
    #[test]
    fn sync_skill_symlink_creates_symlink_on_windows() {
        let tmp = tempdir().unwrap();
        let src = tmp.path().join("source");
        let tgt = tmp.path().join("target");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("SKILL.md"), "# hello").unwrap();

        let mode = sync_skill(&src, &tgt, SyncMode::Symlink).unwrap();
        // Windows requires SeCreateSymbolicLinkPrivilege (Admin or Developer Mode).
        // Production code gracefully falls back to Copy; accept both.
        if tgt.is_symlink() {
            assert!(matches!(mode, SyncMode::Symlink));
        } else {
            assert!(matches!(mode, SyncMode::Copy));
        }
    }

    #[test]
    fn sync_skill_replaces_existing_target() {
        let tmp = tempdir().unwrap();
        let src = tmp.path().join("source");
        let tgt = tmp.path().join("target");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("new.md"), "new").unwrap();

        // Pre-existing target directory
        fs::create_dir_all(&tgt).unwrap();
        fs::write(tgt.join("old.md"), "old").unwrap();

        sync_skill(&src, &tgt, SyncMode::Copy).unwrap();
        assert!(tgt.join("new.md").exists());
        assert!(!tgt.join("old.md").exists());
    }

    #[cfg(unix)]
    #[test]
    fn sync_skill_symlink_skips_existing_correct_link() {
        let tmp = tempdir().unwrap();
        let src = tmp.path().join("source");
        let tgt = tmp.path().join("target");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("SKILL.md"), "# hello").unwrap();
        std::os::unix::fs::symlink(&src, &tgt).unwrap();

        let before = fs::symlink_metadata(&tgt).unwrap().modified().unwrap();
        let mode = sync_skill(&src, &tgt, SyncMode::Symlink).unwrap();

        assert!(matches!(mode, SyncMode::Symlink));
        assert_eq!(fs::read_link(&tgt).unwrap(), src);
        assert_eq!(
            fs::symlink_metadata(&tgt).unwrap().modified().unwrap(),
            before
        );
    }

    // ── copy_dir_recursive ──

    #[test]
    fn copy_dir_recursive_skips_dot_git() {
        let tmp = tempdir().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir_all(src.join(".git")).unwrap();
        fs::write(src.join(".git/config"), "git config").unwrap();
        fs::create_dir_all(src.join("subdir")).unwrap();
        fs::write(src.join("subdir/file.md"), "content").unwrap();
        fs::write(src.join("root.md"), "root").unwrap();

        let dst = tmp.path().join("dst");
        copy_dir_recursive(&src, &dst).unwrap();

        assert!(!dst.join(".git").exists());
        assert!(dst.join("subdir/file.md").exists());
        assert!(dst.join("root.md").exists());
    }

    // ── ensure_dst_not_inside_src ──

    #[test]
    fn ensure_dst_not_inside_src_rejects_subdirectory() {
        let tmp = tempdir().unwrap();
        let src = tmp.path().join("skills");
        fs::create_dir_all(&src).unwrap();
        let dst = src.join("skills");

        let err = ensure_dst_not_inside_src(&src, &dst).unwrap_err();
        assert!(err.to_string().contains("infinite recursion"), "{err}");
    }

    #[test]
    fn ensure_dst_not_inside_src_rejects_same_path() {
        let tmp = tempdir().unwrap();
        let src = tmp.path().join("skills");
        fs::create_dir_all(&src).unwrap();

        let err = ensure_dst_not_inside_src(&src, &src).unwrap_err();
        assert!(err.to_string().contains("infinite recursion"), "{err}");
    }

    #[test]
    fn ensure_dst_not_inside_src_allows_disjoint_paths() {
        let tmp = tempdir().unwrap();
        let src = tmp.path().join("skills");
        let dst = tmp.path().join("other").join("skills");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(dst.parent().unwrap()).unwrap();

        ensure_dst_not_inside_src(&src, &dst).unwrap();
    }

    #[test]
    fn ensure_dst_not_inside_src_allows_sibling_dst() {
        let tmp = tempdir().unwrap();
        let src = tmp.path().join("skills");
        let dst = tmp.path().join("skills-disabled");
        fs::create_dir_all(&src).unwrap();

        ensure_dst_not_inside_src(&src, &dst).unwrap();
    }

    #[test]
    fn sync_skill_refuses_target_inside_source() {
        let tmp = tempdir().unwrap();
        let src = tmp.path().join("skills");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("SKILL.md"), "# hello").unwrap();
        let tgt = src.join("skills");

        let err = sync_skill(&src, &tgt, SyncMode::Copy).unwrap_err();
        assert!(err.to_string().contains("infinite recursion"), "{err}");
        // Source must be untouched after the rejection.
        assert!(src.join("SKILL.md").exists());
    }

    // ── sync_library_replica ──

    #[test]
    fn sync_library_replica_rebuilds_copy_without_importing_replica_drift() {
        let tmp = tempdir().unwrap();
        let primary = tmp.path().join("primary").join("skills");
        let replica = tmp
            .path()
            .join("runtime")
            .join("replica")
            .join(".skills-manager");
        fs::create_dir_all(primary.join("hello")).unwrap();
        fs::write(primary.join("hello/SKILL.md"), "# primary").unwrap();

        fs::create_dir_all(replica.join("hello")).unwrap();
        fs::write(replica.join("hello/SKILL.md"), "# replica edit").unwrap();
        fs::create_dir_all(replica.join("replica-only")).unwrap();
        fs::write(replica.join("replica-only/SKILL.md"), "# drift").unwrap();

        sync_library_replica(&primary, &replica).unwrap();

        assert_eq!(
            fs::read_to_string(primary.join("hello/SKILL.md")).unwrap(),
            "# primary"
        );
        assert_eq!(
            fs::read_to_string(replica.join("hello/SKILL.md")).unwrap(),
            "# primary"
        );
        assert!(!replica.join("replica-only").exists());
    }

    #[test]
    fn sync_library_replica_skips_internal_metadata_dir() {
        let tmp = tempdir().unwrap();
        let primary = tmp.path().join("primary").join("skills");
        let replica = tmp.path().join("runtime").join(".skills-manager");
        fs::create_dir_all(primary.join("hello")).unwrap();
        fs::write(primary.join("hello/SKILL.md"), "# primary").unwrap();
        fs::create_dir_all(primary.join(".skills-manager").join("skills")).unwrap();
        fs::write(
            primary
                .join(".skills-manager")
                .join("skills")
                .join("metadata.json"),
            "{}",
        )
        .unwrap();

        sync_library_replica(&primary, &replica).unwrap();

        assert!(replica.join("hello/SKILL.md").exists());
        assert!(!replica.join(".skills-manager").exists());
    }

    #[test]
    fn sync_library_replica_reports_unreachable_parent_path() {
        let tmp = tempdir().unwrap();
        let primary = tmp.path().join("primary").join("skills");
        fs::create_dir_all(&primary).unwrap();
        fs::write(primary.join("SKILL.md"), "# primary").unwrap();
        let blocked_parent = tmp.path().join("blocked");
        fs::write(&blocked_parent, "not a directory").unwrap();
        let replica = blocked_parent.join(".skills-manager");

        let err = sync_library_replica(&primary, &replica).unwrap_err();

        assert!(
            err.to_string().contains("Failed to create parent dir"),
            "{err}"
        );
    }

    #[test]
    fn sync_library_replica_rejects_non_skills_manager_target() {
        let tmp = tempdir().unwrap();
        let primary = tmp.path().join("primary").join("skills");
        let replica = tmp.path().join("home").join("me").join("Documents");
        fs::create_dir_all(&primary).unwrap();
        fs::write(primary.join("SKILL.md"), "# primary").unwrap();
        fs::create_dir_all(&replica).unwrap();
        fs::write(replica.join("personal.txt"), "keep me").unwrap();

        let err = sync_library_replica(&primary, &replica).unwrap_err();

        assert!(err.to_string().contains(".skills-manager"), "{err}");
        assert_eq!(
            fs::read_to_string(replica.join("personal.txt")).unwrap(),
            "keep me"
        );
    }

    // ── remove_target ──

    #[test]
    fn remove_target_removes_directory() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("to_remove");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("file.txt"), "data").unwrap();

        remove_target(&dir).unwrap();
        assert!(!dir.exists());
    }

    #[test]
    fn remove_target_removes_file() {
        let tmp = tempdir().unwrap();
        let file = tmp.path().join("file.txt");
        fs::write(&file, "data").unwrap();

        remove_target(&file).unwrap();
        assert!(!file.exists());
    }

    #[cfg(unix)]
    #[test]
    fn remove_target_removes_symlink() {
        let tmp = tempdir().unwrap();
        let real = tmp.path().join("real");
        fs::create_dir_all(&real).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        remove_target(&link).unwrap();
        assert!(!link.exists());
        assert!(real.exists()); // original untouched
    }

    #[cfg(windows)]
    #[test]
    fn remove_target_removes_directory_symlink() {
        let tmp = tempdir().unwrap();
        let real = tmp.path().join("real");
        fs::create_dir_all(&real).unwrap();
        fs::write(real.join("SKILL.md"), "# hello").unwrap();
        let link = tmp.path().join("link");
        if let Err(err) = std::os::windows::fs::symlink_dir(&real, &link) {
            eprintln!("skipping directory symlink removal test: {err}");
            return;
        }

        remove_target(&link).unwrap();
        assert!(!link.exists());
        assert!(real.exists());
        assert!(real.join("SKILL.md").exists());
    }

    #[test]
    fn remove_target_nonexistent_is_ok() {
        let tmp = tempdir().unwrap();
        let path = tmp.path().join("does_not_exist");
        assert!(remove_target(&path).is_ok());
    }

    // ── WSL path detection ──

    #[test]
    fn is_wsl_unc_path_matches_wsl_localhost_prefix() {
        assert!(is_wsl_unc_path(Path::new(
            r"\\wsl.localhost\Ubuntu\home\me\path"
        )));
    }

    #[test]
    fn is_wsl_unc_path_matches_forward_slash_prefix() {
        assert!(is_wsl_unc_path(Path::new(
            "//wsl.localhost/Ubuntu/home/me/path"
        )));
    }

    #[test]
    fn is_wsl_unc_path_rejects_local_paths() {
        assert!(!is_wsl_unc_path(Path::new(r"C:\Users\me\path")));
        assert!(!is_wsl_unc_path(Path::new("/home/me/path")));
        assert!(!is_wsl_unc_path(Path::new("relative/path")));
    }

    #[test]
    fn is_wsl_unc_path_rejects_other_unc_prefixes() {
        assert!(!is_wsl_unc_path(Path::new(r"\\server\share\path")));
    }

    // ── remove_wsl_target (path parsing) ──

    #[test]
    fn wsl_distro_from_unc_extracts_distro_name() {
        assert_eq!(
            wsl_distro_from_unc(r"\\wsl.localhost\Ubuntu-24.04\home\me\path").unwrap(),
            "Ubuntu-24.04"
        );
    }

    #[test]
    fn wsl_distro_from_unc_rejects_non_wsl_paths() {
        assert!(wsl_distro_from_unc(r"\\server\share\path").is_err());
    }

    #[test]
    fn wsl_linux_path_from_unc_converts_correctly() {
        assert_eq!(
            wsl_linux_path_from_unc(
                "Ubuntu",
                r"\\wsl.localhost\Ubuntu\home\me\.config\opencode\skills\grill-me"
            )
            .unwrap(),
            "/home/me/.config/opencode/skills/grill-me"
        );
    }

    #[test]
    fn wsl_linux_path_from_unc_rejects_mismatched_distro() {
        assert!(wsl_linux_path_from_unc("Ubuntu", r"\\wsl.localhost\Debian\home\me\path").is_err());
    }

    // ── remove_target for WSL UNC paths ──

    #[test]
    fn remove_target_wsl_path_detection_dispatches_correctly() {
        // The function should detect the WSL UNC path and attempt wsl.exe removal.
        // Since this test doesn't have WSL, it should return an Err containing
        // "wsl.exe" or "Failed to start" in the error message.
        let invalid = Path::new(r"\\wsl.localhost\NonexistentDistro\path\to\skill");
        let result = remove_target(invalid);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("wsl.exe")
                || err.contains("Failed to start")
                || err.contains("remove WSL target"),
            "expected wsl.exe error, got: {err}"
        );
    }
}
