use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::util::now_secs_f64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptChangeRecord {
    pub recorded_at: f64,
    pub agent_scope: String,
    pub agent_id: String,
    pub file_path: String,
    pub strategy: String,
    pub trigger: String,
    pub summary: String,
    pub diff_summary: String,
    pub added_lines: Vec<String>,
    pub removed_lines: Vec<String>,
    pub before: String,
    pub after: String,
}

#[derive(Debug, Clone)]
pub struct PromptHistoryManager {
    path: PathBuf,
}

impl PromptHistoryManager {
    pub fn new(dir: PathBuf) -> anyhow::Result<Self> {
        fs::create_dir_all(&dir)?;
        Ok(Self {
            path: dir.join("prompt_history.jsonl"),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn append(
        &self,
        agent_scope: &str,
        agent_id: &str,
        file_path: &str,
        strategy: &str,
        trigger: &str,
        before: &str,
        after: &str,
    ) -> anyhow::Result<()> {
        let record = PromptChangeRecord {
            recorded_at: now_secs_f64(),
            agent_scope: agent_scope.to_string(),
            agent_id: agent_id.to_string(),
            file_path: file_path.to_string(),
            strategy: strategy.to_string(),
            trigger: trigger.to_string(),
            summary: summarize_change(before, after),
            diff_summary: summarize_line_diff(before, after),
            added_lines: collect_added_lines(before, after),
            removed_lines: collect_removed_lines(before, after),
            before: before.to_string(),
            after: after.to_string(),
        };
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(file, "{}", serde_json::to_string(&record)?)?;
        Ok(())
    }

    pub fn list_recent(&self, limit: usize) -> anyhow::Result<Vec<PromptChangeRecord>> {
        if !self.path.exists() || limit == 0 {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(&self.path)?;
        let mut items = content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| serde_json::from_str::<PromptChangeRecord>(line).ok())
            .collect::<Vec<_>>();
        let start = items.len().saturating_sub(limit);
        Ok(items.drain(start..).collect())
    }
}

fn summarize_change(before: &str, after: &str) -> String {
    let before_len = before.chars().count();
    let after_len = after.chars().count();
    let trend = if after_len < before_len {
        "compressed"
    } else if after_len > before_len {
        "expanded"
    } else {
        "rewrote"
    };
    let before_first = before
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("-");
    let after_first = after
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("-");
    if before_first == after_first {
        format!("{trend} prompt ({before_len} -> {after_len} chars)")
    } else {
        format!(
            "{trend} prompt ({before_len} -> {after_len} chars), first line '{}' -> '{}'",
            truncate(before_first, 48),
            truncate(after_first, 48)
        )
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let end = text
        .char_indices()
        .map(|(idx, _)| idx)
        .take_while(|idx| *idx < max)
        .last()
        .unwrap_or(0);
    format!("{}...", &text[..end])
}

fn summarize_line_diff(before: &str, after: &str) -> String {
    let added = collect_added_lines(before, after);
    let removed = collect_removed_lines(before, after);
    match (added.is_empty(), removed.is_empty()) {
        (true, true) => "no line-level changes detected".to_string(),
        (false, true) => format!("added {} lines", added.len()),
        (true, false) => format!("removed {} lines", removed.len()),
        (false, false) => format!(
            "added {} lines, removed {} lines",
            added.len(),
            removed.len()
        ),
    }
}

fn collect_added_lines(before: &str, after: &str) -> Vec<String> {
    collect_line_delta(after, before)
}

fn collect_removed_lines(before: &str, after: &str) -> Vec<String> {
    collect_line_delta(before, after)
}

fn collect_line_delta(left: &str, right: &str) -> Vec<String> {
    let right_lines = right
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    left.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter(|line| !right_lines.contains(line))
        .map(|line| truncate(line, 140))
        .take(12)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{collect_added_lines, collect_removed_lines, summarize_line_diff};

    #[test]
    fn line_diff_summarizes_add_and_remove() {
        let before = "alpha\nbeta\n";
        let after = "alpha\ngamma\n";
        assert_eq!(
            summarize_line_diff(before, after),
            "added 1 lines, removed 1 lines"
        );
        assert_eq!(
            collect_added_lines(before, after),
            vec!["gamma".to_string()]
        );
        assert_eq!(
            collect_removed_lines(before, after),
            vec!["beta".to_string()]
        );
    }
}
