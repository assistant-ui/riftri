//! Commands whose output other programs and shells consume: ownership
//! inspection, shell activation code, man pages, and hook exit codes.

use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;

use hegel::TestCase;
use hegel::generators as gs;
use serde_json::Value;

use crate::common::{Scratch, copy_on_write_supported, git_ok, riftri};
use crate::contract::{
    check_report_value, decode_hex, native_path, path_from_bytes, process_cases,
};

fn stdout_of(command: &mut std::process::Command) -> String {
    let output = command.output().expect("run riftri");
    assert!(
        output.status.success(),
        "{command:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("UTF-8 output")
}

fn report_of(command: &mut std::process::Command) -> Value {
    let report: Value = serde_json::from_str(&stdout_of(command)).expect("one JSON document");
    check_report_value(&report);
    report
}

#[test]
fn inspection_answers_for_exactly_the_paths_asked_in_order() {
    let scratch = Scratch::new();
    let repository = scratch.repository("repository");
    let managed = scratch.root().join("managed view");
    let supported = copy_on_write_supported(&repository);
    if supported {
        stdout_of(
            riftri()
                .args(["worktree", "add", "--detach"])
                .arg(&managed)
                .arg("--repository")
                .arg(&repository),
        );
    }
    let state = repository.join(".git/riftri");

    hegel::Hegel::new(|tc: TestCase| {
        let count = tc.draw(gs::integers::<usize>().min_value(1).max_value(4));
        let paths = (0..count)
            .map(|_| match tc.draw(gs::integers::<u8>().max_value(2)) {
                0 if supported => managed.clone(),
                1 => repository.clone(),
                // Riftri looks a path up without requiring it to exist.
                _ => scratch.root().join(native_path(&tc)),
            })
            .collect::<Vec<_>>();
        let is_managed = |path: &Path| supported && path == managed;
        let inspect = || {
            let mut command = riftri();
            command
                .current_dir(&repository)
                .args(["worktree", "inspect"]);
            command
        };

        let report = report_of(inspect().arg("--json").arg("--").args(&paths));
        let human = stdout_of(inspect().arg("--").args(&paths));

        let entries = report["worktrees"].as_array().expect("worktrees");
        assert_eq!(entries.len(), paths.len());
        for (entry, path) in entries.iter().zip(&paths) {
            let reported = decode_hex(entry["path_native_hex"].as_str().expect("path hex"));
            assert_eq!(&path_from_bytes(reported), path);
            let owner = entry["state_directory_native_hex"]
                .as_str()
                .map(|hex| path_from_bytes(decode_hex(hex)));
            assert_eq!(owner, is_managed(path).then(|| state.clone()), "{entry}");
            assert_eq!(entry["backend"].is_null(), !is_managed(path));
        }
        // The text form says the same thing, one block per requested path.
        let mut blocks = human.split("Worktree: ").skip(1);
        for path in &paths {
            let block = blocks.next().expect("one block per path");
            assert!(
                block.starts_with(&format!("{}\n", path.display())),
                "{human}"
            );
            assert_eq!(block.contains("Unmanaged by Riftri\n"), !is_managed(path));
            assert_eq!(
                block.contains(&format!("State: {}\n", state.display())),
                is_managed(path)
            );
        }
        assert!(blocks.next().is_none(), "{human}");

        // `owner` answers the same question for a single path.
        let path = &paths[0];
        let owner = || {
            let mut command = riftri();
            command.current_dir(&repository).args(["worktree", "owner"]);
            command
        };
        let report = report_of(owner().arg("--json").arg("--").arg(path));
        assert_eq!(report["state_directory"].is_null(), !is_managed(path));
        let human = stdout_of(owner().arg("--").arg(path));
        assert_eq!(
            human,
            if is_managed(path) {
                format!("State: {}\n", state.display())
            } else {
                "No Riftri state claims this worktree.\n".to_owned()
            }
        );
    })
    .settings(process_cases(10))
    .run();
}

/// A directory name that tests shell quoting without being rejected by the
/// filesystem.
fn awkward_name(tc: &TestCase) -> &'static str {
    tc.draw(gs::sampled_from(vec![
        "plain",
        "with spaces",
        "it's quoted",
        "\"double\"",
        "$HOME",
        "`id`",
        "semi;colon",
        "back\\slash",
        "star*",
        "-dash",
    ]))
}

