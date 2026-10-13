//! Property tests for how the Git proxy reads a `git` command line.
//!
//! The proxy decides whether an invocation is a worktree lifecycle command by
//! parsing the same argv Git parses. Anywhere the two parses disagree, Git
//! runs something the proxy did not classify, so real Git is the oracle here
//! wherever one is cheap: alias bodies, alias chains, and the options in front
//! of a command are compared against what Git actually executes.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use hegel::generators as gs;
use hegel::{Hegel, Settings, TestCase};
use tempfile::tempdir;

use super::{
    COMMON_GIT_BUILTINS, GlobalOptionScope, command_contexts, expand_worktree_alias,
    record_invocation_alias,
};

/// Every case that consults Git spawns it at least once.
pub(super) const GIT_CASES: u64 = 100;

/// The stand-in for `worktree` on the Git side of an alias comparison: an
/// external command that reports the argv Git handed it.
const ECHO: &str = "riftri-echo";

pub(super) fn os(value: &str) -> OsString {
    OsString::from(value)
}

pub(super) fn real_git(directory: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(directory)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    command
}

pub(super) fn run(command: &mut Command) -> Output {
    command.output().expect("start Git")
}

pub(super) fn stdout_line(command: &mut Command) -> Option<String> {
    let output = run(command);
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// Real Git with `git riftri-echo` on its `PATH`.
struct GitOracle {
    directory: tempfile::TempDir,
}

impl GitOracle {
    fn new() -> Self {
        let directory = tempdir().expect("oracle fixture");
        let script = directory.path().join(format!("git-{ECHO}"));
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '{ECHO}\\0'\nfor argument in \"$@\"; do printf '%s\\0' \"$argument\"; done\n"
            ),
        )
        .expect("write echo command");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("mark executable");
        Self { directory }
    }

    /// The argv `git riftri-echo` received when Git ran `arguments`, or
    /// `None` when Git refused the command line or ran something else.
    fn echoed(&self, arguments: &[OsString]) -> Option<Vec<OsString>> {
        let path =
            std::env::join_paths(std::iter::once(self.directory.path().to_path_buf()).chain(
                std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
            ))
            .expect("oracle PATH");
        let output = run(real_git(self.directory.path())
            .args(arguments)
            .env("PATH", path));
        let reported = output.stdout.strip_prefix(format!("{ECHO}\0").as_bytes())?;
        output.status.success().then(|| {
            reported
                .split(|byte| *byte == 0)
                .take(reported.iter().filter(|byte| **byte == 0).count())
                .map(|word| OsString::from_vec(word.to_vec()))
                .collect()
        })
    }

    /// The arguments the proxy believes follow `worktree` once it has
    /// expanded the same command line with `worktree` in place of the echo.
    fn planned(&self, arguments: &[OsString]) -> Option<Vec<OsString>> {
        let arguments = arguments
            .iter()
            .map(|argument| {
                os(&argument
                    .to_str()
                    .expect("generated arguments are UTF-8")
                    .replace(ECHO, "worktree"))
            })
            .collect::<Vec<_>>();
        let context = command_contexts(self.directory.path(), &arguments)
            .into_iter()
            .next()?;
        if arguments[context.command_index] == "worktree" {
            return Some(arguments[context.command_index + 1..].to_vec());
        }
        let expanded = expand_worktree_alias(&context, &arguments).ok()??;
        assert_eq!(expanded[context.command_index], "worktree");
        Some(expanded[context.command_index + 1..].to_vec())
    }

    fn assert_agreement(&self, arguments: &[OsString]) {
        assert_eq!(
            self.planned(arguments),
            self.echoed(arguments),
            "the proxy (left) and Git (right) disagree about {arguments:?}"
        );
    }
}

/// Characters Git's `split_cmdline` treats specially, plus two ordinary ones.
const SPLIT_ALPHABET: &str = "ab'\"\\ \t\n";
/// Whitespace that is not Git's: `isspace` there is ASCII space, tab, LF, CR.
const FOREIGN_WHITESPACE: &str = "\u{b}\u{c}\u{85}\u{a0}\u{2003}";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BodyWhitespace {
    /// What Git's configuration parser hands `split_cmdline` for a value
    /// read from a file: Git's own whitespace, and none at the end.
    Git,
    /// A `-c` value is used verbatim, so it can also end in whitespace.
    GitWithTrailing,
    /// Characters only Unicode calls whitespace, which Git treats as text.
    Foreign,
}

