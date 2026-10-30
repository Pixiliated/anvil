//! The project record: milestones, decisions, findings with their history, and verify runs, in
//! one SQLite file (`anvil_record.db` next to `anvil.toml`, not checked in). Agents query it
//! instead of reading long status files; `export_markdown` is the view for people.
//!
//! Modelled on the Nautilus engine's record. Everything is loud: an unknown status or kind, a
//! milestone that does not exist, or a file written by another schema version is an error that
//! names what would have worked.

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub mod roadmap;

pub const FILE: &str = "anvil_record.db";
const SCHEMA_VERSION: i64 = 1;
pub const MILESTONE_STATUSES: [&str; 4] = ["open", "active", "done", "dropped"];
pub const KINDS: [&str; 4] = ["milestones", "decisions", "findings", "runs"];

const SCHEMA: &str = r#"
CREATE TABLE milestones (
    id      TEXT PRIMARY KEY,
    title   TEXT NOT NULL,
    status  TEXT NOT NULL CHECK (status IN ('open', 'active', 'done', 'dropped')),
    parent  TEXT REFERENCES milestones(id),
    ord     INTEGER NOT NULL,
    updated TEXT NOT NULL
);
CREATE TABLE decisions (
    id    INTEGER PRIMARY KEY,
    date  TEXT NOT NULL,
    title TEXT NOT NULL,
    body  TEXT NOT NULL
);
CREATE TABLE findings (
    id          INTEGER PRIMARY KEY,
    file        TEXT NOT NULL,
    "check"     TEXT NOT NULL,
    message     TEXT NOT NULL,
    at          TEXT NOT NULL,
    first_seen  TEXT NOT NULL,
    last_seen   TEXT NOT NULL,
    resolved_at TEXT
);
CREATE TABLE runs (
    id       INTEGER PRIMARY KEY,
    at       TEXT NOT NULL,
    files    INTEGER NOT NULL,
    native   INTEGER NOT NULL,
    open     INTEGER NOT NULL,
    opened   INTEGER NOT NULL,
    resolved INTEGER NOT NULL
);
"#;

type R<T> = Result<T, String>;

fn sql<T>(r: rusqlite::Result<T>) -> R<T> {
    r.map_err(|e| format!("record: {e}"))
}

/// A problem seen by `verify`: identified by (file, check, message); `at` is where it is now.
pub struct Seen {
    pub file: String,
    pub check: String,
    pub message: String,
    pub at: String,
}

#[derive(Debug, Default)]
pub struct SyncReport {
    pub opened: Vec<i64>,
    pub resolved: Vec<i64>,
    pub still_open: usize,
}

pub struct Record {
    conn: Connection,
    path: PathBuf,
}

