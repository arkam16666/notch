//! Claude Code token usage, computed from the local session logs
//! (`~/.claude/projects/**/*.jsonl`). Nothing leaves the machine.
//!
//! Files are read incrementally (only bytes appended since the last scan), so
//! refreshing is cheap even with hundreds of MB of history.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Datelike, Local, NaiveDate, TimeDelta, Timelike, Utc};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
}

impl Tokens {
    pub fn total(&self) -> u64 {
        self.input + self.output + self.cache_write + self.cache_read
    }
    fn add(&mut self, o: &Tokens) {
        self.input += o.input;
        self.output += o.output;
        self.cache_write += o.cache_write;
        self.cache_read += o.cache_read;
    }
}

/// A 5-hour usage window (how Claude plan limits are counted).
#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub start: DateTime<Local>,
    pub end: DateTime<Local>,
    pub tokens: Tokens,
    pub messages: u32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Usage {
    pub today: Tokens,
    pub today_messages: u32,
    pub week: Tokens,
    /// Last 7 days, oldest first: (date, total tokens).
    pub daily: Vec<(NaiveDate, u64)>,
    /// Currently active 5-hour block, if any.
    pub block: Option<Block>,
    /// Most used model today (by output tokens), prettified.
    pub top_model: Option<String>,
    /// Calendar week (Monday → Sunday) containing today.
    pub this_week: Tokens,
    pub this_week_messages: u32,
    /// Monday → Sunday of this week: (date, total tokens); future days = 0.
    pub week_days: Vec<(NaiveDate, u64)>,
    /// Last week's total, up to the same weekday/time (fair comparison).
    pub last_week_so_far: u64,
}

#[derive(Clone)]
struct Rec {
    at: DateTime<Utc>,
    model: String,
    tokens: Tokens,
}

#[derive(Default)]
struct Scanner {
    /// file → bytes already consumed
    offsets: HashMap<PathBuf, u64>,
    seen: HashSet<String>,
    recs: Vec<Rec>,
}

fn roots() -> Vec<PathBuf> {
    let mut v = Vec::new();
    if let Ok(dirs) = std::env::var("CLAUDE_CONFIG_DIR") {
        v.extend(dirs.split(',').map(|d| PathBuf::from(d.trim()).join("projects")));
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        v.push(home.join(".claude/projects"));
        v.push(home.join(".config/claude/projects"));
    }
    v
}

fn jsonl_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            jsonl_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "jsonl") {
            out.push(p);
        }
    }
}

