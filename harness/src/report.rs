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
    /// Whether the worktree's *code* differed from `commit`.
    ///
    /// Scoped to sources and manifests rather than the whole tree, and
    /// that is not laziness. A run writes its own report into the repo,
    /// so a whole-tree check reports every run after the first as dirty
    /// on the strength of the previous run's output — which says nothing
    /// about whether the binaries under test matched the commit, and
    /// trains a reader to ignore the flag. Prose edits are excluded for
    /// the same reason: they cannot change a measurement.
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
        let dirty = git(repo, &["status", "--porcelain", "--", "*.rs", "*.toml"])
            .map(|out| !out.is_empty())
            // Unknown is reported as dirty: a flag that cannot be
            // established should not read as a clean bill of health.
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

/// Compares a fresh report against a checked-in baseline.
///
/// This is what makes the baselines more than a file nobody opens. The
/// pooling benchmark in `hub/src/node_client.rs` keeps the old code path
/// beside the new one so the comparison can be *run*; a report on disk
/// only gets that property if something reads it. Sections are matched by
/// title and facts by key, which is why both are meant to stay stable
/// across runs even when the prose around them changes.
///
/// Returns the rendered comparison and whether anything got worse -- a
/// verdict that went from confirmed to refuted, or a finding that was not
/// there before.
pub fn compare(baseline: &serde_json::Value, current: &serde_json::Value) -> (String, bool) {
    let empty = Vec::new();
    let sections = |report: &serde_json::Value| -> Vec<Value> {
        report
            .get("sections")
            .and_then(Value::as_array)
            .unwrap_or(&empty)
            .clone()
    };
    let by_title = |report: &serde_json::Value| -> BTreeMap<String, Value> {
        sections(report)
            .into_iter()
            .filter_map(|section| {
                Some((section.get("title")?.as_str()?.to_string(), section))
            })
            .collect()
    };

    let before = by_title(baseline);
    let after = by_title(current);
    let mut out = String::new();
    let mut worse = false;

    out.push_str(&format!(
        "baseline: {} at {}\ncurrent:  {} at {}\n",
        baseline
            .pointer("/environment/commit")
            .and_then(Value::as_str)
            .unwrap_or("?"),
        baseline.get("started_at").and_then(Value::as_str).unwrap_or("?"),
        current
            .pointer("/environment/commit")
            .and_then(Value::as_str)
            .unwrap_or("?"),
        current.get("started_at").and_then(Value::as_str).unwrap_or("?"),
    ));

    for (title, new_section) in &after {
        out.push_str(&format!("\n--- {title}\n"));
        let Some(old_section) = before.get(title) else {
            out.push_str("  (not in the baseline)\n");
            continue;
        };

        let old_verdict = old_section.get("verdict").and_then(Value::as_str);
        let new_verdict = new_section.get("verdict").and_then(Value::as_str);
        if old_verdict != new_verdict {
            out.push_str(&format!(
                "  verdict: {} -> {}\n",
                old_verdict.unwrap_or("none"),
                new_verdict.unwrap_or("none")
            ));
            // Only one direction is a regression. Going from refuted to
            // confirmed is somebody's fix landing, and should not fail a
            // comparison.
            if new_verdict == Some("refuted") && old_verdict != Some("refuted") {
                worse = true;
            }
        }

        let facts = |section: &Value| -> BTreeMap<String, Value> {
            section
                .get("facts")
                .and_then(Value::as_object)
                .map(|map| map.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default()
        };
        let old_facts = facts(old_section);
        for (key, new_value) in facts(new_section) {
            match old_facts.get(&key) {
                Some(old_value) if old_value == &new_value => {}
                Some(old_value) => {
                    out.push_str(&format!("  {key}: {old_value} -> {new_value}\n"))
                }
                None => out.push_str(&format!("  {key}: (new) {new_value}\n")),
            }
        }

        let findings = |section: &Value| -> Vec<String> {
            section
                .get("findings")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(|f| f.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        };
        let old_findings = findings(old_section);
        for finding in findings(new_section) {
            if !old_findings.contains(&finding) {
                out.push_str(&format!("  NEW FINDING: {finding}\n"));
                worse = true;
            }
        }
        for finding in &old_findings {
            if !findings(new_section).contains(finding) {
                out.push_str(&format!("  gone: {finding}\n"));
            }
        }
    }

    (out, worse)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn report(verdict: &str, findings: Vec<&str>, count: i64) -> Value {
        json!({
            "environment": {"commit": "abc"},
            "started_at": "2026-09-06T00:00:00Z",
            "sections": [{
                "title": "A drill",
                "verdict": verdict,
                "facts": {"itx_lost": count},
                "findings": findings,
            }],
        })
    }

    #[test]
    fn a_new_finding_is_a_regression() {
        let (rendered, worse) = compare(
            &report("confirmed", vec![], 0),
            &report("confirmed", vec!["money vanished"], 5),
        );
        assert!(worse);
        assert!(rendered.contains("NEW FINDING: money vanished"));
        assert!(rendered.contains("itx_lost: 0 -> 5"));
    }

    #[test]
    fn a_finding_going_away_is_not() {
        // The whole point of a baseline here is that somebody lands a fix
        // and the drill stops finding the bug. That must not fail.
        let (rendered, worse) = compare(
            &report("confirmed", vec!["money vanished"], 5),
            &report("confirmed", vec![], 0),
        );
        assert!(!worse);
        assert!(rendered.contains("gone: money vanished"));
    }

    #[test]
    fn confirmed_turning_into_refuted_is_a_regression() {
        let (_, worse) = compare(&report("confirmed", vec![], 0), &report("refuted", vec![], 0));
        assert!(worse);
    }

    #[test]
    fn a_section_missing_from_the_baseline_is_reported_not_ignored() {
        let baseline = json!({"sections": []});
        let (rendered, worse) = compare(&baseline, &report("confirmed", vec![], 0));
        assert!(!worse);
        assert!(rendered.contains("(not in the baseline)"));
    }
}
