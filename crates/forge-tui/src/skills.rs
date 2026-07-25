use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct SkillInfo {
    pub name: String,
    pub enabled: bool,
    pub file_count: usize,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct SkillsState {
    disabled: Vec<String>,
}

impl Default for SkillsState {
    fn default() -> Self {
        Self {
            disabled: Vec::new(),
        }
    }
}

pub struct SkillManager {
    dir: PathBuf,
    state_path: PathBuf,
    state: SkillsState,
}

impl SkillManager {
    pub fn new(workspace: &Path) -> Self {
        let dir = workspace.join(".forge").join("skills");
        let state_path = dir.join("skills-state.json");
        let state = Self::load_state(&state_path);
        Self {
            dir,
            state_path,
            state,
        }
    }

    fn load_state(path: &Path) -> SkillsState {
        fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save_state(&self) {
        if let Some(parent) = self.state_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(s) = serde_json::to_string(&self.state) {
            let _ = fs::write(&self.state_path, s);
        }
    }

    pub fn list_skills(&self) -> Vec<SkillInfo> {
        let disabled: HashSet<&str> = self.state.disabled.iter().map(|s| s.as_str()).collect();
        let entries = match fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };
        let mut skills: Vec<SkillInfo> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let skill_dir = e.path();
                let file_count = fs::read_dir(&skill_dir)
                    .ok()
                    .map(|entries| entries.filter_map(|e| e.ok()).count())
                    .unwrap_or(0);
                Some(SkillInfo {
                    enabled: !disabled.contains(name.as_str()),
                    file_count,
                    name,
                })
            })
            .collect();
        skills.sort_by(|a, b| a.name.cmp(&b.name));
        skills
    }

    pub fn install_skill(&mut self, url: &str) -> Result<String, String> {
        let name = extract_repo_name(url)?;
        let target = self.dir.join(&name);
        if target.exists() {
            return Err(format!("skill '{name}' already installed"));
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("create dirs: {e}"))?;
        }
        let status = Command::new("git")
            .args(["clone", "--depth", "1", url])
            .arg(&target)
            .status()
            .map_err(|e| format!("git clone failed: {e}"))?;
        if !status.success() {
            let _ = fs::remove_dir_all(&target);
            return Err("git clone failed".into());
        }
        if !target.join("SKILL.md").exists() {
            let _ = fs::remove_dir_all(&target);
            return Err("no SKILL.md in repo root".into());
        }
        Ok(name)
    }

    pub fn uninstall_skill(&mut self, name: &str) -> Result<(), String> {
        let target = self.dir.join(name);
        if !target.exists() {
            return Err(format!("skill '{name}' not found"));
        }
        fs::remove_dir_all(&target).map_err(|e| format!("remove failed: {e}"))?;
        self.state.disabled.retain(|d| d != name);
        self.save_state();
        Ok(())
    }

    pub fn enable_skill(&mut self, name: &str) -> Result<(), String> {
        if !self.dir.join(name).exists() {
            return Err(format!("skill '{name}' not installed"));
        }
        self.state.disabled.retain(|d| d != name);
        self.save_state();
        Ok(())
    }

    pub fn disable_skill(&mut self, name: &str) -> Result<(), String> {
        if !self.dir.join(name).exists() {
            return Err(format!("skill '{name}' not installed"));
        }
        if !self.state.disabled.contains(&name.to_string()) {
            self.state.disabled.push(name.to_string());
            self.save_state();
        }
        Ok(())
    }

}

fn extract_repo_name(url: &str) -> Result<String, String> {
    let trimmed = url.trim().trim_end_matches(".git");
    let name = trimmed
        .split('/')
        .last()
        .ok_or_else(|| "could not parse repo name from URL".to_string())?;
    if name.is_empty() {
        return Err("empty repo name".into());
    }
    Ok(name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_repo_name_from_github_url() {
        assert_eq!(
            extract_repo_name("https://github.com/user/my-skill").unwrap(),
            "my-skill"
        );
        assert_eq!(
            extract_repo_name("https://github.com/user/my-skill.git").unwrap(),
            "my-skill"
        );
        assert_eq!(
            extract_repo_name("user/my-skill").unwrap(),
            "my-skill"
        );
    }

    #[test]
    fn skill_manager_list_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = SkillManager::new(dir.path());
        assert!(mgr.list_skills().is_empty());
        assert!(mgr.enable_skill("nope").is_err());
        assert!(mgr.disable_skill("nope").is_err());
        assert!(mgr.uninstall_skill("nope").is_err());
    }

    #[test]
    fn skill_manager_enable_disable_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut mgr = SkillManager::new(dir.path());
        let skill_dir = mgr.dir.join("test-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), "# Test").unwrap();
        // default: enabled
        let skills = mgr.list_skills();
        assert!(skills.iter().any(|s| s.name == "test-skill" && s.enabled));
        mgr.disable_skill("test-skill").unwrap();
        let skills = mgr.list_skills();
        assert!(!skills.iter().any(|s| s.name == "test-skill" && s.enabled));
        mgr.enable_skill("test-skill").unwrap();
        let skills = mgr.list_skills();
        assert!(skills.iter().any(|s| s.name == "test-skill" && s.enabled));
    }

    #[test]
    fn skill_state_persists() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join(".forge").join("skills").join("askill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(skill_dir.join("SKILL.md"), "# A").unwrap();
        {
            let mut mgr = SkillManager::new(dir.path());
            mgr.disable_skill("askill").unwrap();
        }
        {
            let mgr = SkillManager::new(dir.path());
            let skills = mgr.list_skills();
            assert!(!skills.iter().any(|s| s.name == "askill" && s.enabled));
        }
    }
}
