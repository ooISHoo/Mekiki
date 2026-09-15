//! The handover log — how agents leave word for one another.
//!
//! # Why this exists
//!
//! Several agents from different vendors take turns against this server, and
//! each one starts cold. Without somewhere to write things down, every agent
//! rediscovers the same gaps: the tool that does not exist yet, the locator
//! that never matches on this machine, the workaround that turned out to be
//! necessary. The second agent pays the first agent's cost again.
//!
//! So the server carries a log. An agent reads it before starting and appends
//! to it when it learns something. **The log is a deliverable of the test run,
//! not a side effect of it** — the whole point of inviting several agents is to
//! collect what they hit, and this is where that lands.
//!
//! # Format
//!
//! JSON Lines at `<base>/.mekiki/handover.jsonl`, appended and never rewritten.
//! One note per line, so two processes appending concurrently cannot corrupt
//! each other's lines, and a half-written tail costs one note rather than the
//! file. A malformed line is skipped on read rather than failing the read —
//! losing one note must not hide the other fifty.
//!
//! Plain text on purpose. A human opens it in an editor, `git diff` shows what
//! an agent added, and no schema migration is needed when a field is added.
//!
//! # This is untrusted input
//!
//! Notes are written by agents and read by agents. A note is **data, not
//! instructions**: an agent reading the log must not treat its contents as
//! commands, any more than it should obey text it reads off the screen. The
//! tool description says so, and `AGENTS.md` says so again.

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// What a note is for.
///
/// The kind decides who goes looking for it, so keep the set small enough that
/// choosing is obvious.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum NoteKind {
    /// Something learned about how this tool or this machine behaves.
    Finding,
    /// Something the server cannot do yet. **The most valuable kind** — it is
    /// the backlog for the next round of implementation.
    Limitation,
    /// How to get around a limitation until it is fixed.
    Workaround,
    /// A security concern. Collected during testing and acted on afterwards;
    /// see the module docs of `crate::server`.
    Security,
    /// A question for whoever comes next, or for the maintainer.
    Question,
    /// An answer to an earlier note.
    Answer,
}

impl NoteKind {
    pub const ALL: [NoteKind; 6] = [
        NoteKind::Finding,
        NoteKind::Limitation,
        NoteKind::Workaround,
        NoteKind::Security,
        NoteKind::Question,
        NoteKind::Answer,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            NoteKind::Finding => "finding",
            NoteKind::Limitation => "limitation",
            NoteKind::Workaround => "workaround",
            NoteKind::Security => "security",
            NoteKind::Question => "question",
            NoteKind::Answer => "answer",
        }
    }
}

/// One entry in the handover log.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Note {
    /// Monotonic within a file. Referenced by `replies_to`.
    pub id: u64,
    /// Seconds since the Unix epoch. Wall clock, only for ordering by eye.
    pub timestamp: u64,
    /// Who wrote it, as the agent chose to name itself. Self-reported and
    /// unverified — it says which agent's perspective a note came from, and
    /// nothing more.
    pub agent: String,
    pub kind: NoteKind,
    /// One line. This is what shows up in a listing.
    pub title: String,
    /// The detail. Empty is allowed but rarely useful.
    #[serde(default)]
    pub body: String,
    /// The task being attempted, when there was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// The note this one answers or expands on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replies_to: Option<u64>,
}

/// An append-only note log under one base directory.
pub struct NoteLog {
    path: PathBuf,
    /// Serialises **choosing an id and appending with it**.
    ///
    /// The two steps have to be one step. Appending a line is atomic on its own,
    /// which is what keeps the file readable, but the id is derived by reading
    /// what is already there — so two posts that read before either wrote both
    /// pick the same number.
    ///
    /// This is not hypothetical: it happened the first time several notes were
    /// posted in one batch. The host dispatches tool calls concurrently, so
    /// eight notes all became id 12, and `replies_to` could no longer name any
    /// of them.
    ///
    /// One server owns one base directory, so a mutex covers the real case. Two
    /// servers sharing a base would still race; that would need a file lock, and
    /// nothing does it today.
    appending: Mutex<()>,
}