#[hegel::test(process_cases(10))]
fn shell_activation_code_survives_any_cache_directory_name(tc: TestCase) {
    let scratch = Scratch::new();
    let cache = scratch.root().join(awkward_name(&tc));
    let shell = tc.draw(gs::sampled_from(vec!["sh", "bash", "zsh"]));
    let render = |action: &str| {
        let mut command = riftri();
        command
            .args(["shell", action, shell])
            .env("RIFTRI_CACHE_DIR", &cache);
        stdout_of(&mut command)
    };

    let hook = render("hook");
    let deactivation = render("deactivate");

    // Rendering installs the shim once and is repeatable byte for byte.
    assert_eq!(render("hook"), hook);
    let shim = cache.join("shims/v1");
    assert!(shim.join("git").exists());
    // A POSIX shell that evaluates the code ends up with exactly this shim
    // first on PATH, and deactivating restores what it started with.
    let script = r#"original=$PATH
eval "$RIFTRI_TEST_HOOK"
printf '%s\n%s\n%s\n' "$RIFTRI_SHELL_SHIM_DIR" "${PATH%%:*}" "$RIFTRI_SHIM_ACTIVE"
eval "$RIFTRI_TEST_DEACTIVATION"
[ "$PATH" = "$original" ] && [ "${RIFTRI_SHIM_ACTIVE-unset}" = unset ] && printf 'restored\n'
"#;
    let evaluated = stdout_of(
        std::process::Command::new("sh")
            .args(["-c", script])
            .env("RIFTRI_TEST_HOOK", &hook)
            .env("RIFTRI_TEST_DEACTIVATION", &deactivation)
            .env_remove("RIFTRI_SHIM_ACTIVE")
            .env_remove("RIFTRI_SHELL_SHIM_DIR"),
    );
    let shim = shim.to_str().expect("fixture path");
    assert_eq!(evaluated, format!("{shim}\n{shim}\n1\nrestored\n"));
}

/// One generated environment for `riftri shell status`.
struct Activation {
    shim_installed: bool,
    marker: bool,
    shim_first: bool,
    /// `RIFTRI_REAL_GIT`: unset, the real Git, or a path with no executable.
    real: &'static str,
    bypass: bool,
    /// The repository argument: enabled, disabled, or not a repository.
    target: &'static str,
}

/// Run `shell status` in `activation`; returns the report, the shim
/// directory, and the path that was inspected.
fn shell_status(
    scratch: &Scratch,
    activation: &Activation,
) -> (String, std::path::PathBuf, std::path::PathBuf) {
    let cache = scratch.root().join("cache");
    let shim = cache.join("shims/v1");
    let repository = scratch.repository("repository");
    if activation.shim_installed {
        let mut hook = riftri();
        hook.args(["shell", "hook", "sh"])
            .env("RIFTRI_CACHE_DIR", &cache);
        stdout_of(&mut hook);
    }
    if activation.target == "enabled" {
        stdout_of(riftri().arg("enable").arg(&repository));
    }
    let selected = if activation.target == "no repository" {
        scratch.root().join("not a repository")
    } else {
        repository
    };

    let mut command = riftri();
    command
        .args(["shell", "status"])
        .arg(&selected)
        .env("RIFTRI_CACHE_DIR", &cache);
    let mut path =
        std::env::split_paths(&std::env::var_os("PATH").expect("PATH")).collect::<Vec<_>>();
    if activation.shim_first {
        path.insert(0, shim.clone());
    }
    command.env("PATH", std::env::join_paths(path).expect("PATH"));
    if activation.marker {
        command.env("RIFTRI_SHIM_ACTIVE", "1");
    }
    match activation.real {
        "git" => {
            let located = std::process::Command::new("sh")
                .args(["-c", "command -v git"])
                .output()
                .expect("locate git");
            let located = String::from_utf8(located.stdout).expect("git path");
            command.env("RIFTRI_REAL_GIT", located.trim_end());
        }
        "missing" => {
            command.env("RIFTRI_REAL_GIT", scratch.root().join("no-git"));
        }
        _ => {}
    }
    if activation.bypass {
        command.env("RIFTRI_BYPASS", "1");
    }
    (stdout_of(&mut command), shim, selected)
}

fn activation(tc: &TestCase) -> Activation {
    Activation {
        shim_installed: tc.draw(gs::booleans()),
        marker: tc.draw(gs::booleans()),
        shim_first: tc.draw(gs::booleans()),
        real: tc.draw(gs::sampled_from(vec!["unset", "git", "missing"])),
        bypass: tc.draw(gs::booleans()),
        target: tc.draw(gs::sampled_from(vec![
            "enabled",
            "disabled",
            "no repository",
        ])),
    }
}

