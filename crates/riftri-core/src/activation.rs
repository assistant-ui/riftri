use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use riftri_git::{Git, GitError};
use serde::Serialize;
use thiserror::Error;

pub const ENABLED_CONFIG_KEY: &str = "riftri.enabled";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepositoryActivation {
    pub repository: PathBuf,
    pub common_git_dir: PathBuf,
    pub enabled: bool,
}

#[derive(Debug, Error)]
pub enum ActivationError {
    #[error(transparent)]
    Git(#[from] GitError),

    #[error("repository-scoped activation requires a non-bare Git worktree")]
    BareRepository,
}

pub fn enable_repository(path: &Path) -> Result<RepositoryActivation, ActivationError> {
    let git = Git::default();
    let status = repository_identity_with_git(&git, path)?;
    git.set_local_config(&status.repository, ENABLED_CONFIG_KEY, OsStr::new("true"))?;
    Ok(RepositoryActivation {
        enabled: true,
        ..status
    })
}

pub fn disable_repository(path: &Path) -> Result<RepositoryActivation, ActivationError> {
    let git = Git::default();
    let status = repository_identity_with_git(&git, path)?;
    git.unset_local_config(&status.repository, ENABLED_CONFIG_KEY)?;
    Ok(RepositoryActivation {
        enabled: false,
        ..status
    })
}

pub fn repository_activation(path: &Path) -> Result<RepositoryActivation, ActivationError> {
    activation_with_git(&Git::default(), path)
}

fn activation_with_git(git: &Git, path: &Path) -> Result<RepositoryActivation, ActivationError> {
    let mut activation = repository_identity_with_git(git, path)?;
    activation.enabled = git
        .local_config_bool(&activation.repository, ENABLED_CONFIG_KEY)?
        .unwrap_or(false);
    Ok(activation)
}

fn repository_identity_with_git(
    git: &Git,
    path: &Path,
) -> Result<RepositoryActivation, ActivationError> {
    let repository = git.inspect_repository(path)?;
    if repository.is_bare {
        return Err(ActivationError::BareRepository);
    }
    let root = repository.root.ok_or(ActivationError::BareRepository)?;
    Ok(RepositoryActivation {
        repository: root,
        common_git_dir: repository.identity.common_git_dir,
        enabled: false,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    use tempfile::tempdir;

    use super::{disable_repository, enable_repository, repository_activation};

    #[test]
    fn repository_enable_is_local_and_reversible() {
        let fixture = repository_fixture();

        let enabled = enable_repository(fixture.path()).expect("enable repository");
        assert!(enabled.enabled);
        assert!(
            repository_activation(fixture.path())
                .expect("read activation")
                .enabled
        );

        let disabled = disable_repository(fixture.path()).expect("disable repository");
        assert!(!disabled.enabled);
        assert!(
            !repository_activation(fixture.path())
                .expect("read activation")
                .enabled
        );
    }

    fn repository_fixture() -> tempfile::TempDir {
        let directory = tempdir().expect("temporary repository");
        git(directory.path(), &["init", "--quiet"]);
        git(directory.path(), &["config", "user.name", "Riftri Tests"]);
        git(
            directory.path(),
            &["config", "user.email", "riftri@example.invalid"],
        );
        git(directory.path(), &["config", "core.autocrlf", "false"]);
        fs::write(directory.path().join("tracked.txt"), "tracked\n").expect("write fixture");
        git(directory.path(), &["add", "--", "tracked.txt"]);
        git(directory.path(), &["commit", "--quiet", "-m", "initial"]);
        directory
    }

    fn git(path: &Path, arguments: &[&str]) {
        let status = Command::new("git")
            .args(arguments)
            .current_dir(path)
            .status()
            .expect("run Git fixture command");
        assert!(status.success(), "git {arguments:?} failed");
    }
}
