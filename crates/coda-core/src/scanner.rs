//! Feature scanning and state lookup.
//!
//! [`FeatureScanner`] encapsulates the logic for discovering features in the
//! `.trees/` directory and reading their persisted `state.yml` files.

use std::fs;
use std::path::{Path, PathBuf};

use tracing::debug;

use crate::CoreError;
use crate::state::FeatureState;

/// Scans the `.trees/` directory for feature worktrees and reads their state.
#[derive(Debug)]
pub struct FeatureScanner {
    trees_dir: PathBuf,
}

impl FeatureScanner {
    /// Creates a scanner for the given project root.
    ///
    /// The scanner will look for features inside `<project_root>/.trees/`.
    pub fn new(project_root: &Path) -> Self {
        Self {
            trees_dir: project_root.join(".trees"),
        }
    }

    /// Lists all features by scanning worktrees in `.trees/`.
    ///
    /// Each worktree is expected to contain `.coda/<slug>/state.yml`.
    /// Invalid state files are silently skipped. Results are sorted by slug.
    ///
    /// # Errors
    ///
    /// Returns `CoreError::ConfigError` if `.trees/` does not exist.
    pub fn list(&self) -> Result<Vec<FeatureState>, CoreError> {
        if !self.trees_dir.is_dir() {
            return Err(CoreError::ConfigError(
                "No .trees/ directory found. Run `coda init` first.".into(),
            ));
        }

        let mut features = Vec::new();

        for worktree_entry in fs::read_dir(&self.trees_dir)?.flatten() {
            if !worktree_entry.file_type().is_ok_and(|ft| ft.is_dir()) {
                continue;
            }

            let coda_dir = worktree_entry.path().join(".coda");
            if !coda_dir.is_dir() {
                continue;
            }

            for feature_entry in fs::read_dir(&coda_dir)?.flatten() {
                if !feature_entry.file_type().is_ok_and(|ft| ft.is_dir()) {
                    continue;
                }

                let state_path = feature_entry.path().join("state.yml");
                if !state_path.is_file() {
                    continue;
                }

                let content = fs::read_to_string(&state_path).map_err(|e| {
                    CoreError::StateError(format!("Cannot read {}: {e}", state_path.display()))
                })?;

                match serde_yaml::from_str::<FeatureState>(&content) {
                    Ok(state) => features.push(state),
                    Err(e) => {
                        debug!(
                            path = %state_path.display(),
                            error = %e,
                            "Skipping invalid state.yml"
                        );
                    }
                }
            }
        }

        features.sort_by(|a, b| a.feature.slug.cmp(&b.feature.slug));
        Ok(features)
    }

    /// Returns the state for a specific feature identified by its slug.
    ///
    /// Tries a direct lookup at `.trees/<slug>/.coda/<slug>/state.yml` first,
    /// then falls back to scanning all worktrees.
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

        // Direct lookup
        let direct_state = self
            .trees_dir
            .join(feature_slug)
            .join(".coda")
            .join(feature_slug)
            .join("state.yml");

        if direct_state.is_file() {
            return Self::read_state(&direct_state);
        }

        // Fallback: scan all worktrees
        let mut available = Vec::new();

        for worktree_entry in fs::read_dir(&self.trees_dir)?.flatten() {
            if !worktree_entry.file_type().is_ok_and(|ft| ft.is_dir()) {
                continue;
            }

            let candidate = worktree_entry
                .path()
                .join(".coda")
                .join(feature_slug)
                .join("state.yml");

            if candidate.is_file() {
                return Self::read_state(&candidate);
            }

            available.push(worktree_entry.file_name().to_string_lossy().to_string());
        }

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
        serde_yaml::from_str(&content).map_err(|e| {
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
            ],
            pr: None,
            total: TotalStats::default(),
        }
    }

    fn write_state(root: &Path, slug: &str, state: &FeatureState) {
        let dir = root.join(".trees").join(slug).join(".coda").join(slug);
        fs::create_dir_all(&dir).expect("create state dir");
        let yaml = serde_yaml::to_string(state).expect("serialize state");
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
        write_state(tmp.path(), "zzz", &make_state("zzz"));
        write_state(tmp.path(), "aaa", &make_state("aaa"));
        let scanner = FeatureScanner::new(tmp.path());

        let features = scanner.list().expect("list");
        assert_eq!(features.len(), 2);
        assert_eq!(features[0].feature.slug, "aaa");
        assert_eq!(features[1].feature.slug, "zzz");
    }

    #[test]
    fn test_should_get_feature_by_slug() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_state(tmp.path(), "my-feat", &make_state("my-feat"));
        let scanner = FeatureScanner::new(tmp.path());

        let state = scanner.get("my-feat").expect("get");
        assert_eq!(state.feature.slug, "my-feat");
    }

    #[test]
    fn test_should_error_when_feature_not_found() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_state(tmp.path(), "existing", &make_state("existing"));
        let scanner = FeatureScanner::new(tmp.path());

        let err = scanner.get("missing").unwrap_err().to_string();
        assert!(err.contains("missing"));
        assert!(err.contains("existing"));
    }

    #[test]
    fn test_should_error_when_no_trees_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let scanner = FeatureScanner::new(tmp.path());
        assert!(scanner.list().is_err());
        assert!(scanner.get("any").is_err());
    }

    #[test]
    fn test_should_skip_invalid_state_files() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_state(tmp.path(), "good", &make_state("good"));
        let bad_dir = tmp.path().join(".trees/bad/.coda/bad");
        fs::create_dir_all(&bad_dir).expect("mkdir");
        fs::write(bad_dir.join("state.yml"), "not: valid: yaml: [").expect("write");
        let scanner = FeatureScanner::new(tmp.path());

        let features = scanner.list().expect("list");
        assert_eq!(features.len(), 1);
        assert_eq!(features[0].feature.slug, "good");
    }
}
