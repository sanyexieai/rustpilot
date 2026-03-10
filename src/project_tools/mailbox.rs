use serde::{Deserialize, Serialize};
use serde_json::json;
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use super::util::now_secs_f64;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailRecord {
    pub msg_id: String,
    pub msg_type: String,
    pub trace_id: String,
    #[serde(default)]
    pub requires_ack: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<String>,
    pub from: String,
    pub to: String,
    pub message: String,
    pub ts: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Mailbox {
    dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MailEnvelope {
    cursor: usize,
    #[serde(flatten)]
    record: MailRecord,
}

impl Mailbox {
    pub fn new(dir: PathBuf) -> anyhow::Result<Self> {
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    pub fn send(
        &self,
        from: &str,
        to: &str,
        message: &str,
        task_id: Option<u64>,
    ) -> anyhow::Result<String> {
        self.send_typed(from, to, "message", message, task_id, None, false, None)
    }

    pub fn send_typed(
        &self,
        from: &str,
        to: &str,
        msg_type: &str,
        message: &str,
        task_id: Option<u64>,
        trace_id: Option<&str>,
        requires_ack: bool,
        in_reply_to: Option<&str>,
    ) -> anyhow::Result<String> {
        let from = normalize_actor(from);
        let to = normalize_actor(to);
        let msg_type = normalize_type(msg_type);
        if from.is_empty() || to.is_empty() {
            anyhow::bail!("from/to 不能为空");
        }
        if msg_type.is_empty() {
            anyhow::bail!("msg_type 不能为空");
        }
        if message.trim().is_empty() {
            anyhow::bail!("message 不能为空");
        }

        let now = now_secs_f64();
        let record = MailRecord {
            msg_id: format!("m-{}-{}", (now * 1000.0) as u64, short_id()),
            msg_type: msg_type.to_string(),
            trace_id: trace_id
                .map(normalize_trace)
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| format!("t-{}", (now * 1000.0) as u64)),
            requires_ack,
            in_reply_to: in_reply_to.map(str::to_string),
            from: from.to_string(),
            to: to.to_string(),
            message: message.trim().to_string(),
            ts: now,
            task_id,
        };

        let path = self.path_for(&to);
        let mut file = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)?;
        writeln!(file, "{}", serde_json::to_string(&record)?)?;
        Ok(serde_json::to_string_pretty(&record)?)
    }

    pub fn inbox(&self, owner: &str, limit: usize) -> anyhow::Result<String> {
        let owner = normalize_actor(owner);
        if owner.is_empty() {
            anyhow::bail!("owner 不能为空");
        }

        let path = self.path_for(&owner);
        if !path.exists() {
            return Ok("[]".to_string());
        }

        let count = limit.clamp(1, 200);
        let content = fs::read_to_string(path)?;
        let lines: Vec<&str> = content.lines().collect();
        let mut items = Vec::new();
        for line in lines
            .into_iter()
            .rev()
            .take(count)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            match serde_json::from_str::<serde_json::Value>(line) {
                Ok(value) => items.push(value),
                Err(_) => items.push(json!({ "event": "parse_error", "raw": line })),
            }
        }
        Ok(serde_json::to_string_pretty(&items)?)
    }

    pub fn poll(&self, owner: &str, after_cursor: usize, limit: usize) -> anyhow::Result<String> {
        let owner = normalize_actor(owner);
        if owner.is_empty() {
            anyhow::bail!("owner 不能为空");
        }
        let path = self.path_for(&owner);
        if !path.exists() {
            return Ok("{\"next_cursor\":0,\"items\":[]}".to_string());
        }

        let count = limit.clamp(1, 200);
        let content = fs::read_to_string(path)?;
        let mut envelopes = Vec::new();
        for (idx, line) in content.lines().enumerate() {
            let cursor = idx + 1;
            if cursor <= after_cursor {
                continue;
            }
            let record = match serde_json::from_str::<MailRecord>(line) {
                Ok(record) => record,
                Err(_) => continue,
            };
            envelopes.push(MailEnvelope { cursor, record });
            if envelopes.len() >= count {
                break;
            }
        }
        let next_cursor = envelopes
            .last()
            .map(|item| item.cursor)
            .unwrap_or(after_cursor);
        let payload = json!({
            "next_cursor": next_cursor,
            "items": envelopes,
        });
        Ok(serde_json::to_string_pretty(&payload)?)
    }

    pub fn ack(&self, owner: &str, msg_id: &str, note: &str) -> anyhow::Result<String> {
        let owner = normalize_actor(owner);
        if owner.is_empty() {
            anyhow::bail!("owner 不能为空");
        }
        let msg_id = msg_id.trim();
        if msg_id.is_empty() {
            anyhow::bail!("msg_id 不能为空");
        }

        let target = self.find_message_for_owner(&owner, msg_id)?;
        let target = target.ok_or_else(|| anyhow::anyhow!("消息不存在: {}", msg_id))?;
        self.send_typed(
            &owner,
            &target.from,
            "task.ack",
            if note.trim().is_empty() {
                "ack"
            } else {
                note.trim()
            },
            target.task_id,
            Some(&target.trace_id),
            false,
            Some(msg_id),
        )
    }

    pub fn pending_count(&self, owner: &str) -> anyhow::Result<usize> {
        let owner = normalize_actor(owner);
        if owner.is_empty() {
            anyhow::bail!("owner cannot be empty");
        }
        let path = self.path_for(&owner);
        if !path.exists() {
            return Ok(0);
        }
        let content = fs::read_to_string(path)?;
        Ok(content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count())
    }

    pub fn backlog_count(&self, owner: &str, after_cursor: usize) -> anyhow::Result<usize> {
        let total = self.pending_count(owner)?;
        Ok(total.saturating_sub(after_cursor))
    }

    fn find_message_for_owner(
        &self,
        owner: &str,
        msg_id: &str,
    ) -> anyhow::Result<Option<MailRecord>> {
        let path = self.path_for(owner);
        if !path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(path)?;
        for line in content.lines().rev() {
            let Ok(record) = serde_json::from_str::<MailRecord>(line) else {
                continue;
            };
            if record.msg_id == msg_id {
                return Ok(Some(record));
            }
        }
        Ok(None)
    }

    fn path_for(&self, owner: &str) -> PathBuf {
        self.dir.join(format!("{}.jsonl", owner))
    }
}

fn normalize_actor(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        .take(40)
        .collect::<String>()
}

fn normalize_type(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        .take(60)
        .collect::<String>()
}

fn normalize_trace(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        .take(80)
        .collect::<String>()
}

fn short_id() -> u32 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    nanos % 100_000
}