fn parse_line(line: &str) -> Option<(String, Rec)> {
    // cheap pre-filter before full JSON parsing
    if !line.contains("\"usage\"") || !line.contains("\"assistant\"") {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let msg = v.get("message")?;
    let u = msg.get("usage")?;
    let model = msg.get("model").and_then(|m| m.as_str()).unwrap_or("").to_string();
    if model.starts_with('<') {
        return None; // "<synthetic>" placeholder messages
    }
    let n = |k: &str| u.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
    let tokens = Tokens {
        input: n("input_tokens"),
        output: n("output_tokens"),
        cache_write: n("cache_creation_input_tokens"),
        cache_read: n("cache_read_input_tokens"),
    };
    let at: DateTime<Utc> = v.get("timestamp")?.as_str()?.parse().ok()?;
    // Streaming / resumed sessions log the same response more than once.
    let key = format!(
        "{}:{}",
        msg.get("id").and_then(|x| x.as_str()).unwrap_or(""),
        v.get("requestId").and_then(|x| x.as_str()).unwrap_or("")
    );
    Some((key, Rec { at, model, tokens }))
}

impl Scanner {
    fn scan(&mut self) {
        // keep ~2 weeks so "this week vs last week" works
        let horizon = SystemTime::now() - Duration::from_secs(16 * 24 * 3600);
        let mut files = Vec::new();
        for r in roots() {
            jsonl_files(&r, &mut files);
        }
        for path in files {
            let Ok(meta) = std::fs::metadata(&path) else { continue };
            if meta.modified().is_ok_and(|m| m < horizon) {
                continue; // nothing from the last week in here
            }
            let off = self.offsets.get(&path).copied().unwrap_or(0);
            let len = meta.len();
            if len <= off {
                if len < off {
                    self.offsets.insert(path, 0); // truncated/rewritten: rescan next time
                }
                continue;
            }
            let Ok(mut f) = File::open(&path) else { continue };
            if f.seek(SeekFrom::Start(off)).is_err() {
                continue;
            }
            // Stream line by line so a big first scan doesn't balloon memory.
            let mut reader = BufReader::with_capacity(256 * 1024, f);
            let mut line = Vec::new();
            let mut complete = 0u64;
            loop {
                line.clear();
                match reader.read_until(b'\n', &mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if line.last() != Some(&b'\n') {
                            break; // partial last line: re-read on the next scan
                        }
                        complete += n as u64;
                        if let Ok(text) = std::str::from_utf8(&line) {
                            if let Some((key, rec)) = parse_line(text) {
                                if self.seen.insert(key) {
                                    self.recs.push(rec);
                                }
                            }
                        }
                    }
                }
            }
            self.offsets.insert(path, off + complete);
        }
        // forget anything older than our window
        let cutoff = Utc::now() - TimeDelta::days(16);
        self.recs.retain(|r| r.at >= cutoff);
    }

    fn summarize(&mut self) -> Usage {
        self.recs.sort_by_key(|r| r.at);
        let now = Local::now();
        let today = now.date_naive();
        let mut u = Usage::default();
        let mut by_day: HashMap<NaiveDate, u64> = HashMap::new();
        let mut models: HashMap<&str, u64> = HashMap::new();
        let week_start = today - TimeDelta::days(6);
        let monday = today - TimeDelta::days(today.weekday().num_days_from_monday() as i64);
        let last_monday = monday - TimeDelta::days(7);
        let same_point_last_week = now - TimeDelta::days(7);

        for r in &self.recs {
            let local = r.at.with_timezone(&Local);
            let d = local.date_naive();
            if d >= monday {
                u.this_week.add(&r.tokens);
                u.this_week_messages += 1;
            } else if d >= last_monday && local <= same_point_last_week {
                u.last_week_so_far += r.tokens.total();
            }
            if d >= week_start {
                *by_day.entry(d).or_default() += r.tokens.total();
                u.week.add(&r.tokens);
            }
            if d == today {
                u.today.add(&r.tokens);
                u.today_messages += 1;
                *models.entry(&r.model).or_default() += r.tokens.output;
            }
        }
        u.daily = (0..7)
            .map(|i| {
                let d = week_start + TimeDelta::days(i);
                (d, by_day.get(&d).copied().unwrap_or(0))
            })
            .collect();
        u.week_days = (0..7)
            .map(|i| {
                let d = monday + TimeDelta::days(i);
                (d, by_day.get(&d).copied().unwrap_or(0))
            })
            .collect();
        u.top_model = models
            .into_iter()
            .max_by_key(|(_, v)| *v)
            .map(|(m, _)| pretty_model(m));

        // 5-hour blocks: a block starts at the hour of its first message and
        // lasts 5h; a gap of 5h+ also starts a new block.
        let five = TimeDelta::hours(5);
        let mut block: Option<Block> = None;
        let mut last_at: Option<DateTime<Local>> = None;
        for r in &self.recs {
            let at = r.at.with_timezone(&Local);
            let new_block = match (&block, last_at) {
                (Some(b), Some(l)) => at >= b.end || at - l >= five,
                _ => true,
            };
            if new_block {
                let start = at
                    .with_minute(0)
                    .and_then(|t| t.with_second(0))
                    .and_then(|t| t.with_nanosecond(0))
                    .unwrap_or(at);
                block = Some(Block { start, end: start + five, tokens: Tokens::default(), messages: 0 });
            }
            if let Some(b) = block.as_mut() {
                b.tokens.add(&r.tokens);
                b.messages += 1;
            }
            last_at = Some(at);
        }
        u.block = block.filter(|b| now < b.end);
        u
    }
}

/// "claude-opus-5-5" → "Opus 5.5", "claude-sonnet-4-5-20250929" → "Sonnet 4.5"
pub fn pretty_model(m: &str) -> String {
    let parts: Vec<&str> = m.trim_start_matches("claude-").split('-').collect();
    let mut name = String::new();
    let mut nums = Vec::new();
    for p in parts {
        if p.chars().all(|c| c.is_ascii_digit()) {
            if p.len() <= 2 {
                nums.push(p);
            }
        } else if name.is_empty() {
            let mut c = p.chars();
            if let Some(f) = c.next() {
                name = f.to_uppercase().chain(c).collect();
            }
        }
    }
    if nums.is_empty() { name } else { format!("{name} {}", nums.join(".")) }
}

pub fn format_tokens(n: u64) -> String {
    let n = n as f64;
    if n >= 1e9 {
        format!("{:.1}B", n / 1e9)
    } else if n >= 1e6 {
        format!("{:.1}M", n / 1e6)
    } else if n >= 1e3 {
        format!("{:.0}K", n / 1e3)
    } else {
        format!("{n}")
    }
}

pub fn current() -> Usage {
    let mut s = Scanner::default();
    s.scan();
    s.summarize()
}

/// Background loop: rescan every 15 s and send changes.
pub fn run(tx: Sender<Usage>) {
    let mut s = Scanner::default();
    let mut last: Option<Usage> = None;
    loop {
        s.scan();
        let u = s.summarize();
        if last.as_ref() != Some(&u) {
            if tx.send(u.clone()).is_err() {
                return;
            }
            last = Some(u);
        }
        std::thread::sleep(Duration::from_secs(15));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models() {
        assert_eq!(pretty_model("claude-opus-5-5"), "Opus 5.5");
        assert_eq!(pretty_model("claude-sonnet-4-5-20250929"), "Sonnet 4.5");
        assert_eq!(pretty_model("claude-fable-5-2"), "Fable 5.2");
    }

    #[test]
    fn fmt() {
        assert_eq!(format_tokens(12_400_000), "12.4M");
        assert_eq!(format_tokens(345_000), "345K");
        assert_eq!(format_tokens(12), "12");
    }
}
