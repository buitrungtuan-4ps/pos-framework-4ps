// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A single check failure, and how it is rendered for a reviewer.

use std::fmt;

/// One rule violation, located precisely enough to fix without searching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Repository-relative path of the offending file.
    pub file: String,
    /// One-based line, when the check can identify one.
    pub line: Option<u32>,
    /// Short kebab-case rule name, e.g. `dependency-rule`.
    pub rule: String,
    /// What is wrong.
    pub message: String,
    /// What to do about it. Omitted when the message is already actionable.
    pub fix_hint: Option<String>,
}

impl Finding {
    /// A finding with no specific line.
    pub fn new(
        file: impl Into<String>,
        rule: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            file: file.into(),
            line: None,
            rule: rule.into(),
            message: message.into(),
            fix_hint: None,
        }
    }

    /// Attaches a one-based line number.
    #[must_use]
    pub fn at_line(mut self, line: u32) -> Self {
        self.line = Some(line);
        self
    }

    /// Attaches the remedy.
    #[must_use]
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.fix_hint = Some(hint.into());
        self
    }

    /// Renders as a GitHub workflow command, so the failure appears inline on the
    /// pull-request diff rather than only in the job log.
    pub fn as_github_annotation(&self) -> String {
        let line = self.line.map_or_else(String::new, |l| format!(",line={l}"));
        let hint = self
            .fix_hint
            .as_ref()
            .map_or_else(String::new, |h| format!(" — {h}"));
        // Newlines would terminate the workflow command, so fold them.
        let body = format!("[{}] {}{}", self.rule, self.message, hint).replace('\n', " ");
        format!(
            "::error file={}{line},title={}::{body}",
            self.file, self.rule
        )
    }
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.file)?;
        if let Some(line) = self.line {
            write!(f, ":{line}")?;
        }
        write!(f, ": [{}] {}", self.rule, self.message)?;
        if let Some(hint) = &self.fix_hint {
            write!(f, " — {hint}")?;
        }
        Ok(())
    }
}