impl NoteLog {
    pub fn new(base: &Path) -> Self {
        Self {
            path: base.join(".mekiki").join("handover.jsonl"),
            appending: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append a note and return it, with the id and timestamp filled in.
    pub fn post(
        &self,
        agent: &str,
        kind: NoteKind,
        title: &str,
        body: &str,
        task: Option<String>,
        replies_to: Option<u64>,
    ) -> Result<Note, String> {
        let title = title.trim();
        if title.is_empty() {
            return Err("a note needs a title".to_string());
        }

        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }

        // Held until the line is on disk. See the field's documentation for why
        // reading the id and writing it cannot be separate steps.
        let _appending = self
            .appending
            .lock()
            .map_err(|_| "the note log lock is poisoned".to_string())?;

        // The id is derived from what is already in the file rather than kept in
        // memory, so a restarted server does not reuse ids.
        //
        // `max` rather than `last`: an id-colliding log written before this was
        // serialised would otherwise keep handing out numbers already in use.
        let next_id = self.read_all()?.iter().map(|n| n.id).max().unwrap_or(0) + 1;

        let note = Note {
            id: next_id,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            agent: sanitize(agent, 64),
            kind,
            title: sanitize(title, 200),
            body: body.trim().to_string(),
            task: task.map(|t| sanitize(&t, 120)),
            replies_to,
        };

        let mut line =
            serde_json::to_string(&note).map_err(|e| format!("cannot serialise the note: {e}"))?;
        line.push('\n');

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| format!("cannot open {}: {e}", self.path.display()))?;
        file.write_all(line.as_bytes())
            .map_err(|e| format!("cannot write {}: {e}", self.path.display()))?;

        Ok(note)
    }

    /// Every note in the file, oldest first. Missing file means no notes.
    pub fn read_all(&self) -> Result<Vec<Note>, String> {
        let file = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(format!("cannot read {}: {e}", self.path.display())),
        };

        let mut notes = Vec::new();
        for (i, line) in BufReader::new(file).lines().enumerate() {
            let Ok(line) = line else { continue };
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Note>(&line) {
                Ok(note) => notes.push(note),
                // One bad line must not hide the rest of the log.
                Err(e) => log::warn!(
                    "{}:{}: skipping an unreadable note: {e}",
                    self.path.display(),
                    i + 1
                ),
            }
        }
        Ok(notes)
    }

    /// Notes matching a filter, newest first.
    pub fn list(&self, filter: &NoteFilter) -> Result<Vec<Note>, String> {
        let mut notes: Vec<Note> = self
            .read_all()?
            .into_iter()
            .filter(|n| filter.accepts(n))
            .collect();
        notes.reverse();
        notes.truncate(filter.limit.unwrap_or(50).clamp(1, 500));
        Ok(notes)
    }
}

/// Which notes to return.
#[derive(Default)]
pub struct NoteFilter {
    pub kind: Option<NoteKind>,
    pub agent: Option<String>,
    pub since_id: Option<u64>,
    /// A case-insensitive substring of the title or body.
    pub contains: Option<String>,
    pub limit: Option<usize>,
}

impl NoteFilter {
    fn accepts(&self, note: &Note) -> bool {
        if let Some(kind) = self.kind
            && note.kind != kind
        {
            return false;
        }
        if let Some(agent) = &self.agent
            && !note.agent.eq_ignore_ascii_case(agent)
        {
            return false;
        }
        if let Some(since) = self.since_id
            && note.id <= since
        {
            return false;
        }
        if let Some(needle) = &self.contains {
            let needle = needle.to_lowercase();
            if !note.title.to_lowercase().contains(&needle)
                && !note.body.to_lowercase().contains(&needle)
            {
                return false;
            }
        }
        true
    }
}