fn alias_body_agrees_with_git(tc: &TestCase, oracle: &GitOracle, whitespace: BodyWhitespace) {
    let alphabet = match whitespace {
        BodyWhitespace::Foreign => format!("{SPLIT_ALPHABET}{FOREIGN_WHITESPACE}"),
        _ => SPLIT_ALPHABET.to_owned(),
    };
    let mut body = format!(
        "{ECHO} {}",
        tc.draw(gs::text().alphabet(&alphabet).max_size(10))
    );
    if whitespace != BodyWhitespace::GitWithTrailing {
        body.truncate(body.trim_end_matches([' ', '\t', '\n']).len());
    }
    oracle.assert_agreement(&[
        os("-c"),
        os(&format!("alias.split={body}")),
        os("split"),
        os("end"),
    ]);
}

fn alias_bodies_agree_with_git(whitespace: BodyWhitespace) {
    let oracle = GitOracle::new();
    Hegel::new(|tc: TestCase| alias_body_agrees_with_git(&tc, &oracle, whitespace))
        .settings(Settings::new().test_cases(GIT_CASES))
        .run();
}

#[test]
fn alias_bodies_split_as_git_splits_them() {
    alias_bodies_agree_with_git(BodyWhitespace::Git);
}

#[test]
fn alias_bodies_with_unicode_whitespace_split_as_git_splits_them() {
    alias_bodies_agree_with_git(BodyWhitespace::Foreign);
}

#[test]
fn alias_bodies_with_trailing_whitespace_split_as_git_splits_them() {
    alias_bodies_agree_with_git(BodyWhitespace::GitWithTrailing);
}

/// A chain of up to `longest` aliases `n0 -> n1 -> ... -> riftri-echo`,
/// then a few redefinitions that cut, loop, or redirect it (Git resolves
/// repeated definitions last-one-wins), then the name Git is asked to run.
fn alias_chain(tc: &TestCase, longest: usize) -> Vec<OsString> {
    let length = tc.draw(gs::integers::<usize>().min_value(1).max_value(longest));
    let redefinitions = tc.draw(gs::integers::<usize>().max_value(2));
    let mut arguments = Vec::new();
    for definition in 0..length + redefinitions {
        let index = if definition < length {
            definition
        } else {
            tc.draw(gs::integers::<usize>().max_value(length - 1))
        };
        let first = if definition >= length {
            match tc.draw(gs::integers::<u8>().max_value(2)) {
                0 => ECHO.to_owned(),
                1 => "version".to_owned(),
                _ => format!("n{}", tc.draw(gs::integers::<usize>().max_value(length))),
            }
        } else if index + 1 == length {
            ECHO.to_owned()
        } else {
            format!("n{}", index + 1)
        };
        let words = tc.draw(gs::vecs(gs::sampled_from(vec!["a", "b c", "-x", "--"])).max_size(2));
        let mut body = first;
        for word in words {
            body.push_str(&format!(" '{word}'"));
        }
        let name = if tc.draw(gs::booleans()) { "N" } else { "n" };
        arguments.extend([os("-c"), os(&format!("alias.{name}{index}={body}"))]);
    }
    let name = if tc.draw(gs::booleans()) { "N" } else { "n" };
    arguments.extend([os(&format!("{name}0")), os("end")]);
    arguments
}

#[test]
fn alias_chains_expand_as_git_expands_them() {
    let oracle = GitOracle::new();
    Hegel::new(|tc: TestCase| oracle.assert_agreement(&alias_chain(&tc, 6)))
        .settings(Settings::new().test_cases(GIT_CASES))
        .run();
}

#[test]
fn long_alias_chains_expand_as_git_expands_them() {
    let oracle = GitOracle::new();
    Hegel::new(|tc: TestCase| oracle.assert_agreement(&alias_chain(&tc, 40)))
        .settings(Settings::new().test_cases(GIT_CASES))
        .run();
}