#[hegel::test(process_cases(16))]
fn shell_status_reports_activation_only_when_every_part_is_in_place(tc: TestCase) {
    let scratch = Scratch::new();
    let activation = activation(&tc);
    let Activation {
        shim_installed,
        marker,
        shim_first,
        real,
        bypass,
        target,
    } = activation;

    let (status, shim, selected) = shell_status(&scratch, &activation);

    let line = |text: String| status.contains(&format!("{text}\n"));
    let active = marker && shim_first && shim_installed && real == "git";
    assert!(
        line(format!(
            "Shell interception: {}",
            if active {
                "active"
            } else if marker || shim_first || real != "unset" {
                // Part of an activation is not an activation.
                "incomplete"
            } else {
                "inactive"
            }
        )),
        "{status}"
    );
    assert!(
        line(format!("Shim directory: {}", shim.display())),
        "{status}"
    );
    assert_eq!(status.contains("Real Git: "), real != "unset", "{status}");
    if target == "no repository" {
        assert!(line(format!("Repository: none at {}", selected.display())));
        assert!(line("Repository optimization: not applicable".to_owned()));
        assert!(line(
            "Effective optimized interception: inactive".to_owned()
        ));
        return;
    }
    if marker && real == "missing" {
        // See `shell_status_never_calls_an_uninspectable_repository_absent`.
        return;
    }
    assert!(
        line(format!("Repository optimization: {target}")),
        "{status}"
    );
    assert!(
        line(format!(
            "Effective optimized interception: {}",
            if bypass {
                "inactive (RIFTRI_BYPASS)"
            } else if active && target == "enabled" {
                "active"
            } else {
                "inactive"
            }
        )),
        "{status}"
    );
}

#[hegel::test(process_cases(6))]
fn shell_status_never_calls_an_uninspectable_repository_absent(tc: TestCase) {
    let scratch = Scratch::new();
    let activation = Activation {
        marker: true,
        real: "missing",
        target: tc.draw(gs::sampled_from(vec!["enabled", "disabled"])),
        ..activation(&tc)
    };

    let (status, _, selected) = shell_status(&scratch, &activation);

    assert!(
        !status.contains(&format!("Repository: none at {}", selected.display())),
        "{status}"
    );
    assert!(
        !status.contains("Repository optimization: not applicable"),
        "{status}"
    );
}

#[test]
fn enabling_suggests_the_hook_for_the_callers_shell() {
    let scratch = Scratch::new();
    let repository = scratch.repository("repository");

    hegel::Hegel::new(|tc: TestCase| {
        // `$SHELL` as a login shell sets it: some directory, then a name.
        // Several per case, since each is only one cheap invocation.
        let names = tc.draw(
            gs::vecs(gs::sampled_from(vec![
                &b"zsh"[..],
                b"bash",
                b"sh",
                b"fish",
                b"zsh-5.9",
                b"rbash",
                b"\xffzsh",
                b"",
            ]))
            .min_size(3)
            .max_size(6),
        );
        for name in names {
            let directory = tc.draw(gs::sampled_from(vec![
                None,
                Some(&b""[..]),
                Some(b"/bin/"),
                Some(b"/opt/my bash/"),
                Some(b"/zsh/\xfe/"),
            ]));
            let shell = directory.map(|directory| OsString::from_vec([directory, name].concat()));
            let mut command = riftri();
            command.arg("enable").arg(&repository).env_remove("SHELL");
            if let Some(shell) = &shell {
                command.env("SHELL", shell);
            }

            let enabled = stdout_of(&mut command);

            // Only an executable named exactly bash or zsh selects that
            // hook; anything else gets the portable one rather than a guess.
            let expected = match shell
                .as_deref()
                .and_then(|shell| Path::new(shell).file_name())
            {
                Some(name) if name.as_bytes() == b"bash" => "bash",
                Some(name) if name.as_bytes() == b"zsh" => "zsh",
                _ => "sh",
            };
            assert!(
                enabled.contains(&format!(
                    "Activate this shell with: eval \"$(riftri shell hook {expected})\"\n"
                )),
                "SHELL={shell:?}: {enabled}"
            );
        }
        assert_eq!(
            git_ok(
                &repository,
                &["config", "--local", "--get", "riftri.enabled"]
            ),
            "true\n"
        );
    })
    .settings(process_cases(10))
    .run();
}

