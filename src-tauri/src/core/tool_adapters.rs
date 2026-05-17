use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use super::wsl_runtime;

#[derive(Debug, Clone, Serialize)]
pub struct ToolAdapter {
    pub key: String,
    pub display_name: String,
    pub relative_skills_dir: String,
    pub relative_detect_dir: String,
    /// Additional directories to scan for skills (e.g. plugin/marketplace dirs).
    /// These are only used for discovery, not for deployment.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub additional_scan_dirs: Vec<String>,
    /// When set, overrides the computed skills_dir with this absolute path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub override_skills_dir: Option<String>,
    /// Whether this is a user-defined custom agent (not built-in).
    #[serde(default)]
    pub is_custom: bool,
    /// When true, scan the skills directory recursively for skill directories
    /// (directories containing SKILL.md) instead of treating immediate children as skills.
    /// Used by tools with nested category directories (e.g., Hermes Agent).
    #[serde(default)]
    pub recursive_scan: bool,
    /// Optional override for the project-level skills path. When `None`, the
    /// project-level path falls back to `relative_skills_dir`. Used by tools
    /// like OpenCode where the global path (`~/.config/opencode/skills`)
    /// differs from the project path (`.opencode/skills`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_relative_skills_dir: Option<String>,
}

/// Serializable custom tool definition stored in settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomToolDef {
    pub key: String,
    pub display_name: String,
    pub skills_dir: String,
    #[serde(default)]
    pub project_relative_skills_dir: Option<String>,
}

impl ToolAdapter {
    fn home() -> PathBuf {
        dirs::home_dir().expect("Cannot determine home directory")
    }

    fn candidate_paths(relative: &str) -> Vec<PathBuf> {
        let mut candidates = vec![Self::home().join(relative)];

        if let Some(suffix) = relative.strip_prefix(".config/") {
            if let Some(config_dir) = dirs::config_dir() {
                let config_path = config_dir.join(suffix);
                if !candidates.contains(&config_path) {
                    candidates.push(config_path);
                }
            }
        }

        candidates
    }

    fn select_existing_or_default(paths: &[PathBuf]) -> PathBuf {
        paths
            .iter()
            .find(|path| path.exists())
            .cloned()
            .unwrap_or_else(|| paths[0].clone())
    }

    pub fn skills_dir(&self) -> PathBuf {
        if let Some(ref abs) = self.override_skills_dir {
            return PathBuf::from(abs);
        }
        let candidates = Self::candidate_paths(&self.relative_skills_dir);
        Self::select_existing_or_default(&candidates)
    }

    /// Project-relative skills path used when scanning workspaces. Falls back
    /// to `relative_skills_dir` when no project-specific override is set.
    pub fn project_relative_skills_dir(&self) -> &str {
        self.project_relative_skills_dir
            .as_deref()
            .unwrap_or(&self.relative_skills_dir)
    }

    /// Returns all directories to scan for skills: the primary skills_dir plus any additional scan dirs.
    pub fn all_scan_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = vec![self.skills_dir()];
        for c in self.additional_existing_scan_dirs() {
            if !dirs.contains(&c) {
                dirs.push(c);
            }
        }
        dirs
    }

    /// Returns the existing additional discovery roots for this adapter.
    pub fn additional_existing_scan_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = Vec::new();
        for rel in &self.additional_scan_dirs {
            let candidates = Self::candidate_paths(rel);
            for c in candidates {
                if c.exists() && !dirs.contains(&c) {
                    dirs.push(c);
                }
            }
        }
        dirs
    }

    pub fn is_installed(&self) -> bool {
        // Product decision: when users explicitly provide a skills path (override/custom),
        // we treat the tool as available so sync can proceed without probing vendor install state.
        if self.is_custom || (self.override_skills_dir.is_some() && !self.key.starts_with("wsl:")) {
            return true;
        }
        if self.key.starts_with("wsl:") {
            let detect_dir = if self.relative_detect_dir.is_empty() {
                self.override_skills_dir.as_deref().unwrap_or_default()
            } else {
                self.relative_detect_dir.as_str()
            };
            return !detect_dir.is_empty() && PathBuf::from(detect_dir).exists();
        }
        Self::candidate_paths(&self.relative_detect_dir)
            .iter()
            .any(|path| path.exists())
    }

    /// Whether this adapter's skills_dir has been overridden from the default.
    pub fn has_path_override(&self) -> bool {
        self.override_skills_dir.is_some()
    }
}