/// Global options Git accepts in front of any command, outside a repository
/// too. `separate_unknown_values` adds the spaced form of options the proxy
/// has no rule for, whose value it cannot tell from the command.
fn harmless_global_options(tc: &TestCase, separate_unknown_values: bool) -> Vec<OsString> {
    let mut choices = vec![
        vec!["--no-pager"],
        vec!["-P"],
        vec!["--no-replace-objects"],
        vec!["--no-lazy-fetch"],
        vec!["--no-optional-locks"],
        vec!["--no-advice"],
        vec!["--literal-pathspecs"],
        vec!["--icase-pathspecs"],
        vec!["--namespace=name"],
        vec!["--namespace", "name"],
        vec!["--namespace", ECHO],
        vec!["--attr-source=HEAD"],
        vec!["-c", "core.quotePath=false"],
        vec!["--config-env=core.quotePath=HOME"],
        vec!["-C", "."],
    ];
    if separate_unknown_values {
        choices.extend([
            vec!["--attr-source", "HEAD"],
            vec!["--attr-source", ECHO],
            vec!["--config-env", "core.quotePath=HOME"],
        ]);
    }
    tc.draw(gs::vecs(gs::sampled_from(choices)).max_size(3))
        .into_iter()
        .flatten()
        .map(os)
        .collect()
}

/// Whatever Git skips on its way to the command, the proxy skips too: when
/// Git ends up running the command or alias, the proxy has classified that
/// same command with those same arguments.
fn commands_after_global_options_agree_with_git(separate_unknown_values: bool) {
    let oracle = GitOracle::new();
    Hegel::new(|tc: TestCase| {
        let mut arguments = harmless_global_options(&tc, separate_unknown_values);
        if tc.draw(gs::booleans()) {
            arguments.extend([ECHO, "a"].map(os));
        } else {
            arguments.extend(["-c", "alias.run=riftri-echo a", "run"].map(os));
        }
        arguments.push(os("end"));
        let Some(echoed) = oracle.echoed(&arguments) else {
            // Git refused the options themselves, so nothing ran.
            return;
        };
        assert_eq!(
            oracle.planned(&arguments),
            Some(echoed),
            "Git (right) ran a command the proxy (left) read differently: {arguments:?}"
        );
    })
    .settings(Settings::new().test_cases(GIT_CASES))
    .run();
}

#[test]
fn commands_after_global_options_are_found_where_git_finds_them() {
    commands_after_global_options_agree_with_git(false);
}

#[test]
fn commands_after_spaced_global_option_values_are_found_where_git_finds_them() {
    commands_after_global_options_agree_with_git(true);
}

/// Git never lets an alias replace one of its own commands, so an alias named
/// after a builtin must leave that builtin's command line alone. A Git that
/// lists deprecated builtins lets an alias replace them, so those are left
/// out; an older Git refuses the `deprecated` listing.
#[test]
fn aliases_never_shadow_git_builtins() {
    let oracle = GitOracle::new();
    let deprecated = stdout_line(real_git(oracle.directory.path()).arg("--list-cmds=deprecated"))
        .unwrap_or_default();
    let deprecated = deprecated.lines().collect::<Vec<_>>();
    let builtins = stdout_line(real_git(oracle.directory.path()).arg("--list-cmds=builtins"))
        .expect("list Git builtins")
        .lines()
        .filter(|builtin| !deprecated.contains(builtin))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert!(builtins.iter().any(|builtin| builtin == "worktree"));
    Hegel::new(|tc: TestCase| {
        let builtin = tc.draw(gs::sampled_from(builtins.clone()));
        tc.assume(builtin != "worktree");
        let arguments = [
            os("-c"),
            os(&format!("alias.{builtin}=worktree prune")),
            os(&builtin),
        ];
        let context = command_contexts(oracle.directory.path(), &arguments)
            .into_iter()
            .next()
            .expect("context");
        assert_eq!(
            expand_worktree_alias(&context, &arguments).expect("expand"),
            None,
            "`git {builtin}` was read as `git worktree prune`"
        );
    })
    .settings(Settings::new().test_cases(GIT_CASES))
    .run();
}

