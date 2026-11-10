//! Creating and removing the `HEADLESS-*` output a stream renders into
//! (M2-T6, M2-T8).
//!
//! This is the subprocess boundary the plan calls for: the Hyprland helpers
//! today are Python, and the daemon must not link Wayland to create an output.
//! A Rust port of the *minimal* operations is small enough to own here — create
//! a headless output, apply one literal monitor rule, remove the output — while
//! the full arranger in `scripts/display_config.py` stays untouched (plan §5.2).
//!
//! # Removing only what we created
//!
//! `create_headless` returns the output name it created, and the session manager
//! stores exactly that name. Teardown removes that name and nothing else, so a
//! foreign `HEADLESS-*` output is never touched (M2-T8, M6-T1).

use std::process::Command;

use gs_proto::StreamMode;

/// Why an output operation failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OutputError {
    /// `hyprctl` is not installed or not on `PATH`.
    #[error("hyprctl is not available; is this a Hyprland session?")]
    HyprlandUnavailable,
    /// `hyprctl` ran but reported failure.
    #[error("hyprctl {operation} failed: {detail}")]
    Command {
        /// The operation that failed (`create`, `eval`, `remove`, …).
        operation: &'static str,
        /// The message `hyprctl` produced.
        detail: String,
    },
    /// A monitor listing could not be parsed.
    #[error("could not parse the Hyprland monitor list: {0}")]
    Parse(String),
    /// The output was created but its name could not be determined.
    #[error("created a headless output but could not find its name")]
    Unnamed,
}

/// Creates and removes headless outputs.
pub trait OutputManager: std::fmt::Debug + Send + Sync {
    /// Create a headless output at `mode` and return its output name.
    ///
    /// # Errors
    ///
    /// [`OutputError`] when the compositor refuses or is unavailable.
    fn create_headless(&self, mode: StreamMode) -> Result<String, OutputError>;

    /// Remove a headless output previously returned by
    /// [`Self::create_headless`].
    ///
    /// # Errors
    ///
    /// [`OutputError`] when the compositor refuses or is unavailable.
    fn remove_output(&self, name: &str) -> Result<(), OutputError>;
}

/// An [`OutputManager`] backed by `hyprctl`.
///
/// # Behaviour
///
/// `hyprctl output create headless` creates a new `HEADLESS-N` output, but does
/// not reliably print its name, so the name is found by diffing the monitor list
/// before and after creation. That is also what makes the operation safe: the
/// only output ever removed is the one this call observed appear.
#[derive(Debug, Clone)]
pub struct HyprlandOutput {
    program: String,
}

impl Default for HyprlandOutput {
    fn default() -> Self {
        Self::new()
    }
}

impl HyprlandOutput {
    /// Use the `hyprctl` on `PATH`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            program: String::from("hyprctl"),
        }
    }

    /// Use a specific `hyprctl` executable, for tests or a non-standard PATH.
    #[must_use]
    pub fn with_program(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
        }
    }

    fn run(&self, operation: &'static str, args: &[&str]) -> Result<String, OutputError> {
        let output =
            Command::new(&self.program)
                .args(args)
                .output()
                .map_err(|error| match error.kind() {
                    std::io::ErrorKind::NotFound => OutputError::HyprlandUnavailable,
                    _ => OutputError::Command {
                        operation,
                        detail: error.to_string(),
                    },
                })?;
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        if !output.status.success() {
            return Err(OutputError::Command {
                operation,
                detail: if stderr.is_empty() { stdout } else { stderr },
            });
        }
        let combined = format!("{stdout}\n{stderr}").to_lowercase();
        if combined.contains("error") || combined.contains("invalid") {
            return Err(OutputError::Command {
                operation,
                detail: if stderr.is_empty() { stdout } else { stderr },
            });
        }
        Ok(stdout)
    }

    /// The names of every monitor the compositor currently reports.
    fn monitor_names(&self) -> Result<Vec<String>, OutputError> {
        let json = self.run("monitors", &["monitors", "all", "-j"])?;
        parse_monitor_names(&json)
    }
}

impl OutputManager for HyprlandOutput {
    fn create_headless(&self, mode: StreamMode) -> Result<String, OutputError> {
        let before = self.monitor_names()?;
        self.run("create", &["output", "create", "headless"])?;
        let after = self.monitor_names()?;
        let created = after
            .into_iter()
            .find(|name| name.starts_with("HEADLESS-") && !before.contains(name))
            .ok_or(OutputError::Unnamed)?;

        let rule = format!(
            "hl.monitor({{ output = \"{created}\", mode = \"{}x{}@{}\", position = \"auto\", scale = 1 }})",
            mode.width, mode.height, mode.fps
        );
        self.run("eval", &["eval", &rule])?;
        Ok(created)
    }

    fn remove_output(&self, name: &str) -> Result<(), OutputError> {
        self.run("remove", &["output", "remove", name])?;
        Ok(())
    }
}

/// Extract monitor names from `hyprctl monitors all -j` output.
///
/// Tolerant of a missing or unexpected `name` field: a monitor without one is
/// skipped rather than failing the whole listing, because naming is what the
/// diff needs and a single odd entry should not strand a session.
fn parse_monitor_names(json: &str) -> Result<Vec<String>, OutputError> {
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|error| OutputError::Parse(format!("{error}: {json}")))?;
    let array = value
        .as_array()
        .ok_or_else(|| OutputError::Parse(String::from("monitor list is not an array")))?;
    Ok(array
        .iter()
        .filter_map(|monitor| monitor.get("name").and_then(serde_json::Value::as_str))
        .map(str::to_owned)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monitor_names_are_extracted_from_the_json_listing() {
        let json = r#"[
            {"name":"eDP-1","description":"Built-in"},
            {"name":"HEADLESS-1","description":"Headless"}
        ]"#;
        assert_eq!(
            parse_monitor_names(json).unwrap(),
            vec![String::from("eDP-1"), String::from("HEADLESS-1")]
        );
    }

    #[test]
    fn a_missing_name_field_is_skipped_not_fatal() {
        let json = r#"[{"description":"no name"},{"name":"eDP-1"}]"#;
        assert_eq!(
            parse_monitor_names(json).unwrap(),
            vec![String::from("eDP-1")]
        );
    }

    #[test]
    fn a_non_array_listing_is_an_error() {
        assert!(matches!(
            parse_monitor_names("{}"),
            Err(OutputError::Parse(_))
        ));
        assert!(matches!(
            parse_monitor_names("not json"),
            Err(OutputError::Parse(_))
        ));
    }
}