pub fn default_tool_adapters() -> Vec<ToolAdapter> {
    vec![
        ToolAdapter {
            key: "cursor".into(),
            display_name: "Cursor".into(),
            relative_skills_dir: ".cursor/skills".into(),
            relative_detect_dir: ".cursor".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "claude_code".into(),
            display_name: "Claude Code".into(),
            relative_skills_dir: ".claude/skills".into(),
            relative_detect_dir: ".claude".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            // Codex reads user-level skills only from `~/.agents/skills` per the
            // official docs (developers.openai.com/codex/skills). Deploy there;
            // keep `.codex/skills` as a discovery fallback for users whose
            // earlier syncs landed in the old (incorrect) deployment target.
            //
            // Project-level path is pinned to `.codex/skills` to preserve the
            // existing per-project UI grouping. Codex CLI's repo-scope reads
            // <repo>/.agents/skills (which is already covered by other adapters
            // sharing that path like cline/warp), so this pin doesn't lose
            // functionality — it just keeps Codex from displacing those
            // adapters as the representative for the shared `.agents/skills`
            // project group.
            key: "codex".into(),
            display_name: "Codex".into(),
            relative_skills_dir: ".agents/skills".into(),
            relative_detect_dir: ".codex".into(),
            additional_scan_dirs: vec![".codex/skills".into()],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: Some(".codex/skills".into()),
        },
        ToolAdapter {
            key: "opencode".into(),
            display_name: "OpenCode".into(),
            relative_skills_dir: ".config/opencode/skills".into(),
            relative_detect_dir: ".config/opencode".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: Some(".opencode/skills".into()),
        },
        ToolAdapter {
            key: "antigravity".into(),
            display_name: "Antigravity".into(),
            relative_skills_dir: ".gemini/antigravity/skills".into(),
            relative_detect_dir: ".gemini/antigravity".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "amp".into(),
            display_name: "Amp".into(),
            relative_skills_dir: ".config/agents/skills".into(),
            relative_detect_dir: ".config/agents".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "kilo_code".into(),
            display_name: "Kilo Code".into(),
            relative_skills_dir: ".kilocode/skills".into(),
            relative_detect_dir: ".kilocode".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "roo_code".into(),
            display_name: "Roo Code".into(),
            relative_skills_dir: ".roo/skills".into(),
            relative_detect_dir: ".roo".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "goose".into(),
            display_name: "Goose".into(),
            relative_skills_dir: ".config/goose/skills".into(),
            relative_detect_dir: ".config/goose".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "gemini_cli".into(),
            display_name: "Gemini CLI".into(),
            relative_skills_dir: ".gemini/skills".into(),
            relative_detect_dir: ".gemini".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "github_copilot".into(),
            display_name: "GitHub Copilot".into(),
            relative_skills_dir: ".copilot/skills".into(),
            relative_detect_dir: ".copilot".into(),
            // GitHub Copilot now reads skills from the unified `~/.agents/skills` location too.
            additional_scan_dirs: vec![".agents/skills".into()],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "openclaw".into(),
            display_name: "OpenClaw".into(),
            relative_skills_dir: ".openclaw/skills".into(),
            relative_detect_dir: ".openclaw".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "droid".into(),
            display_name: "Droid".into(),
            relative_skills_dir: ".factory/skills".into(),
            relative_detect_dir: ".factory".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "windsurf".into(),
            display_name: "Windsurf".into(),
            relative_skills_dir: ".codeium/windsurf/skills".into(),
            relative_detect_dir: ".codeium/windsurf".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "trae".into(),
            display_name: "TRAE IDE".into(),
            relative_skills_dir: ".trae/skills".into(),
            relative_detect_dir: ".trae".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "cline".into(),
            display_name: "Cline".into(),
            relative_skills_dir: ".agents/skills".into(),
            relative_detect_dir: ".cline".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "deepagents".into(),
            display_name: "Deep Agents".into(),
            relative_skills_dir: ".deepagents/agent/skills".into(),
            relative_detect_dir: ".deepagents".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "firebender".into(),
            display_name: "Firebender".into(),
            relative_skills_dir: ".firebender/skills".into(),
            relative_detect_dir: ".firebender".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "kimi".into(),
            display_name: "Kimi Code CLI".into(),
            relative_skills_dir: ".config/agents/skills".into(),
            relative_detect_dir: ".kimi".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "replit".into(),
            display_name: "Replit".into(),
            relative_skills_dir: ".config/agents/skills".into(),
            relative_detect_dir: ".replit".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "warp".into(),
            display_name: "Warp".into(),
            relative_skills_dir: ".agents/skills".into(),
            relative_detect_dir: ".warp".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "augment".into(),
            display_name: "Augment".into(),
            relative_skills_dir: ".augment/skills".into(),
            relative_detect_dir: ".augment".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "bob".into(),
            display_name: "IBM Bob".into(),
            relative_skills_dir: ".bob/skills".into(),
            relative_detect_dir: ".bob".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "codebuddy".into(),
            display_name: "CodeBuddy".into(),
            relative_skills_dir: ".codebuddy/skills".into(),
            relative_detect_dir: ".codebuddy".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "command_code".into(),
            display_name: "Command Code".into(),
            relative_skills_dir: ".commandcode/skills".into(),
            relative_detect_dir: ".commandcode".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "continue".into(),
            display_name: "Continue".into(),
            relative_skills_dir: ".continue/skills".into(),
            relative_detect_dir: ".continue".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "cortex".into(),
            display_name: "Cortex Code".into(),
            relative_skills_dir: ".snowflake/cortex/skills".into(),
            relative_detect_dir: ".snowflake/cortex".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "crush".into(),
            display_name: "Crush".into(),
            relative_skills_dir: ".config/crush/skills".into(),
            relative_detect_dir: ".config/crush".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "iflow".into(),
            display_name: "iFlow CLI".into(),
            relative_skills_dir: ".iflow/skills".into(),
            relative_detect_dir: ".iflow".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "junie".into(),
            display_name: "Junie".into(),
            relative_skills_dir: ".junie/skills".into(),
            relative_detect_dir: ".junie".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "kiro".into(),
            display_name: "Kiro CLI".into(),
            relative_skills_dir: ".kiro/skills".into(),
            relative_detect_dir: ".kiro".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "kode".into(),
            display_name: "Kode".into(),
            relative_skills_dir: ".kode/skills".into(),
            relative_detect_dir: ".kode".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "mcpjam".into(),
            display_name: "MCPJam".into(),
            relative_skills_dir: ".mcpjam/skills".into(),
            relative_detect_dir: ".mcpjam".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "mistral_vibe".into(),
            display_name: "Mistral Vibe".into(),
            relative_skills_dir: ".vibe/skills".into(),
            relative_detect_dir: ".vibe".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "mux".into(),
            display_name: "Mux".into(),
            relative_skills_dir: ".mux/skills".into(),
            relative_detect_dir: ".mux".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "neovate".into(),
            display_name: "Neovate".into(),
            relative_skills_dir: ".neovate/skills".into(),
            relative_detect_dir: ".neovate".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "openhands".into(),
            display_name: "OpenHands".into(),
            relative_skills_dir: ".openhands/skills".into(),
            relative_detect_dir: ".openhands".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "pi".into(),
            display_name: "Pi".into(),
            relative_skills_dir: ".pi/agent/skills".into(),
            relative_detect_dir: ".pi/agent".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "pochi".into(),
            display_name: "Pochi".into(),
            relative_skills_dir: ".pochi/skills".into(),
            relative_detect_dir: ".pochi".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "qoder".into(),
            display_name: "Qoder".into(),
            relative_skills_dir: ".qoder/skills".into(),
            relative_detect_dir: ".qoder".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "qwen_code".into(),
            display_name: "Qwen Code".into(),
            relative_skills_dir: ".qwen/skills".into(),
            relative_detect_dir: ".qwen".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "trae_cn".into(),
            display_name: "TRAE CN".into(),
            relative_skills_dir: ".trae-cn/skills".into(),
            relative_detect_dir: ".trae-cn".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "zencoder".into(),
            display_name: "Zencoder".into(),
            relative_skills_dir: ".zencoder/skills".into(),
            relative_detect_dir: ".zencoder".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "adal".into(),
            display_name: "AdaL".into(),
            relative_skills_dir: ".adal/skills".into(),
            relative_detect_dir: ".adal".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: false,
            project_relative_skills_dir: None,
        },
        ToolAdapter {
            key: "hermes".into(),
            display_name: "Hermes Agent".into(),
            relative_skills_dir: ".hermes/skills".into(),
            relative_detect_dir: ".hermes".into(),
            additional_scan_dirs: vec![],
            override_skills_dir: None,
            is_custom: false,
            recursive_scan: true,
            project_relative_skills_dir: None,
        },
    ]
}