impl Record {
    /// Opens `<root>/anvil_record.db`, creating it on first use.
    pub fn open(root: &Path) -> R<Record> {
        let path = root.join(FILE);
        let fresh = !path.exists();
        let conn = sql(Connection::open(&path))?;
        if fresh {
            sql(conn.execute_batch(SCHEMA))?;
            sql(conn.pragma_update(None, "user_version", SCHEMA_VERSION))?;
        } else {
            let v: i64 = sql(conn.query_row("PRAGMA user_version", [], |r| r.get(0)))?;
            if v != SCHEMA_VERSION {
                return Err(format!(
                    "{} has schema version {v}, this anvil reads version {SCHEMA_VERSION}; export it with an older \
                     anvil, then delete it",
                    path.display()
                ));
            }
        }
        Ok(Record { conn, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    // --- milestones -------------------------------------------------------------------------

    /// Replaces the milestones with the roadmap's, keeping any marked `dropped` by hand.
    pub fn import_roadmap(&mut self, text: &str, now: &str) -> R<Value> {
        let items = roadmap::parse(text)?;
        let tx = sql(self.conn.transaction())?;
        let dropped: Vec<String> = {
            let mut st = sql(tx.prepare("SELECT id FROM milestones WHERE status = 'dropped'"))?;
            let rows = sql(st.query_map([], |r| r.get(0)))?;
            sql(rows.collect::<rusqlite::Result<Vec<String>>>())?
        };
        sql(tx.execute("DELETE FROM milestones", []))?;
        let mut done = 0;
        for (ord, it) in items.iter().enumerate() {
            let status = if dropped.contains(&it.id) { "dropped" } else { it.status };
            done += (status == "done") as usize;
            sql(tx.execute(
                "INSERT INTO milestones (id, title, status, parent, ord, updated) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![it.id, it.title, status, it.parent, ord as i64, now],
            ))?;
        }
        sql(tx.commit())?;
        let current = items.iter().find(|i| i.status == "active").map(|i| json!({ "id": i.id, "title": i.title }));
        Ok(json!({ "milestones": items.len(), "done": done, "current": current }))
    }

    pub fn set_status(&self, id: &str, status: &str, now: &str) -> R<Value> {
        if !MILESTONE_STATUSES.contains(&status) {
            return Err(format!("unknown status `{status}`; use one of {}", MILESTONE_STATUSES.join(", ")));
        }
        let n = sql(self
            .conn
            .execute("UPDATE milestones SET status = ?2, updated = ?3 WHERE id = ?1", params![id, status, now]))?;
        if n == 0 {
            let ids: Vec<String> = self.milestones()?.iter().map(|m| m["id"].as_str().unwrap().to_string()).collect();
            let hint = if ids.is_empty() {
                "import the roadmap first".to_string()
            } else {
                format!("known: {}", ids.join(", "))
            };
            return Err(format!("no milestone `{id}`; {hint}"));
        }
        Ok(json!({ "id": id, "status": status }))
    }

    pub fn milestones(&self) -> R<Vec<Value>> {
        let mut st = sql(self.conn.prepare("SELECT id, title, status, parent, updated FROM milestones ORDER BY ord"))?;
        let rows = sql(st.query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?, "title": r.get::<_, String>(1)?, "status": r.get::<_, String>(2)?,
                "parent": r.get::<_, Option<String>>(3)?, "updated": r.get::<_, String>(4)?,
            }))
        }))?;
        sql(rows.collect())
    }

    // --- decisions --------------------------------------------------------------------------

    pub fn decide(&self, date: &str, title: &str, body: &str) -> R<Value> {
        if title.trim().is_empty() {
            return Err("a decision needs a title".into());
        }
        sql(self
            .conn
            .execute("INSERT INTO decisions (date, title, body) VALUES (?1, ?2, ?3)", params![date, title, body]))?;
        Ok(json!({ "id": self.conn.last_insert_rowid(), "date": date, "title": title, "body": body }))
    }

    pub fn decisions(&self) -> R<Vec<Value>> {
        let mut st = sql(self.conn.prepare("SELECT id, date, title, body FROM decisions ORDER BY id"))?;
        let rows = sql(st.query_map([], |r| {
            Ok(json!({ "id": r.get::<_, i64>(0)?, "date": r.get::<_, String>(1)?, "title": r.get::<_, String>(2)?, "body": r.get::<_, String>(3)? }))
        }))?;
        sql(rows.collect())
    }

    // --- findings and runs ------------------------------------------------------------------

    /// Stores what one verify run saw: new problems open, problems no longer seen resolve, and
    /// one that comes back after being resolved opens a new row (a regression keeps its history).
    pub fn sync(&mut self, seen: &[Seen], now: &str) -> R<SyncReport> {
        let mut report = SyncReport::default();
        let tx = sql(self.conn.transaction())?;
        let mut touched = Vec::new();
        for s in seen {
            let open: Option<i64> = sql(tx
                .query_row(
                    r#"SELECT id FROM findings WHERE file = ?1 AND "check" = ?2 AND message = ?3 AND resolved_at IS NULL"#,
                    params![s.file, s.check, s.message],
                    |r| r.get(0),
                )
                .optional())?;
            let id = match open {
                Some(id) => {
                    sql(
                        tx.execute("UPDATE findings SET last_seen = ?2, at = ?3 WHERE id = ?1", params![id, now, s.at])
                    )?;
                    report.still_open += 1;
                    id
                }
                None => {
                    sql(tx.execute(
                        r#"INSERT INTO findings (file, "check", message, at, first_seen, last_seen) VALUES (?1, ?2, ?3, ?4, ?5, ?5)"#,
                        params![s.file, s.check, s.message, s.at, now],
                    ))?;
                    let id = tx.last_insert_rowid();
                    report.opened.push(id);
                    id
                }
            };
            touched.push(id);
        }
        let open: Vec<i64> = {
            let mut st = sql(tx.prepare("SELECT id FROM findings WHERE resolved_at IS NULL"))?;
            let rows = sql(st.query_map([], |r| r.get(0)))?;
            sql(rows.collect::<rusqlite::Result<Vec<i64>>>())?
        };
        for id in open {
            if !touched.contains(&id) {
                sql(tx.execute("UPDATE findings SET resolved_at = ?2 WHERE id = ?1", params![id, now]))?;
                report.resolved.push(id);
            }
        }
        sql(tx.commit())?;
        Ok(report)
    }

    pub fn findings(&self, open_only: bool) -> R<Vec<Value>> {
        let q = format!(
            r#"SELECT id, file, "check", message, at, first_seen, last_seen, resolved_at FROM findings {} ORDER BY id"#,
            if open_only { "WHERE resolved_at IS NULL" } else { "" }
        );
        let mut st = sql(self.conn.prepare(&q))?;
        let rows = sql(st.query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?, "file": r.get::<_, String>(1)?, "check": r.get::<_, String>(2)?,
                "message": r.get::<_, String>(3)?, "at": r.get::<_, String>(4)?, "first_seen": r.get::<_, String>(5)?,
                "last_seen": r.get::<_, String>(6)?, "resolved_at": r.get::<_, Option<String>>(7)?,
            }))
        }))?;
        sql(rows.collect())
    }

    pub fn add_run(&self, now: &str, files: usize, native: bool, open: usize, sync: &SyncReport) -> R<i64> {
        sql(self.conn.execute(
            "INSERT INTO runs (at, files, native, open, opened, resolved) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![now, files as i64, native, open as i64, sync.opened.len() as i64, sync.resolved.len() as i64],
        ))?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn runs(&self) -> R<Vec<Value>> {
        let mut st =
            sql(self.conn.prepare("SELECT id, at, files, native, open, opened, resolved FROM runs ORDER BY id"))?;
        let rows = sql(st.query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, i64>(0)?, "at": r.get::<_, String>(1)?, "files": r.get::<_, i64>(2)?,
                "native": r.get::<_, bool>(3)?, "open": r.get::<_, i64>(4)?, "opened": r.get::<_, i64>(5)?,
                "resolved": r.get::<_, i64>(6)?,
            }))
        }))?;
        sql(rows.collect())
    }

    // --- views ------------------------------------------------------------------------------

    /// One kind's rows, or (no kind) a one-screen summary. `open_only` applies to findings.
    pub fn query(&self, kind: Option<&str>, open_only: bool) -> R<Value> {
        if open_only && kind.is_some_and(|k| k != "findings") {
            return Err("--open applies to findings only".into());
        }
        Ok(match kind {
            None if open_only => json!({ "findings": self.findings(true)? }),
            None => self.summary()?,
            Some("milestones") => json!({ "milestones": self.milestones()? }),
            Some("decisions") => json!({ "decisions": self.decisions()? }),
            Some("findings") => json!({ "findings": self.findings(open_only)? }),
            Some("runs") => json!({ "runs": self.runs()? }),
            Some(other) => return Err(format!("unknown kind `{other}`; use one of {}", KINDS.join(", "))),
        })
    }

    pub fn summary(&self) -> R<Value> {
        let ms = self.milestones()?;
        let count = |s: &str| ms.iter().filter(|m| m["status"] == s && m["parent"].is_null()).count();
        let current = ms.iter().find(|m| m["parent"].is_null() && m["status"] == "active");
        let findings = self.findings(false)?;
        let open = findings.iter().filter(|f| f["resolved_at"].is_null()).count();
        let last_run = self.runs()?.pop();
        Ok(json!({
            "record": self.path.display().to_string(),
            "milestones": { "open": count("open"), "active": count("active"), "done": count("done"), "dropped": count("dropped") },
            "current": current.map(|m| json!({ "id": m["id"], "title": m["title"] })),
            "decisions": self.decisions()?.len(),
            "findings": { "open": open, "resolved": findings.len() - open },
            "last_run": last_run,
        }))
    }

    pub fn export_markdown(&self, project_name: &str) -> R<String> {
        let mut md = format!("# {project_name}: project record\n\nExported from `{FILE}`.\n\n## Milestones\n");
        let ms = self.milestones()?;
        if ms.is_empty() {
            md.push_str("\nNone imported (`anvil record import-roadmap`).\n");
        }
        for m in &ms {
            let (id, title, status) =
                (m["id"].as_str().unwrap(), m["title"].as_str().unwrap(), m["status"].as_str().unwrap());
            if m["parent"].is_null() {
                md.push_str(&format!("\n**{id} {title}**: {status}\n\n"));
            } else {
                let mark = match status {
                    "done" => "x",
                    "dropped" => "-",
                    _ => " ",
                };
                md.push_str(&format!("- [{mark}] {id} {title}\n"));
            }
        }
        md.push_str("\n## Decisions\n\n");
        let ds = self.decisions()?;
        if ds.is_empty() {
            md.push_str("None.\n");
        }
        for d in ds {
            md.push_str(&format!(
                "- **{} · {}.** {}\n",
                d["date"].as_str().unwrap(),
                d["title"].as_str().unwrap(),
                d["body"].as_str().unwrap()
            ));
        }
        for (title, open) in [("Open findings", true), ("Resolved findings", false)] {
            md.push_str(&format!("\n## {title}\n\n"));
            let rows: Vec<Value> =
                self.findings(false)?.into_iter().filter(|f| f["resolved_at"].is_null() == open).collect();
            if rows.is_empty() {
                md.push_str("None.\n");
            }
            for f in rows {
                let line = format!(
                    "- #{} {} at {} [{}] {} (since {}",
                    f["id"],
                    f["file"].as_str().unwrap(),
                    f["at"].as_str().unwrap(),
                    f["check"].as_str().unwrap(),
                    f["message"].as_str().unwrap(),
                    f["first_seen"].as_str().unwrap()
                );
                md.push_str(&match f["resolved_at"].as_str() {
                    Some(r) => format!("{line}, resolved {r})\n"),
                    None => format!("{line})\n"),
                });
            }
        }
        Ok(md)
    }
}

