use std::path::Path;
use std::process::Command;

pub(crate) fn isolated_git(repository: &Path) -> Command {
    // Use the same Git defaults as Cargo when a test binary runs directly.
    let config_home = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cargo/test-config");
    let mut command = Command::new("git");
    command
        .current_dir(repository)
        .env("GIT_CONFIG_GLOBAL", config_home.join("git/config"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("XDG_CONFIG_HOME", config_home);
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
    ] {
        command.env_remove(name);
    }
    command
}