#[hegel::test]
fn aliases_never_shadow_the_builtins_the_proxy_knows(tc: TestCase) {
    let builtin = tc.draw(gs::sampled_from(COMMON_GIT_BUILTINS.to_vec()));
    tc.assume(builtin != "worktree");
    let arguments = [
        os("-c"),
        os(&format!("alias.{builtin}=worktree prune")),
        os(builtin),
        os("end"),
    ];
    let context = command_contexts(Path::new("."), &arguments)
        .into_iter()
        .next()
        .expect("context");
    assert_eq!(
        expand_worktree_alias(&context, &arguments).expect("expand"),
        None
    );
}

/// `-c` and `--config-env` assignments define an alias only when Git would
/// use them: an `alias.<name>` key in any case, with a name and a body.
#[hegel::test]
fn invocation_aliases_are_recorded_only_for_usable_assignments(tc: TestCase) {
    let section = tc.draw(gs::sampled_from(vec![
        "alias.", "ALIAS.", "Alias.", "alias", "core.", "",
    ]));
    let name = tc.draw(gs::text().alphabet("aB.-\u{e9}").max_size(4));
    let body = tc.draw(gs::optional(gs::text().alphabet("w =!\u{e9}").max_size(6)));
    let mut assignment = format!("{section}{name}").into_bytes();
    if let Some(body) = &body {
        assignment.push(b'=');
        assignment.extend_from_slice(body.as_bytes());
    }
    // Git runs an alias body whether or not it is UTF-8, so a trailing byte
    // that is not UTF-8 stays in the recorded body. Without an `=` it lands in
    // the key instead, and the assignment defines nothing.
    let invalid = tc.draw(gs::integers::<u8>().max_value(9)) == 0;
    if invalid {
        assignment.push(0xff);
    }

    let mut recorded = Vec::new();
    record_invocation_alias(&mut recorded, OsStr::from_bytes(&assignment));
    // The key is whatever precedes the first `=`, however it was assembled.
    let key = format!("{section}{name}");
    let alias = key
        .get(..6)
        .filter(|prefix| prefix.eq_ignore_ascii_case("alias."))
        .map(|_| &key[6..])
        .filter(|alias| !alias.is_empty());
    let body = body
        .map(|body| {
            let mut body = body.into_bytes();
            if invalid {
                body.push(0xff);
            }
            body
        })
        .filter(|body| !body.is_empty());
    let expected = alias
        .zip(body)
        .map(|(alias, body)| (alias.to_ascii_lowercase(), body));
    assert_eq!(recorded, expected.into_iter().collect::<Vec<_>>());
}

/// One global option as Git spells it, with what the proxy must learn from it.
#[derive(Debug, Clone)]
pub(super) struct GlobalOption {
    pub words: Vec<OsString>,
    pub scope: GlobalOptionScope,
    pub change_directory: Option<PathBuf>,
    pub git_directory: Option<PathBuf>,
    pub work_tree: Option<PathBuf>,
}