/// "2026-10-30T12:38:05Z", from the system clock (the record is history, not program output).
pub fn now_utc() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let (y, m, d) = civil(secs.div_euclid(86_400));
    let s = secs.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", s / 3600, s % 3600 / 60, s % 60)
}

pub fn today_utc() -> String {
    now_utc()[..10].to_string()
}

/// Days since 1970-01-01 to (year, month, day); Howard Hinnant's algorithm.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + (m <= 2) as i64, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(19_723), (2024, 1, 1));
        assert_eq!(civil(20_756), (2026, 10, 30));
    }

    #[test]
    fn findings_open_resolve_and_regress() {
        let dir = std::env::temp_dir().join(format!("anvil_record_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let _ = std::fs::remove_file(dir.join(FILE));
        let mut r = Record::open(&dir).unwrap();
        let seen = |m: &str| Seen { file: "a.anv".into(), check: "E0301".into(), message: m.into(), at: "3:1".into() };
        let s1 = r.sync(&[seen("x"), seen("y")], "t1").unwrap();
        assert_eq!((s1.opened.len(), s1.resolved.len(), s1.still_open), (2, 0, 0));
        let s2 = r.sync(&[seen("y")], "t2").unwrap();
        assert_eq!((s2.opened.len(), s2.resolved.len(), s2.still_open), (0, 1, 1));
        let s3 = r.sync(&[seen("x"), seen("y")], "t3").unwrap();
        assert_eq!((s3.opened.len(), s3.resolved.len(), s3.still_open), (1, 0, 1), "a regression opens a new row");
        assert_eq!(r.findings(false).unwrap().len(), 3);
        assert_eq!(r.findings(true).unwrap().len(), 2);
        assert!(r.query(Some("decisions"), true).is_err());
        assert!(r.query(Some("notes"), false).unwrap_err().contains("milestones, decisions"));
        assert!(r.set_status("M9", "done", "t").unwrap_err().contains("import the roadmap"));
        drop(r);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
