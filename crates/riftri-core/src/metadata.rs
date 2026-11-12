//! Free-form labels a caller attaches to a worktree when it creates one.
//!
//! Metadata answers questions like "which agent asked for this worktree"
//! without making Riftri agent-specific: keys and values are the caller's own.
//! It is reporting only. It never selects a base, changes a checkout, or
//! authorizes a lifecycle operation, and recovery never depends on it.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};

use crate::worktree::WorktreeError;

/// Environment variable `riftri exec --meta` sets for its command. Every
/// worktree that command's process tree creates through Riftri, whether
/// through the Git shim or an explicit `riftri worktree add`, carries these
/// entries. Its value is a JSON object of string keys to string values.
pub const WORKTREE_METADATA_ENV: &str = "RIFTRI_WORKTREE_METADATA";

/// Most entries one worktree can carry.
pub const MAX_METADATA_ENTRIES: usize = 32;
/// Longest key, in bytes.
pub const MAX_METADATA_KEY_BYTES: usize = 64;
/// Longest value, in bytes.
pub const MAX_METADATA_VALUE_BYTES: usize = 1024;

/// Validated metadata for one worktree, ordered by key.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorktreeMetadata(BTreeMap<String, String>);

impl WorktreeMetadata {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }

    pub fn as_map(&self) -> &BTreeMap<String, String> {
        &self.0
    }

    /// Parse `KEY=VALUE` arguments, as `--meta` receives them. A key given
    /// twice is refused rather than silently keeping one of the values.
    pub fn parse_entries<I, S>(entries: I) -> Result<Self, WorktreeError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut metadata = Self::new();
        for entry in entries {
            let entry = entry.as_ref();
            let text = entry.to_str().ok_or_else(|| {
                invalid(format!(
                    "metadata entry {} is not valid UTF-8",
                    entry.to_string_lossy()
                ))
            })?;
            let (key, value) = text.split_once('=').ok_or_else(|| {
                invalid(format!(
                    "metadata entry `{}` must have the form KEY=VALUE",
                    text.escape_debug()
                ))
            })?;
            if metadata.0.contains_key(key) {
                return Err(invalid(format!(
                    "metadata key `{}` was given more than once",
                    key.escape_debug()
                )));
            }
            metadata.insert(key, value)?;
        }
        Ok(metadata)
    }

    /// Parse the JSON object `riftri exec --meta` puts in
    /// [`WORKTREE_METADATA_ENV`].
    pub fn from_environment_value(value: &OsStr) -> Result<Self, WorktreeError> {
        let text = value
            .to_str()
            .ok_or_else(|| invalid(format!("{WORKTREE_METADATA_ENV} is not valid UTF-8")))?;
        if text.trim().is_empty() {
            return Ok(Self::new());
        }
        let entries: BTreeMap<String, String> = serde_json::from_str(text).map_err(|error| {
            invalid(format!(
                "{WORKTREE_METADATA_ENV} must be a JSON object of string values: {error}"
            ))
        })?;
        let mut metadata = Self::new();
        for (key, value) in entries {
            metadata.insert(&key, &value).map_err(|error| match error {
                WorktreeError::InvalidRequest(detail) => {
                    invalid(format!("{WORKTREE_METADATA_ENV}: {detail}"))
                }
                other => other,
            })?;
        }
        Ok(metadata)
    }

    /// The metadata inherited from [`WORKTREE_METADATA_ENV`], or none when
    /// the variable is unset or empty. A value that is set but invalid is
    /// refused: the caller asked for labels it would not get.
    pub fn from_environment() -> Result<Self, WorktreeError> {
        match std::env::var_os(WORKTREE_METADATA_ENV) {
            Some(value) => Self::from_environment_value(&value),
            None => Ok(Self::new()),
        }
    }

    /// The value to put in [`WORKTREE_METADATA_ENV`] for a child process.
    pub fn to_environment_value(&self) -> OsString {
        OsString::from(serde_json::to_string(&self.0).expect("serialize string map"))
    }

    /// This metadata with every entry of `overrides` added, replacing an
    /// inherited value under the same key.
    pub fn overridden_by(mut self, overrides: Self) -> Result<Self, WorktreeError> {
        for (key, value) in overrides.0 {
            self.0.insert(key, value);
        }
        if self.0.len() > MAX_METADATA_ENTRIES {
            return Err(too_many());
        }
        Ok(self)
    }

    fn insert(&mut self, key: &str, value: &str) -> Result<(), WorktreeError> {
        validate_key(key)?;
        validate_value(key, value)?;
        self.0.insert(key.to_owned(), value.to_owned());
        if self.0.len() > MAX_METADATA_ENTRIES {
            return Err(too_many());
        }
        Ok(())
    }
}

fn validate_key(key: &str) -> Result<(), WorktreeError> {
    let mut bytes = key.bytes();
    let valid = key.len() <= MAX_METADATA_KEY_BYTES
        && bytes
            .next()
            .is_some_and(|first| first.is_ascii_alphanumeric())
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(invalid(format!(
            "metadata key `{}` must be 1 to {MAX_METADATA_KEY_BYTES} ASCII letters, digits, `.`, `_`, or `-`, starting with a letter or digit",
            key.escape_debug()
        )))
    }
}

