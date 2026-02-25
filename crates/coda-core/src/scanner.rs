//! Feature scanning and state lookup.
//!
//! [`FeatureScanner`] encapsulates the logic for discovering features in the
//! `.trees/` directory (active worktrees) and `.coda/` directory (merged
//! features whose PRs have been merged and worktrees cleaned up).

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use tracing::debug;

use crate::CoreError;
use crate::state::{FeatureState, FeatureStatus};

/// Scans `.trees/` for active worktrees and `.coda/` for merged features.
#[derive(Debug)]
pub struct FeatureScanner {
    trees_dir: PathBuf,
    coda_dir: PathBuf,
}

impl FeatureScanner {
    /// Creates a scanner for the given project root.
    ///
    /// The scanner looks for active features in `<project_root>/.trees/`
    /// and merged features in `<project_root>/.coda/`.
    pub fn new(project_root: &Path) -> Self {
        Self {
            trees_dir: project_root.join(".trees"),
            coda_dir: project_root.join(".coda"),
        }
    }

    /// Lists all features: active (from `.trees/`) and merged (from `.coda/`).
    ///
    /// Active features come from worktree state files at
    /// `.trees/<slug>/.coda/<slug>/state.yml`. Merged features come from
    /// `.coda/<slug>/state.yml` on the main branch. If a slug appears in
    /// both locations, the active worktree version takes precedence.
    ///
    /// Invalid or unparseable state files are silently skipped.
    /// Results are sorted by slug within each group.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::ConfigError` if neither `.trees/` nor `.coda/`
    /// exists.
    pub fn list(&self) -> Result<Vec<FeatureState>, CoreError> {
        let active = self.list_active();
        let merged = self.list_merged(&active);

        if active.is_empty() && merged.is_empty() && !self.trees_dir.is_dir() {
            return Err(CoreError::ConfigError(
                "No .trees/ directory found. Run `coda init` first.".into(),
            ));
        }

        let mut all = active;
        all.extend(merged);
        Ok(all)
    }

    /// Lists only active features from `.trees/`.
    ///
    /// Each worktree directory `<slug>` owns exactly one feature at
    /// `.coda/<slug>/state.yml`. Other `.coda/` subdirectories inherited
    /// from the base branch (e.g. merged features) are ignored to prevent
    /// ghost features from appearing.
    ///
    /// Returns an empty `Vec` if `.trees/` does not exist.
    fn list_active(&self) -> Vec<FeatureState> {
        let mut features = Vec::new();

        let Ok(entries) = fs::read_dir(&self.trees_dir) else {
            return features;
        };

        for worktree_entry in entries.flatten() {
            if !worktree_entry.file_type().is_ok_and(|ft| ft.is_dir()) {
                continue;
            }

            let slug = worktree_entry.file_name();
            let state_path = worktree_entry
                .path()
                .join(".coda")
                .join(&slug)
                .join("state.yml");

            if !state_path.is_file() {
                continue;
            }

            match Self::read_state(&state_path) {
                Ok(state) => features.push(state),
                Err(e) => {
                    debug!(
                        path = %state_path.display(),
                        error = %e,
                        "Skipping invalid state.yml in worktree"
                    );
                }
            }
        }

        features.sort_by(|a, b| a.feature.slug.cmp(&b.feature.slug));
        features
    }

    /// Lists merged features from `.coda/`, excluding any slugs already
    /// present in `active_features`.
    ///
    /// Scans `<project_root>/.coda/<slug>/state.yml` for subdirectories
    /// that contain a valid state file. The status is overwritten to
    /// [`FeatureStatus::Merged`] since the original status may be stale.
    ///
    /// Returns an empty `Vec` if `.coda/` does not exist.
    fn list_merged(&self, active_features: &[FeatureState]) -> Vec<FeatureState> {
        let mut features = Vec::new();

        let Ok(entries) = fs::read_dir(&self.coda_dir) else {
            return features;
        };

        let active_slugs: HashSet<&str> = active_features
            .iter()
            .map(|f| f.feature.slug.as_str())
            .collect();

        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|ft| ft.is_dir()) {
                continue;
            }

            let dir_name = entry.file_name();
            let slug = dir_name.to_string_lossy();

            if active_slugs.contains(slug.as_ref()) {
                continue;
            }

            let state_path = entry.path().join("state.yml");
            if !state_path.is_file() {
                continue;
            }