/// Read custom tool path overrides from store.
pub fn custom_tool_paths(store: &crate::core::skill_store::SkillStore) -> HashMap<String, String> {
    store
        .get_setting("custom_tool_paths")
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default()
}

/// Read user-defined custom tools from store.
pub fn custom_tools(store: &crate::core::skill_store::SkillStore) -> Vec<CustomToolDef> {
    store
        .get_setting("custom_tools")
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default()
}

/// Returns all tool adapters: built-in (with path overrides applied) + custom tools.
pub fn all_tool_adapters(store: &crate::core::skill_store::SkillStore) -> Vec<ToolAdapter> {
    let overrides = custom_tool_paths(store);
    let customs = custom_tools(store);

    let mut adapters: Vec<ToolAdapter> = default_tool_adapters()
        .into_iter()
        .map(|mut a| {
            if let Some(path) = overrides.get(&a.key) {
                a.override_skills_dir = Some(path.clone());
            }
            a
        })
        .collect();

    for ct in customs {
        adapters.push(ToolAdapter {
            key: ct.key,
            display_name: ct.display_name,
            relative_skills_dir: ct.project_relative_skills_dir.unwrap_or_default(),
            relative_detect_dir: String::new(),
            additional_scan_dirs: vec![],
            override_skills_dir: Some(ct.skills_dir),
            is_custom: true,
            recursive_scan: false,
            project_relative_skills_dir: None,
        });
    }

    adapters.extend(wsl_tool_adapters(store));
    adapters
}