#[hegel::test(process_cases(8))]
fn man_pages_are_written_wherever_a_directory_can_be_created(tc: TestCase) {
    let scratch = Scratch::new();
    let mut directory = scratch.root();
    for _ in 0..tc.draw(gs::integers::<usize>().min_value(1).max_value(3)) {
        directory.push(awkward_name(&tc));
    }
    let exists = tc.draw(gs::booleans());
    if exists {
        std::fs::create_dir_all(&directory).expect("create directory");
    }

    let written = stdout_of(riftri().arg("man").arg(&directory));

    assert_eq!(
        written,
        format!("Man pages written to {}\n", directory.display())
    );
    let mut pages = std::fs::read_dir(&directory)
        .expect("man directory")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .into_string()
                .expect("name")
        })
        .collect::<Vec<_>>();
    pages.sort();
    // One page for the program and one per command, named as `man` finds them.
    assert!(pages.contains(&"riftri.1".to_owned()), "{pages:?}");
    assert!(
        pages.contains(&"riftri-worktree-add.1".to_owned()),
        "{pages:?}"
    );
    assert!(
        pages
            .iter()
            .all(|page| page.starts_with("riftri") && page.ends_with(".1"))
    );
    for page in &pages {
        let troff = std::fs::read_to_string(directory.join(page)).expect("read page");
        assert!(troff.contains(".TH "), "{page}");
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use hegel::TestCase;
    use hegel::generators as gs;
    use serde_json::Value;

    use crate::common::{Scratch, copy_on_write_supported, git, git_ok, receipt, riftri};
    use crate::contract::{check_report_value, process_cases};

    #[hegel::test(process_cases(8))]
    fn commands_for_another_platform_refuse_without_output(tc: TestCase) {
        let (arguments, operation) = tc.draw(gs::sampled_from(vec![
            (&["shell", "hook", "powershell"][..], "shell"),
            (&["shell", "deactivate", "powershell"], "shell"),
            (&["overlayfs", "install-helper"], "overlayfs-helper-install"),
            (
                &["overlayfs", "install-helper", "--replace"],
                "overlayfs-helper-install",
            ),
        ]));
        let json_errors = tc.draw(gs::booleans());
        let scratch = Scratch::new();
        let cache = scratch.root().join("cache");
        let mut command = riftri();
        command.args(arguments).env("RIFTRI_CACHE_DIR", &cache);
        if json_errors {
            command.arg("--json-errors");
        }

        let output = command.output().expect("run riftri");

        // Nothing a shell could evaluate, and nothing installed.
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        assert!(!cache.exists());
        if json_errors {
            let receipt = receipt(&output);
            assert_eq!(receipt["operation"], operation);
            assert!(receipt["nextCommand"].is_null());
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(stderr.starts_with("Error: "), "{stderr}");
            assert!(stderr.contains(" only on "), "{stderr}");
        }
    }

    /// A repository whose post-checkout hook exits with `status`.
    fn hooked_repository(scratch: &Scratch, name: &str, status: u8) -> PathBuf {
        let repository = scratch.repository(name);
        let hook = repository.join(".git/hooks/post-checkout");
        std::fs::write(&hook, format!("#!/bin/sh\nexit {status}\n")).expect("write hook");
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))
            .expect("chmod hook");
        repository
    }

    #[hegel::test(process_cases(3))]
    fn a_post_checkout_hook_decides_the_exit_code_as_it_does_for_git(tc: TestCase) {
        let scratch = Scratch::new();
        let status = tc.draw(gs::integers::<u8>());
        let repository = hooked_repository(&scratch, "riftri", status);
        if !copy_on_write_supported(&repository) {
            return;
        }
        let twin = hooked_repository(&scratch, "git", status);
        let view = scratch.root().join("riftri view");
        let twin_view = scratch.root().join("git view");

        let real = git(&twin)
            .args(["worktree", "add", "--quiet", "--detach"])
            .arg(&twin_view)
            .output()
            .expect("run git");
        let output = riftri()
            .args(["worktree", "add", "--json", "--detach"])
            .arg(&view)
            .arg("--repository")
            .arg(&repository)
            .output()
            .expect("run riftri");

        // Git leaves the worktree in place and exits with the hook's status.
        assert_eq!(real.status.code(), Some(i32::from(status)));
        assert_eq!(output.status.code(), real.status.code());
        let report: Value = serde_json::from_slice(&output.stdout).expect("add report");
        check_report_value(&report);
        assert_eq!(report["post_checkout"]["exit_code"], status);
        assert_eq!(report["post_checkout"]["started"], true);
        for (repository, view) in [(&repository, &view), (&twin, &twin_view)] {
            assert_eq!(git_ok(view, &["status", "--porcelain=v1"]), "");
            assert!(
                git_ok(repository, &["worktree", "list", "--porcelain"])
                    .contains(&format!("worktree {}\n", view.display()))
            );
        }
    }
}
