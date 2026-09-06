//! What a run produces.
//!
//! The point of the report format is re-runnability. `hub/src/node_client.rs`
//! keeps its pooling benchmark in the tree with the *old code path* as its
//! baseline, so the comparison that chose the pool size can be repeated
//! rather than re-argued. The same idea applies here, one level up: a drill
//! writes a JSON report with a stable shape, the report is checked in under
//! `harness/baselines/`, and after the code it measures changes, the same
//! drill is re-run and the two files are compared. A number in prose is an
//! opinion by the time anyone reads it.
//!
//! Every report therefore records what it ran against, not just what
//! happened. A drill result with no commit and no binary fingerprint is
//! not evidence -- `target/` is shared by every worktree on this machine,
//! so "the hub" is a path, not a version.

use crate::stats::Summary;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

/// Whether the measurement agreed with what the plan predicted.
///
/// `Refuted` is a first-class outcome, not an error. It has happened once
/// already on this codebase -- §6.2's recommended connection pooling
/// measured slower than the churn it was supposed to replace -- and the
/// handoff for this work says plainly that when the numbers disagree with
/// the plan, the numbers win.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// The plan said this would happen, and it did.
    Confirmed,
    /// The plan said this would happen, and it did not.
    Refuted,
    /// The drill ran but could not decide. Distinct from a drill that
    /// failed to run at all, which is an error and not a verdict.
    Inconclusive,
}

/// The code and machine a run happened on.
#[derive(Debug, Clone, Serialize)]
pub struct Environment {
    pub commit: String,
    /// Whether the worktree had uncommitted changes. A dirty tree does not
    /// invalidate a number, but it does mean the commit alone will not
    /// reproduce it.
    pub dirty: bool,
    /// `debug` or `release`. Load numbers from a debug build are worth
    /// recording and worth nothing as an absolute, so this is reported
    /// rather than assumed.
    pub profile: &'static str,
    pub host: String,
    pub cpus: usize,
    /// Every binary the run launched, by path, with its SHA256.
    pub binaries: BTreeMap<String, String>,
}

impl Environment {
    pub fn capture(repo: &Path) -> Self {
        let commit = git(repo, &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
        let dirty = git(repo, &["status", "--porcelain"])
            .map(|out| !out.is_empty())
            .unwrap_or(true);
        Self {
            commit,
            dirty,
            profile: if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            },
            host: std::process::Command::new("uname")
                .arg("-sm")
                .output()
                .ok()
                .and_then(|out| String::from_utf8(out.stdout).ok())
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|| "unknown".to_string()),
            cpus: std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(0),
            binaries: BTreeMap::new(),
        }
    }

    /// Records the SHA256 of a binary this run is about to launch.
    ///
    /// This is the mechanical form of the rule every handoff in this wave
    /// repeats: `target/debug/hub` is one path for four worktrees, so a
    /// measurement that does not name the bytes it ran against may be
    /// measuring another session's work in progress.
    pub fn fingerprint(&mut self, path: &Path) -> Result<()> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("reading {} to fingerprint it", path.display()))?;
        let digest = Sha256::digest(&bytes);
        self.binaries
            .insert(path.display().to_string(), hex::encode(digest));
        Ok(())
    }
}

fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// One drill, or one phase of the load profile.
#[derive(Debug, Clone, Serialize)]
pub struct Section {
    pub title: String,
    /// The plan section this speaks to, e.g. `"§6.4b"`. Present so a
    /// report can be walked back into `docs/agent-ecosystem-plan.md`
    /// mechanically rather than from memory.
    pub plan_item: Option<String>,
    pub verdict: Option<Verdict>,
    /// The numbers the drill actually measured, named. This is the part
    /// that gets compared across runs, so keys should stay stable even
    /// when the prose around them changes.
    pub facts: BTreeMap<String, Value>,
    /// What a reader needs in order to interpret the facts, in sentences.
    pub notes: Vec<String>,
    /// A bug the drill found. Separate from `notes` because these are the
    /// deliverable: the handoff asks for findings written up whether or
    /// not they get fixed.
    pub findings: Vec<String>,
    pub latency: Vec<Summary>,
}

impl Section {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            plan_item: None,
            verdict: None,
            facts: BTreeMap::new(),
            notes: Vec::new(),
            findings: Vec::new(),
            latency: Vec::new(),
        }
    }

    pub fn plan_item(mut self, item: &str) -> Self {
        self.plan_item = Some(item.to_string());
        self
    }

    pub fn verdict(mut self, verdict: Verdict) -> Self {
        self.verdict = Some(verdict);
        self
    }

    pub fn fact(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.facts.insert(key.to_string(), value.into());
        self
    }

    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    pub fn finding(mut self, finding: impl Into<String>) -> Self {
        self.findings.push(finding.into());
        self
    }

    pub fn latency(mut self, summaries: Vec<Summary>) -> Self {
        self.latency = summaries;
        self
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub name: String,
    pub started_at: DateTime<Utc>,
    pub environment: Environment,
    pub sections: Vec<Section>,
}

impl Report {
    pub fn new(name: impl Into<String>, environment: Environment) -> Self {
        Self {
            name: name.into(),
            started_at: Utc::now(),
            environment,
            sections: Vec::new(),
        }
    }

    pub fn push(&mut self, section: Section) {
        self.sections.push(section);
    }

    /// True when any section refuted what the plan predicted, or found a
    /// bug. The process exit code keys off this, so a drill run in CI
    /// fails loudly rather than leaving a file for someone to read.
    pub fn needs_attention(&self) -> bool {
        self.sections
            .iter()
            .any(|s| s.verdict == Some(Verdict::Refuted) || !s.findings.is_empty())
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("=== {} ===\n", self.name));
        out.push_str(&format!(
            "{}  commit {}{}  {} build  {}  {} cpus\n",
            self.started_at.format("%Y-%m-%d %H:%M:%SZ"),
            &self.environment.commit.chars().take(12).collect::<String>(),
            if self.environment.dirty { " (dirty)" } else { "" },
            self.environment.profile,
            self.environment.host,
            self.environment.cpus,
        ));
        for (path, digest) in &self.environment.binaries {
            out.push_str(&format!(
                "  {path}  sha256:{}\n",
                &digest.chars().take(16).collect::<String>()
            ));
        }

        for section in &self.sections {
            out.push_str(&format!("\n--- {} ", section.title));
            if let Some(item) = &section.plan_item {
                out.push_str(&format!("(plan {item}) "));
            }
            match section.verdict {
                Some(Verdict::Confirmed) => out.push_str("[CONFIRMED]"),
                Some(Verdict::Refuted) => out.push_str("[REFUTED]"),
                Some(Verdict::Inconclusive) => out.push_str("[INCONCLUSIVE]"),
                None => {}
            }
            out.push('\n');

            for (key, value) in &section.facts {
                out.push_str(&format!("  {key}: {value}\n"));
            }
            for note in &section.notes {
                out.push_str(&format!("  note: {note}\n"));
            }
            for finding in &section.findings {
                out.push_str(&format!("  FINDING: {finding}\n"));
            }
            if !section.latency.is_empty() {
                out.push('\n');
                out.push_str(&crate::stats::render_table(&section.latency));
            }
        }
        out
    }

    pub fn write_json(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)
            .with_context(|| format!("writing the report to {}", path.display()))?;
        Ok(())
    }
}