            match Self::read_state(&state_path) {
                Ok(mut state) => {
                    state.status = FeatureStatus::Merged;
                    features.push(state);
                }
                Err(e) => {
                    debug!(
                        path = %state_path.display(),
                        error = %e,
                        "Skipping invalid merged state.yml"
                    );
                }
            }
        }

        features.sort_by(|a, b| a.feature.slug.cmp(&b.feature.slug));
        features
    }

    /// Returns the state for a specific feature identified by its slug.
    ///
    /// Looks up `.trees/<slug>/.coda/<slug>/state.yml` first (active),
    /// then falls back to `.coda/<slug>/state.yml` (merged).
    ///
    /// # Errors
    ///
    /// Returns `CoreError::ConfigError` if `.trees/` does not exist, or
    /// `CoreError::StateError` if no matching feature is found.
    pub fn get(&self, feature_slug: &str) -> Result<FeatureState, CoreError> {
        if !self.trees_dir.is_dir() {
            return Err(CoreError::ConfigError(
                "No .trees/ directory found. Run `coda init` first.".into(),
            ));
        }

        let active_path = self
            .trees_dir
            .join(feature_slug)
            .join(".coda")
            .join(feature_slug)
            .join("state.yml");

        if active_path.is_file() {
            return Self::read_state(&active_path);
        }

        let merged_path = self.coda_dir.join(feature_slug).join("state.yml");
        if merged_path.is_file() {
            let mut state = Self::read_state(&merged_path)?;
            state.status = FeatureStatus::Merged;
            return Ok(state);
        }

        let available: Vec<String> = fs::read_dir(&self.trees_dir)?
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|ft| ft.is_dir()))
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();

        let hint = if available.is_empty() {
            "No features have been planned yet.".to_string()
        } else {
            format!("Available features: {}", available.join(", "))
        };

        Err(CoreError::StateError(format!(
            "No feature found for slug '{feature_slug}'. {hint}"
        )))
    }

    /// Reads and deserializes a `state.yml` file.
    fn read_state(path: &Path) -> Result<FeatureState, CoreError> {
        let content = fs::read_to_string(path)
            .map_err(|e| CoreError::StateError(format!("Cannot read {}: {e}", path.display())))?;
        serde_yaml_ng::from_str(&content).map_err(|e| {
            CoreError::StateError(format!("Invalid state.yml at {}: {e}", path.display()))
        })
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::state::{
        FeatureInfo, FeatureState, FeatureStatus, GitInfo, PhaseKind, PhaseRecord, PhaseStatus,
        TokenCost, TotalStats,
    };

    use super::*;

    fn make_state(slug: &str) -> FeatureState {
        let now = chrono::Utc::now();
        FeatureState {
            feature: FeatureInfo {
                slug: slug.to_string(),
                created_at: now,
                updated_at: now,
            },
            status: FeatureStatus::Planned,
            current_phase: 0,
            git: GitInfo {
                worktree_path: PathBuf::from(format!(".trees/{slug}")),
                branch: format!("feature/{slug}"),
                base_branch: "main".to_string(),
            },
            phases: vec![
                PhaseRecord {
                    name: "dev".to_string(),
                    kind: PhaseKind::Dev,
                    status: PhaseStatus::Pending,
                    started_at: None,
                    completed_at: None,
                    turns: 0,
                    cost_usd: 0.0,
                    cost: TokenCost::default(),
                    duration_secs: 0,
                    details: serde_json::json!({}),
                },
                PhaseRecord {
                    name: "review".to_string(),
                    kind: PhaseKind::Quality,
                    status: PhaseStatus::Pending,
                    started_at: None,
                    completed_at: None,
                    turns: 0,
                    cost_usd: 0.0,
                    cost: TokenCost::default(),
                    duration_secs: 0,
                    details: serde_json::json!({}),
                },
                PhaseRecord {
                    name: "verify".to_string(),
                    kind: PhaseKind::Quality,
                    status: PhaseStatus::Pending,
                    started_at: None,
                    completed_at: None,
                    turns: 0,
                    cost_usd: 0.0,
                    cost: TokenCost::default(),
                    duration_secs: 0,
                    details: serde_json::json!({}),
                },
                PhaseRecord {
                    name: "update-docs".to_string(),
                    kind: PhaseKind::Quality,
                    status: PhaseStatus::Pending,
                    started_at: None,
                    completed_at: None,
                    turns: 0,
                    cost_usd: 0.0,
                    cost: TokenCost::default(),
                    duration_secs: 0,
                    details: serde_json::json!({}),
                },
            ],
            pr: None,
            total: TotalStats::default(),
        }
    }

    fn write_active_state(root: &Path, slug: &str, state: &FeatureState) {
        let dir = root.join(".trees").join(slug).join(".coda").join(slug);
        fs::create_dir_all(&dir).expect("create state dir");
        let yaml = serde_yaml_ng::to_string(state).expect("serialize state");
        fs::write(dir.join("state.yml"), yaml).expect("write state.yml");
    }

    fn write_merged_state(root: &Path, slug: &str, state: &FeatureState) {
        let dir = root.join(".coda").join(slug);
        fs::create_dir_all(&dir).expect("create merged state dir");
        let yaml = serde_yaml_ng::to_string(state).expect("serialize state");
        fs::write(dir.join("state.yml"), yaml).expect("write state.yml");
    }

    #[test]
    fn test_should_list_empty_trees() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(tmp.path().join(".trees")).expect("mkdir");
        let scanner = FeatureScanner::new(tmp.path());
        assert!(scanner.list().expect("list").is_empty());
    }

    #[test]
    fn test_should_list_sorted_features() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_active_state(tmp.path(), "zzz", &make_state("zzz"));
        write_active_state(tmp.path(), "aaa", &make_state("aaa"));
        let scanner = FeatureScanner::new(tmp.path());

        let features = scanner.list().expect("list");
        assert_eq!(features.len(), 2);
        assert_eq!(features[0].feature.slug, "aaa");
        assert_eq!(features[1].feature.slug, "zzz");
    }

    #[test]
    fn test_should_get_feature_by_slug() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_active_state(tmp.path(), "my-feat", &make_state("my-feat"));
        let scanner = FeatureScanner::new(tmp.path());

        let state = scanner.get("my-feat").expect("get");
        assert_eq!(state.feature.slug, "my-feat");
    }

    #[test]
    fn test_should_error_when_feature_not_found() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_active_state(tmp.path(), "existing", &make_state("existing"));
        let scanner = FeatureScanner::new(tmp.path());

        let err = scanner.get("missing").unwrap_err().to_string();
        assert!(err.contains("missing"));
        assert!(err.contains("existing"));
    }

    #[test]
    fn test_should_error_when_no_trees_and_no_coda() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let scanner = FeatureScanner::new(tmp.path());
        assert!(scanner.list().is_err());
        assert!(scanner.get("any").is_err());
    }

    #[test]
    fn test_should_skip_invalid_state_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_active_state(tmp.path(), "good", &make_state("good"));
        let bad_dir = tmp.path().join(".trees/bad/.coda/bad");
        fs::create_dir_all(&bad_dir).expect("mkdir");
        fs::write(bad_dir.join("state.yml"), "not: valid: yaml: [").expect("write");
        let scanner = FeatureScanner::new(tmp.path());

        let features = scanner.list().expect("list");
        assert_eq!(features.len(), 1);
        assert_eq!(features[0].feature.slug, "good");
    }

    #[test]
    fn test_should_ignore_ghost_features_inherited_from_base_branch() {
        let tmp = tempfile::tempdir().expect("tempdir");

        write_active_state(tmp.path(), "new-feat", &make_state("new-feat"));

        let ghost_dir = tmp.path().join(".trees/new-feat/.coda/old-merged");
        fs::create_dir_all(&ghost_dir).expect("create ghost dir");
        let ghost_yaml =
            serde_yaml_ng::to_string(&make_state("old-merged")).expect("serialize ghost");
        fs::write(ghost_dir.join("state.yml"), ghost_yaml).expect("write ghost state");

        let scanner = FeatureScanner::new(tmp.path());
        let features = scanner.list().expect("list");

        assert_eq!(features.len(), 1, "ghost feature must not appear");
        assert_eq!(features[0].feature.slug, "new-feat");
    }

    #[test]
    fn test_should_not_find_ghost_feature_via_get() {
        let tmp = tempfile::tempdir().expect("tempdir");

        write_active_state(tmp.path(), "active", &make_state("active"));

        let ghost_dir = tmp.path().join(".trees/active/.coda/ghost");
        fs::create_dir_all(&ghost_dir).expect("create ghost dir");
        let ghost_yaml = serde_yaml_ng::to_string(&make_state("ghost")).expect("serialize ghost");
        fs::write(ghost_dir.join("state.yml"), ghost_yaml).expect("write ghost state");

        let scanner = FeatureScanner::new(tmp.path());

        let err = scanner.get("ghost").unwrap_err().to_string();
        assert!(err.contains("ghost"), "error should mention the slug");
        assert!(
            err.contains("active"),
            "hint should list available worktrees"
        );
    }

    #[test]
    fn test_should_list_merged_features_from_coda_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(tmp.path().join(".trees")).expect("mkdir .trees");

        let mut state = make_state("old-feat");
        state.status = FeatureStatus::Completed;
        write_merged_state(tmp.path(), "old-feat", &state);

        let scanner = FeatureScanner::new(tmp.path());
        let features = scanner.list().expect("list");

        assert_eq!(features.len(), 1);
        assert_eq!(features[0].feature.slug, "old-feat");
        assert_eq!(features[0].status, FeatureStatus::Merged);
    }

    #[test]
    fn test_should_combine_active_and_merged_features() {
        let tmp = tempfile::tempdir().expect("tempdir");

        write_active_state(tmp.path(), "active-feat", &make_state("active-feat"));
        write_merged_state(tmp.path(), "merged-feat", &make_state("merged-feat"));

        let scanner = FeatureScanner::new(tmp.path());
        let features = scanner.list().expect("list");

        assert_eq!(features.len(), 2);
        assert_eq!(features[0].feature.slug, "active-feat");
        assert_eq!(features[0].status, FeatureStatus::Planned);
        assert_eq!(features[1].feature.slug, "merged-feat");
        assert_eq!(features[1].status, FeatureStatus::Merged);
    }

    #[test]
    fn test_should_deduplicate_active_over_merged() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let mut active = make_state("my-feat");
        active.status = FeatureStatus::InProgress;
        write_active_state(tmp.path(), "my-feat", &active);

        let mut merged = make_state("my-feat");
        merged.status = FeatureStatus::Completed;
        write_merged_state(tmp.path(), "my-feat", &merged);

        let scanner = FeatureScanner::new(tmp.path());
        let features = scanner.list().expect("list");

        assert_eq!(features.len(), 1, "duplicate slug must be deduplicated");
        assert_eq!(features[0].status, FeatureStatus::InProgress);
    }

    #[test]
    fn test_should_list_only_merged_when_no_trees_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");

        write_merged_state(tmp.path(), "old-feat", &make_state("old-feat"));

        let scanner = FeatureScanner::new(tmp.path());
        let features = scanner.list().expect("list");

        assert_eq!(features.len(), 1);
        assert_eq!(features[0].status, FeatureStatus::Merged);
    }

    #[test]
    fn test_should_skip_non_dir_entries_in_coda() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(tmp.path().join(".trees")).expect("mkdir .trees");
        fs::create_dir_all(tmp.path().join(".coda")).expect("mkdir .coda");
        fs::write(tmp.path().join(".coda/config.yml"), "base_branch: main").expect("write config");

        let scanner = FeatureScanner::new(tmp.path());
        let features = scanner.list().expect("list");
        assert!(features.is_empty());
    }

    #[test]
    fn test_should_skip_invalid_merged_state_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(tmp.path().join(".trees")).expect("mkdir .trees");

        let bad_dir = tmp.path().join(".coda/bad-feat");
        fs::create_dir_all(&bad_dir).expect("mkdir");
        fs::write(bad_dir.join("state.yml"), "not: valid: yaml: [").expect("write");

        write_merged_state(tmp.path(), "good-feat", &make_state("good-feat"));

        let scanner = FeatureScanner::new(tmp.path());
        let features = scanner.list().expect("list");

        assert_eq!(features.len(), 1);
        assert_eq!(features[0].feature.slug, "good-feat");
    }

    #[test]
    fn test_should_get_merged_feature_by_slug() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::create_dir_all(tmp.path().join(".trees")).expect("mkdir .trees");
        write_merged_state(tmp.path(), "old-feat", &make_state("old-feat"));

        let scanner = FeatureScanner::new(tmp.path());
        let state = scanner.get("old-feat").expect("get");

        assert_eq!(state.feature.slug, "old-feat");
        assert_eq!(state.status, FeatureStatus::Merged);
    }

    #[test]
    fn test_should_prefer_active_over_merged_in_get() {
        let tmp = tempfile::tempdir().expect("tempdir");

        let mut active = make_state("my-feat");
        active.status = FeatureStatus::InProgress;
        write_active_state(tmp.path(), "my-feat", &active);

        let mut merged = make_state("my-feat");
        merged.status = FeatureStatus::Completed;
        write_merged_state(tmp.path(), "my-feat", &merged);

        let scanner = FeatureScanner::new(tmp.path());
        let state = scanner.get("my-feat").expect("get");

        assert_eq!(state.status, FeatureStatus::InProgress);
    }
}