fn validate_value(key: &str, value: &str) -> Result<(), WorktreeError> {
    if value.len() > MAX_METADATA_VALUE_BYTES {
        return Err(invalid(format!(
            "metadata value for `{key}` is longer than {MAX_METADATA_VALUE_BYTES} bytes"
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(invalid(format!(
            "metadata value for `{key}` contains a control character"
        )));
    }
    Ok(())
}

fn too_many() -> WorktreeError {
    invalid(format!(
        "a worktree can carry at most {MAX_METADATA_ENTRIES} metadata entries"
    ))
}

fn invalid(detail: String) -> WorktreeError {
    WorktreeError::InvalidRequest(detail)
}

/// Render one stored value for a terminal. Stored metadata was validated when
/// it was written, but the journal is a file, so control characters are
/// escaped rather than trusted.
pub fn display_metadata_value(value: &str) -> String {
    if value.chars().any(char::is_control) {
        value
            .chars()
            .map(|character| {
                if character.is_control() {
                    character.escape_default().to_string()
                } else {
                    character.to_string()
                }
            })
            .collect()
    } else {
        value.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refusal(result: Result<WorktreeMetadata, WorktreeError>) -> String {
        match result {
            Err(WorktreeError::InvalidRequest(detail)) => detail,
            other => panic!("expected an invalid-request refusal, got {other:?}"),
        }
    }

    #[test]
    fn parses_entries_in_key_order_and_keeps_equals_signs_in_values() {
        let metadata =
            WorktreeMetadata::parse_entries(["task=42", "agent=codex", "note=a=b", "empty="])
                .expect("parse metadata");
        let entries: Vec<_> = metadata.iter().collect();
        assert_eq!(
            entries,
            [
                ("agent", "codex"),
                ("empty", ""),
                ("note", "a=b"),
                ("task", "42"),
            ]
        );
    }

    #[test]
    fn refuses_malformed_entries() {
        for entry in [
            "agent",
            "=codex",
            "-agent=x",
            "agent name=x",
            "agént=x",
            "a/b=x",
        ] {
            let detail = refusal(WorktreeMetadata::parse_entries([entry]));
            assert!(detail.contains("metadata"), "{entry}: {detail}");
        }
        let long_key = format!("{}=x", "k".repeat(MAX_METADATA_KEY_BYTES + 1));
        refusal(WorktreeMetadata::parse_entries([long_key]));
        let longest_key = format!("{}=x", "k".repeat(MAX_METADATA_KEY_BYTES));
        WorktreeMetadata::parse_entries([longest_key]).expect("longest key fits");
    }

    #[test]
    fn refuses_control_characters_and_oversized_values() {
        for value in ["line\nbreak", "tab\there", "bell\u{7}", "c1\u{85}"] {
            let detail = refusal(WorktreeMetadata::parse_entries([format!("agent={value}")]));
            assert!(detail.contains("control character"), "{detail}");
        }
        let long = format!("agent={}", "v".repeat(MAX_METADATA_VALUE_BYTES + 1));
        refusal(WorktreeMetadata::parse_entries([long]));
        let longest = format!("agent={}", "v".repeat(MAX_METADATA_VALUE_BYTES));
        WorktreeMetadata::parse_entries([longest]).expect("longest value fits");
        WorktreeMetadata::parse_entries(["agent=Claude Code ✓"]).expect("non-ASCII text fits");
    }

    #[test]
    fn refuses_a_repeated_key_and_too_many_entries() {
        let detail = refusal(WorktreeMetadata::parse_entries(["agent=a", "agent=b"]));
        assert!(detail.contains("more than once"), "{detail}");

        let many: Vec<String> = (0..=MAX_METADATA_ENTRIES)
            .map(|index| format!("k{index}=v"))
            .collect();
        refusal(WorktreeMetadata::parse_entries(&many));
        WorktreeMetadata::parse_entries(&many[..MAX_METADATA_ENTRIES]).expect("limit fits");
    }

    #[cfg(unix)]
    #[test]
    fn refuses_entries_that_are_not_utf8() {
        use std::os::unix::ffi::OsStringExt;
        let entry = OsString::from_vec(b"agent=\xff".to_vec());
        let detail = refusal(WorktreeMetadata::parse_entries([entry]));
        assert!(detail.contains("UTF-8"), "{detail}");
    }

    #[test]
    fn environment_value_round_trips_and_validates() {
        let metadata = WorktreeMetadata::parse_entries(["agent=codex", "task=7"]).unwrap();
        let encoded = metadata.to_environment_value();
        assert_eq!(encoded, OsStr::new(r#"{"agent":"codex","task":"7"}"#));
        assert_eq!(
            WorktreeMetadata::from_environment_value(&encoded).unwrap(),
            metadata
        );
        assert!(
            WorktreeMetadata::from_environment_value(OsStr::new(""))
                .unwrap()
                .is_empty()
        );

        for bad in [r#"["agent"]"#, r#"{"agent":1}"#, r#"{"bad key":"x"}"#, "{"] {
            let detail = refusal(WorktreeMetadata::from_environment_value(OsStr::new(bad)));
            assert!(detail.contains(WORKTREE_METADATA_ENV), "{bad}: {detail}");
        }
    }

    #[test]
    fn explicit_entries_override_inherited_ones() {
        let inherited = WorktreeMetadata::parse_entries(["agent=codex", "team=core"]).unwrap();
        let explicit = WorktreeMetadata::parse_entries(["agent=claude-code"]).unwrap();
        let merged = inherited.overridden_by(explicit).unwrap();
        assert_eq!(merged.get("agent"), Some("claude-code"));
        assert_eq!(merged.get("team"), Some("core"));

        let half: Vec<String> = (0..MAX_METADATA_ENTRIES)
            .map(|index| format!("a{index}=v"))
            .collect();
        let inherited = WorktreeMetadata::parse_entries(&half).unwrap();
        let explicit = WorktreeMetadata::parse_entries(["b0=v"]).unwrap();
        refusal(inherited.overridden_by(explicit));
    }

    #[test]
    fn display_escapes_control_characters_from_a_tampered_journal() {
        assert_eq!(display_metadata_value("codex"), "codex");
        assert_eq!(display_metadata_value("a\u{1b}[31mb"), "a\\u{1b}[31mb");
    }
}