pub(super) fn global_option(tc: &TestCase) -> GlobalOption {
    let plain = |words: &[&str], scope| GlobalOption {
        words: words.iter().map(|word| os(word)).collect(),
        scope,
        change_directory: None,
        git_directory: None,
        work_tree: None,
    };
    let path = || {
        let mut bytes = tc.draw(gs::binary().min_size(1).max_size(6));
        bytes.retain(|byte| *byte != 0);
        if tc.draw(gs::booleans()) {
            bytes.insert(0, b'/');
        }
        PathBuf::from(OsString::from_vec(bytes))
    };
    let joined = |prefix: &str, path: &Path| {
        let mut word = os(prefix);
        word.push(path);
        word
    };
    match tc.draw(gs::integers::<u8>().max_value(9)) {
        0 => {
            let path = path();
            GlobalOption {
                words: vec![os("-C"), path.clone().into_os_string()],
                change_directory: Some(path),
                ..plain(&[], GlobalOptionScope::None)
            }
        }
        1 => {
            let path = path();
            let words = if tc.draw(gs::booleans()) {
                vec![os("--git-dir"), path.clone().into_os_string()]
            } else {
                vec![joined("--git-dir=", &path)]
            };
            GlobalOption {
                words,
                git_directory: Some(path),
                ..plain(&[], GlobalOptionScope::Significant)
            }
        }
        2 => {
            let path = path();
            let words = if tc.draw(gs::booleans()) {
                vec![os("--work-tree"), path.clone().into_os_string()]
            } else {
                vec![joined("--work-tree=", &path)]
            };
            GlobalOption {
                words,
                work_tree: Some(path),
                ..plain(&[], GlobalOptionScope::Significant)
            }
        }
        3 => plain(
            &[tc.draw(gs::sampled_from(vec![
                "--no-pager",
                "--paginate",
                "-p",
                "-P",
            ]))],
            GlobalOptionScope::None,
        ),
        4 => plain(
            &[tc.draw(gs::sampled_from(vec![
                "--no-optional-locks",
                "--no-advice",
                "--literal-pathspecs",
            ]))],
            GlobalOptionScope::CheckoutNeutral,
        ),
        5 => plain(
            &["-c", "core.quotePath=false"],
            GlobalOptionScope::Significant,
        ),
        6 => plain(&["--namespace", "name"], GlobalOptionScope::Significant),
        _ => plain(
            &[tc.draw(gs::sampled_from(vec![
                "--namespace=name",
                "--exec-path=/usr/libexec/git-core",
                "--no-replace-objects",
                "--no-lazy-fetch",
                "--glob-pathspecs",
                "--noglob-pathspecs",
                "--icase-pathspecs",
                "--bare",
                "--config-env=core.quotePath=HOME",
                "--attr-source=HEAD",
            ]))],
            GlobalOptionScope::Significant,
        ),
    }
}

/// Git applies global options left to right: each `-C` is relative to the
/// one before it, `--git-dir` is relative to the final directory and the last
/// one wins, and `--work-tree` moves the checked-out files but never the
/// repository. The command is the first word that is not one of them or its
/// value.
#[hegel::test]
fn global_options_are_read_the_way_git_applies_them(tc: TestCase) {
    let current = PathBuf::from("/current");
    let options = tc.draw(gs::integers::<usize>().max_value(5));
    let options = (0..options).map(|_| global_option(&tc)).collect::<Vec<_>>();
    let mut arguments = options
        .iter()
        .flat_map(|option| option.words.clone())
        .collect::<Vec<_>>();
    let command_index = arguments.len();
    let has_command = tc.draw(gs::integers::<u8>().max_value(9)) > 0;
    if has_command {
        arguments.extend([os("worktree"), os("list")]);
    }

    let Some(context) = command_contexts(&current, &arguments).into_iter().next() else {
        assert!(!has_command, "no command was found in {arguments:?}");
        return;
    };
    assert!(has_command, "a command was found in {arguments:?}");
    assert_eq!(context.command_index, command_index);

    let directory = options
        .iter()
        .filter_map(|option| option.change_directory.as_ref())
        .fold(current, |directory, next| directory.join(next));
    let last = |select: fn(&GlobalOption) -> Option<&PathBuf>| {
        options
            .iter()
            .filter_map(select)
            .next_back()
            .map(|path| directory.join(path))
    };
    assert_eq!(
        context.git_directory,
        last(|option| option.git_directory.as_ref())
    );
    assert_eq!(context.repository, directory);

    let scopes = options.iter().map(|option| option.scope);
    let expected = if scopes
        .clone()
        .any(|scope| scope == GlobalOptionScope::Significant)
    {
        GlobalOptionScope::Significant
    } else if scopes
        .clone()
        .any(|scope| scope == GlobalOptionScope::CheckoutNeutral)
    {
        GlobalOptionScope::CheckoutNeutral
    } else {
        GlobalOptionScope::None
    };
    assert_eq!(context.global_options, expected);
    assert_eq!(
        context.global_options.optimization_compatible(),
        expected == GlobalOptionScope::None
    );
}