pub fn discovery_tool_adapters(store: &crate::core::skill_store::SkillStore) -> Vec<ToolAdapter> {
    let mut adapters = all_tool_adapters(store);
    adapters.extend(wsl_library_replica_scan_adapters(store));
    adapters
}

fn wsl_library_replica_scan_adapters(
    store: &crate::core::skill_store::SkillStore,
) -> Vec<ToolAdapter> {
    wsl_runtime::list_runtime_environments(store)
        .unwrap_or_default()
        .into_iter()
        .map(|runtime| ToolAdapter {
            key: wsl_runtime::wsl_tool_key(&runtime.distro_name, "library_replica"),
            display_name: format!("Library Replica ({})", runtime.distro_name),
            relative_skills_dir: String::new(),
            relative_detect_dir: String::new(),
            additional_scan_dirs: vec![],
            override_skills_dir: Some(runtime.library_replica_path),
            is_custom: true,
            recursive_scan: false,
            project_relative_skills_dir: None,
        })
        .collect()
}

fn wsl_tool_adapters(store: &crate::core::skill_store::SkillStore) -> Vec<ToolAdapter> {
    let runtimes = wsl_runtime::list_runtime_environments(store).unwrap_or_default();
    let base_adapters = all_windows_tool_adapters(store);
    let mut adapters = Vec::new();

    for runtime_status in runtimes {
        let runtime = wsl_runtime::WslRuntimeEnvironment {
            distro_name: runtime_status.distro_name.clone(),
            library_replica_path: runtime_status.library_replica_path.clone(),
            agent_targets: runtime_status.agent_targets,
        };

        for base in &base_adapters {
            let configured = wsl_runtime::configured_agent_target(&runtime, &base.key);
            let skills_dir = configured
                .as_ref()
                .and_then(|target| target.skills_dir.clone())
                .or_else(|| {
                    wsl_runtime::resolve_agent_target_path(&runtime, &base.relative_skills_dir).ok()
                });
            let Some(skills_dir) = skills_dir else {
                continue;
            };
            let detect_dir =
                wsl_runtime::resolve_agent_target_path(&runtime, &base.relative_detect_dir)
                    .unwrap_or_default();

            adapters.push(ToolAdapter {
                key: wsl_runtime::wsl_tool_key(&runtime.distro_name, &base.key),
                display_name: format!("{} ({})", base.display_name, runtime.distro_name),
                relative_skills_dir: base.relative_skills_dir.clone(),
                relative_detect_dir: detect_dir,
                additional_scan_dirs: vec![],
                override_skills_dir: Some(skills_dir),
                is_custom: base.is_custom,
                recursive_scan: base.recursive_scan,
                project_relative_skills_dir: base.project_relative_skills_dir.clone(),
            });
        }

        for configured in &runtime.agent_targets {
            let is_known_base_adapter = base_adapters
                .iter()
                .any(|adapter| adapter.key == configured.key);
            if is_known_base_adapter {
                continue;
            }
            let skills_dir = configured.skills_dir.clone();
            let Some(skills_dir) = skills_dir else {
                continue;
            };

            adapters.push(ToolAdapter {
                key: wsl_runtime::wsl_tool_key(&runtime.distro_name, &configured.key),
                display_name: format!("{} ({})", configured.key, runtime.distro_name),
                relative_skills_dir: String::new(),
                relative_detect_dir: String::new(),
                additional_scan_dirs: vec![],
                override_skills_dir: Some(skills_dir),
                is_custom: true,
                recursive_scan: false,
                project_relative_skills_dir: None,
            });
        }
    }

    adapters
}

