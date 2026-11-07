//! A session of the agent's, and what it has read (AI-LAYER.md, taint tracking). Once a tool that
//! returns untrusted text has answered, the session is tainted for good: nothing clears it. Its
//! record is kept in the agent's home, so what is proposed from it later (ADR-042, Proposals) is
//! proposed `--tainted`, with what tainted it shown to the person who approves.

use crate::tools::Tool;
use anyhow::{Context, Result};
use serde_json::json;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct Session {
    /// The tools that returned untrusted text into it.
    pub tainted_by: BTreeSet<Tool>,
    /// Each tool call, as `name(arguments)`.
    pub calls: Vec<String>,
    /// Each call refused, and why.
    pub refused: Vec<String>,
}

impl Session {
    /// A tool answered into this session.
    pub fn read(&mut self, tool: Tool) {
        if tool.taints() {
            self.tainted_by.insert(tool);
        }
    }

    pub fn tainted(&self) -> bool {
        !self.tainted_by.is_empty()
    }

    pub fn taint_sources(&self) -> Vec<&'static str> {
        self.tainted_by.iter().map(|t| t.name()).collect()
    }

    /// Keep its record in `dir` (the agent's own, 0700), as `<id>.json`.
    pub fn save(&self, dir: &Path, id: &str, question: &str, model: &str) -> Result<PathBuf> {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).with_context(|| format!("creating {}", dir.display()))?;
        let p = dir.join(format!("{id}.json"));
        let record = json!({
            "id": id,
            "mode": "doctor",
            "question": question,
            "model": model,
            "tainted": self.tainted(),
            "taintedBy": self.taint_sources(),
            "calls": self.calls,
            "refused": self.refused,
            "proposed": [],
        });
        std::fs::write(&p, serde_json::to_vec_pretty(&record)?).with_context(|| format!("writing {}", p.display()))?;
        Ok(p)
    }
}