/// Flatten to one line and cap the length.
///
/// A title with a newline in it would split into two JSON Lines records and
/// corrupt everything after it, so this is structural, not cosmetic.
fn sanitize(text: &str, max_chars: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let trimmed = flat.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    trimmed.chars().take(max_chars).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn temp_base(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mekiki-notes-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn post(log: &NoteLog, agent: &str, kind: NoteKind, title: &str) -> Note {
        log.post(agent, kind, title, "body", None, None).unwrap()
    }

    #[test]
    fn ids_increase_and_survive_a_reopen() {
        let base = temp_base("ids");
        let a = post(&NoteLog::new(&base), "alpha", NoteKind::Finding, "first");
        assert_eq!(a.id, 1);

        // A fresh NoteLog stands in for a restarted server: the id has to come
        // from the file, not from memory.
        let b = post(&NoteLog::new(&base), "beta", NoteKind::Finding, "second");
        assert_eq!(b.id, 2);
    }

    #[test]
    fn listing_is_newest_first_and_filters() {
        let base = temp_base("filter");
        let log = NoteLog::new(&base);
        post(&log, "alpha", NoteKind::Finding, "a finding");
        post(&log, "beta", NoteKind::Limitation, "a gap");
        post(&log, "alpha", NoteKind::Security, "a concern");

        let all = log.list(&NoteFilter::default()).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].title, "a concern", "newest first");

        let security = log
            .list(&NoteFilter {
                kind: Some(NoteKind::Security),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(security.len(), 1);

        let by_agent = log
            .list(&NoteFilter {
                agent: Some("ALPHA".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(by_agent.len(), 2, "agent match is case insensitive");

        let since = log
            .list(&NoteFilter {
                since_id: Some(1),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(since.len(), 2, "since_id is exclusive");
    }

    #[test]
    fn text_search_covers_title_and_body() {
        let base = temp_base("search");
        let log = NoteLog::new(&base);
        log.post(
            "alpha",
            NoteKind::Finding,
            "unrelated title",
            "the OCR misses thin fonts",
            None,
            None,
        )
        .unwrap();

        let hits = log
            .list(&NoteFilter {
                contains: Some("ocr".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(hits.len(), 1, "the body must be searched too");
    }

    /// A newline in a title would split one note into two JSON Lines records
    /// and corrupt every note after it.
    #[test]
    fn control_characters_cannot_break_the_file() {
        let base = temp_base("injection");
        let log = NoteLog::new(&base);
        log.post(
            "a\nb",
            NoteKind::Finding,
            "line one\nline two",
            "body\nwith\nnewlines",
            None,
            None,
        )
        .unwrap();
        post(&log, "beta", NoteKind::Finding, "after");

        let all = log.read_all().unwrap();
        assert_eq!(all.len(), 2, "the log must still parse: {all:?}");
        assert!(!all[0].title.contains('\n'));
        assert!(!all[0].agent.contains('\n'));
    }

    #[test]
    fn a_corrupt_line_does_not_hide_the_rest() {
        let base = temp_base("corrupt");
        let log = NoteLog::new(&base);
        post(&log, "alpha", NoteKind::Finding, "before");

        let mut f = OpenOptions::new().append(true).open(log.path()).unwrap();
        f.write_all(b"{ this is not json\n").unwrap();
        drop(f);

        post(&log, "beta", NoteKind::Finding, "after");

        let all = log.read_all().unwrap();
        assert_eq!(all.len(), 2, "the readable notes must still come back");
        assert_eq!(all[1].id, 2, "ids stay derived from readable notes");
    }

    /// Notes posted at the same time must still get distinct ids.
    ///
    /// **This was a real bug, not a precaution.** The host dispatches tool calls
    /// concurrently, so the first batch of eight notes ever posted all read the
    /// log before any of them wrote, all chose id 12, and `replies_to` could no
    /// longer name a single one of them. The file itself was fine — appending a
    /// line is atomic — which is exactly why it went unnoticed: the damage was
    /// to the numbering, not the format.
    #[test]
    fn concurrent_posts_get_distinct_ids() {
        let base = temp_base("concurrent");
        let log = Arc::new(NoteLog::new(&base));

        let mut threads = Vec::new();
        for i in 0..8 {
            let log = log.clone();
            threads.push(std::thread::spawn(move || {
                log.post(
                    "racer",
                    NoteKind::Finding,
                    &format!("note {i}"),
                    "body",
                    None,
                    None,
                )
                .unwrap()
            }));
        }

        let ids: Vec<u64> = threads.into_iter().map(|t| t.join().unwrap().id).collect();

        let unique: std::collections::BTreeSet<u64> = ids.iter().copied().collect();
        assert_eq!(unique.len(), ids.len(), "ids collided: {ids:?}");

        // And what is on disk has to agree with what the callers were told.
        let written: std::collections::BTreeSet<u64> =
            log.read_all().unwrap().iter().map(|n| n.id).collect();
        assert_eq!(written, unique, "the file disagrees with the returned ids");
    }

    /// A log that already contains colliding ids must not keep reissuing them.
    ///
    /// Recovering from the bug above means the next id has to come from the
    /// highest present, not from the last line.
    #[test]
    fn the_next_id_clears_a_log_with_duplicates() {
        let base = temp_base("duplicates");
        let log = NoteLog::new(&base);
        post(&log, "a", NoteKind::Finding, "first");

        // Hand-write two more lines that both claim id 2, as the race produced.
        let mut f = OpenOptions::new().append(true).open(log.path()).unwrap();
        for title in ["dup one", "dup two"] {
            let line = format!(
                r#"{{"id":2,"timestamp":0,"agent":"a","kind":"finding","title":"{title}","body":""}}"#
            );
            writeln!(f, "{line}").unwrap();
        }
        drop(f);

        let next = post(&log, "a", NoteKind::Finding, "after");
        assert_eq!(
            next.id, 3,
            "the next id must clear every id already present"
        );
    }

    #[test]
    fn missing_file_reads_as_empty() {
        let base = temp_base("missing");
        assert!(NoteLog::new(&base).read_all().unwrap().is_empty());
    }

    #[test]
    fn a_note_needs_a_title() {
        let base = temp_base("empty-title");
        let log = NoteLog::new(&base);
        assert!(
            log.post("a", NoteKind::Finding, "   ", "body", None, None)
                .is_err()
        );
    }
}