fn all_windows_tool_adapters(store: &crate::core::skill_store::SkillStore) -> Vec<ToolAdapter> {
    let overrides = custom_tool_paths(store);
    let customs = custom_tools(store);

    let mut adapters: Vec<ToolAdapter> = default_tool_adapters()
        .into_iter()
        .map(|mut a| {
            if let Some(path) = overrides.get(&a.key) {
                a.override_skills_dir = Some(path.clone());
            }
            a
        })
        .collect();

    for ct in customs {
        adapters.push(ToolAdapter {
            key: ct.key,
            display_name: ct.display_name,
            relative_skills_dir: ct.project_relative_skills_dir.unwrap_or_default(),
            relative_detect_dir: String::new(),
            additional_scan_dirs: vec![],
            override_skills_dir: Some(ct.skills_dir),
            is_custom: true,
            recursive_scan: false,
            project_relative_skills_dir: None,
        });
    }

    adapters
}

#[allow(dead_code)]
pub fn find_adapter(key: &str) -> Option<ToolAdapter> {
    default_tool_adapters().into_iter().find(|a| a.key == key)
}

/// Find an adapter by key, considering custom tools and path overrides.
pub fn find_adapter_with_store(
    store: &crate::core::skill_store::SkillStore,
    key: &str,
) -> Option<ToolAdapter> {
    if wsl_runtime::parse_wsl_tool_key(key).is_some() {
        return all_tool_adapters(store).into_iter().find(|a| a.key == key);
    }

    if let Some(mut adapter) = default_tool_adapters().into_iter().find(|a| a.key == key) {
        if let Some(path) = custom_tool_paths(store).get(key) {
            adapter.override_skills_dir = Some(path.clone());
        }
        return Some(adapter);
    }

    custom_tools(store)
        .into_iter()
        .find(|ct| ct.key == key)
        .map(|ct| ToolAdapter {
            key: ct.key,
            display_name: ct.display_name,
            relative_skills_dir: ct.project_relative_skills_dir.unwrap_or_default(),
            relative_detect_dir: String::new(),
            additional_scan_dirs: vec![],
            override_skills_dir: Some(ct.skills_dir),
            is_custom: true,
            recursive_scan: false,
            project_relative_skills_dir: None,
        })
}

/// Returns adapters that are installed and not in the disabled list.
pub fn enabled_installed_adapters(
    store: &crate::core::skill_store::SkillStore,
) -> Vec<ToolAdapter> {
    let disabled: Vec<String> = store
        .get_setting("disabled_tools")
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_str(&v).ok())
        .unwrap_or_default();
    all_tool_adapters(store)
        .into_iter()
        .filter(|a| {
            if let Some((distro_name, agent_key)) = wsl_runtime::parse_wsl_tool_key(&a.key) {
                return a.is_installed()
                    && wsl_runtime::agent_target_enabled(store, distro_name, agent_key);
            }
            a.is_installed() && !disabled.contains(&a.key)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{all_tool_adapters, default_tool_adapters};
    use crate::core::skill_store::SkillStore;
    use tempfile::tempdir;

    #[test]
    fn antigravity_uses_current_default_skills_path() {
        let adapter = default_tool_adapters()
            .into_iter()
            .find(|adapter| adapter.key == "antigravity")
            .expect("antigravity adapter should exist");

        assert_eq!(adapter.relative_skills_dir, ".gemini/antigravity/skills");
    }

    #[test]
    fn claude_code_does_not_scan_plugin_marketplaces_by_default() {
        let adapter = default_tool_adapters()
            .into_iter()
            .find(|adapter| adapter.key == "claude_code")
            .expect("claude_code adapter should exist");

        assert!(adapter.additional_scan_dirs.is_empty());
    }

    #[test]
    fn opencode_uses_distinct_project_and_global_skill_paths() {
        let adapter = default_tool_adapters()
            .into_iter()
            .find(|adapter| adapter.key == "opencode")
            .expect("opencode adapter should exist");

        // Global path under home: ~/.config/opencode/skills
        assert_eq!(adapter.relative_skills_dir, ".config/opencode/skills");
        // Project path under workspace: .opencode/skills
        assert_eq!(adapter.project_relative_skills_dir(), ".opencode/skills");
    }

    #[test]
    fn wsl_runtime_agent_targets_resolve_independently_from_windows_overrides() {
        let tmp = tempdir().unwrap();
        let store = SkillStore::new(&tmp.path().join("tools.db")).unwrap();
        store
            .set_setting(
                "custom_tool_paths",
                r#"{"codex":"C:\\Users\\me\\custom-codex-skills"}"#,
            )
            .unwrap();
        store
            .set_setting(
                "wsl_runtime_environments",
                r#"[{"distro_name":"Ubuntu","library_replica_path":"\\\\wsl.localhost\\Ubuntu\\home\\me\\skills","agent_targets":[{"key":"codex","enabled":true}] }]"#,
            )
            .unwrap();

        let adapters = all_tool_adapters(&store);
        let windows_codex = adapters
            .iter()
            .find(|adapter| adapter.key == "codex")
            .expect("Windows Codex adapter should exist");
        let wsl_codex = adapters
            .iter()
            .find(|adapter| adapter.key == "wsl:Ubuntu:codex")
            .expect("WSL Codex adapter should exist");

        assert_eq!(
            windows_codex.skills_dir().to_string_lossy(),
            r"C:\Users\me\custom-codex-skills"
        );
        assert_eq!(
            wsl_codex.skills_dir().to_string_lossy(),
            r"\\wsl.localhost\Ubuntu\home\me\.agents\skills"
        );
    }

    #[test]
    fn disabled_wsl_runtime_agent_targets_remain_listable_but_not_enabled() {
        let tmp = tempdir().unwrap();
        let store = SkillStore::new(&tmp.path().join("disabled-wsl-tools.db")).unwrap();
        store
            .set_setting(
                "wsl_runtime_environments",
                r#"[{"distro_name":"Ubuntu","library_replica_path":"\\\\wsl.localhost\\Ubuntu\\home\\me\\skills","agent_targets":[{"key":"codex","enabled":false}]}]"#,
            )
            .unwrap();

        let adapters = all_tool_adapters(&store);
        assert!(adapters
            .iter()
            .any(|adapter| adapter.key == "wsl:Ubuntu:codex"));
        assert!(!super::enabled_installed_adapters(&store)
            .iter()
            .any(|adapter| adapter.key == "wsl:Ubuntu:codex"));
    }

    #[test]
    fn wsl_runtime_agent_targets_are_listed_without_preconfiguration() {
        let tmp = tempdir().unwrap();
        let store = SkillStore::new(&tmp.path().join("wsl-default-targets.db")).unwrap();
        store
            .set_setting(
                "wsl_runtime_environments",
                r#"[{"distro_name":"Ubuntu","library_replica_path":"\\\\wsl.localhost\\Ubuntu\\mnt\\d\\skills"}]"#,
            )
            .unwrap();

        let adapters = all_tool_adapters(&store);
        let wsl_codex = adapters
            .iter()
            .find(|adapter| adapter.key == "wsl:Ubuntu:codex")
            .expect("WSL Codex target should be listable");

        assert_eq!(
            wsl_codex.skills_dir().to_string_lossy(),
            r"\\wsl.localhost\Ubuntu\mnt\d\.agents\skills"
        );
        assert!(!wsl_codex.is_installed());
        assert!(!super::enabled_installed_adapters(&store)
            .iter()
            .any(|adapter| adapter.key == "wsl:Ubuntu:codex"));
    }

    #[test]
    fn wsl_custom_agent_targets_without_relative_paths_require_explicit_wsl_paths() {
        let tmp = tempdir().unwrap();
        let store = SkillStore::new(&tmp.path().join("wsl-custom-empty-relative.db")).unwrap();
        store
            .set_setting(
                "custom_tools",
                r#"[{"key":"custom_agent","display_name":"Custom Agent","skills_dir":"C:\\custom\\skills"}]"#,
            )
            .unwrap();
        store
            .set_setting(
                "wsl_runtime_environments",
                r#"[{"distro_name":"Ubuntu","library_replica_path":"\\\\wsl.localhost\\Ubuntu\\home\\me\\skills","agent_targets":[{"key":"custom_agent","enabled":true}]}]"#,
            )
            .unwrap();

        assert!(!all_tool_adapters(&store)
            .iter()
            .any(|adapter| adapter.key == "wsl:Ubuntu:custom_agent"));
    }

    #[test]
    fn wsl_library_replica_is_discovery_only_not_a_tool_adapter() {
        let tmp = tempdir().unwrap();
        let store = SkillStore::new(&tmp.path().join("wsl-library-only.db")).unwrap();
        store
            .set_setting(
                "wsl_runtime_environments",
                r#"[{"distro_name":"Ubuntu","library_replica_path":"\\\\wsl.localhost\\Ubuntu\\home\\me\\.codex\\skills"}]"#,
            )
            .unwrap();

        let adapters = all_tool_adapters(&store);
        let discovery_adapters = super::discovery_tool_adapters(&store);

        assert!(!adapters
            .iter()
            .any(|adapter| adapter.key == "wsl:Ubuntu:library_replica"));
        assert!(discovery_adapters
            .iter()
            .any(|adapter| adapter.key == "wsl:Ubuntu:library_replica"));
    }

    #[test]
    fn wsl_library_replica_is_never_an_enabled_sync_adapter() {
        let tmp = tempdir().unwrap();
        let store = SkillStore::new(&tmp.path().join("wsl-replica-not-sync.db")).unwrap();
        store
            .set_setting(
                "wsl_runtime_environments",
                r#"[{"distro_name":"Ubuntu","library_replica_path":"\\\\wsl.localhost\\Ubuntu\\home\\me\\.codex\\skills"}]"#,
            )
            .unwrap();

        assert!(!super::enabled_installed_adapters(&store)
            .iter()
            .any(|adapter| adapter.key == "wsl:Ubuntu:library_replica"));
    }

    #[test]
    fn wsl_discovery_includes_explicit_agent_target_paths() {
        let tmp = tempdir().unwrap();
        let store = SkillStore::new(&tmp.path().join("wsl-agent-targets.db")).unwrap();
        store
            .set_setting(
                "wsl_runtime_environments",
                r#"[{"distro_name":"Ubuntu","library_replica_path":"\\\\wsl.localhost\\Ubuntu\\home\\me\\.codex\\skills","agent_targets":[{"key":"codex","enabled":true,"skills_dir":"\\\\wsl.localhost\\Ubuntu\\home\\me\\.agents\\skills"}]}]"#,
            )
            .unwrap();

        let adapters = all_tool_adapters(&store);
        let wsl_codex = adapters
            .iter()
            .find(|adapter| adapter.key == "wsl:Ubuntu:codex")
            .expect("explicit WSL Codex target should be scanned");

        assert_eq!(
            wsl_codex.skills_dir().to_string_lossy(),
            r"\\wsl.localhost\Ubuntu\home\me\.agents\skills"
        );
    }
}
