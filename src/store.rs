use std::{fs, path::Path};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};

use crate::model::{Task, TaskRow};

#[rustfmt::skip]
const CONTROLS: &[(&str, &str)] = &[
    ("pause", "UPDATE task SET paused=1,state=CASE state WHEN 'running' THEN 'backlog' ELSE state END,owner=NULL,lease_until=NULL,revision=revision+1 WHERE uri=?1 AND state NOT IN ('done','cancelled') AND ?2 IS ?2 AND ?3 IS ?3"),
    ("resume", "UPDATE task SET paused=0,revision=revision+1 WHERE uri=?1 AND paused=1 AND state NOT IN ('done','cancelled') AND ?2 IS ?2 AND ?3 IS ?3"),
    ("cancel", "UPDATE task SET state='cancelled',paused=0,owner=NULL,lease_until=NULL,error=coalesce(?3,'cancelled'),revision=revision+1 WHERE uri=?1 AND state!='done' AND ?2 IS ?2"),
    ("reprioritize", "UPDATE task SET queue_priority=?2,revision=revision+1 WHERE uri=?1 AND state NOT IN ('done','cancelled') AND ?2 IS ?2 AND ?3 IS ?3"),
    ("block", "UPDATE task SET state='blocked',error=?3,revision=revision+1 WHERE uri=?1 AND paused=0 AND state NOT IN ('done','cancelled') AND ?2 IS ?2"),
    ("retry", "UPDATE task SET state='backlog',owner=NULL,lease_until=NULL,error=NULL,revision=revision+1 WHERE uri=?1 AND paused=0 AND state='blocked' AND ?2 IS ?2 AND ?3 IS ?3"),
];

pub struct Store(pub Connection);

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS task(uri TEXT PRIMARY KEY, doc TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'backlog', workflow TEXT, step INTEGER NOT NULL DEFAULT 0, owner TEXT, lease_until INTEGER, branch TEXT, worktree TEXT, error TEXT, revision INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS dependency(task TEXT NOT NULL, blocker TEXT NOT NULL, PRIMARY KEY(task, blocker));
            CREATE TABLE IF NOT EXISTS receipt(task TEXT NOT NULL, step TEXT NOT NULL, gate TEXT NOT NULL, tree TEXT NOT NULL, ok INTEGER NOT NULL, output TEXT NOT NULL, at INTEGER NOT NULL, PRIMARY KEY(task, step, gate, tree));
            CREATE INDEX IF NOT EXISTS task_state ON task(state); CREATE INDEX IF NOT EXISTS dependency_blocker ON dependency(blocker);")?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version == 0 {
            connection.execute_batch("BEGIN; ALTER TABLE task ADD COLUMN paused INTEGER NOT NULL DEFAULT 0; ALTER TABLE task ADD COLUMN queue_priority INTEGER NOT NULL DEFAULT 0; PRAGMA user_version=1; COMMIT;")?;
        } else if version != 1 {
            bail!("unsupported database schema {version}");
        }
        Ok(Self(connection))
    }

    pub fn ingest(&mut self, tasks: &[Task]) -> Result<usize> {
        let tx = self.0.transaction()?;
        for task in tasks {
            if task.uri.trim().is_empty() || task.title.trim().is_empty() {
                bail!("task uri and title are required");
            }
            tx.execute(
                "INSERT INTO task(uri,doc) VALUES(?1,?2) ON CONFLICT(uri) DO UPDATE SET doc=excluded.doc, revision=task.revision+1",
                params![task.uri, serde_json::to_string(task)?],
            )?;
            tx.execute("DELETE FROM dependency WHERE task=?1", [&task.uri])?;
            for blocker in &task.depends_on {
                tx.execute("INSERT OR IGNORE INTO dependency(task,blocker) VALUES(?1,?2)", params![task.uri, blocker])?;
            }
        }
        let cycle: Option<i64> = tx.query_row("WITH RECURSIVE reach(start,node) AS (SELECT task,blocker FROM dependency UNION SELECT reach.start,d.blocker FROM reach JOIN dependency d ON d.task=reach.node) SELECT 1 FROM reach WHERE start=node LIMIT 1", [], |row| row.get(0)).optional()?;
        if cycle.is_some() {
            bail!("task dependency cycle");
        }
        tx.commit()?;
        Ok(tasks.len())
    }

    pub fn all(&self) -> Result<Vec<TaskRow>> {
        let mut statement = self
            .0
            .prepare("SELECT uri,doc,state,workflow,step,owner,lease_until,branch,worktree,error,revision,paused,queue_priority FROM task ORDER BY queue_priority DESC,uri")?;
        statement.query_map([], row)?.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    pub fn get(&self, uri: &str) -> Result<TaskRow> {
        self.0
            .query_row(
                "SELECT uri,doc,state,workflow,step,owner,lease_until,branch,worktree,error,revision,paused,queue_priority FROM task WHERE uri=?1",
                [uri],
                row,
            )
            .optional()?
            .with_context(|| format!("task not found: {uri}"))
    }

    pub fn ready(&self, task: &TaskRow) -> Result<bool> {
        let waiting: i64 = self.0.query_row(
            "SELECT count(*) FROM dependency d LEFT JOIN task b ON b.uri=d.blocker WHERE d.task=?1 AND coalesce(b.state,'missing')!='done'",
            [&task.task.uri],
            |row| row.get(0),
        )?;
        Ok(task.state == "backlog" && !task.paused && waiting == 0)
    }

    pub fn claim(&mut self, uris: &[String], owner: &str, lease_until: i64, workflows: &[Option<String>]) -> Result<Vec<TaskRow>> {
        let tx = self.0.transaction()?;
        for (uri, workflow) in uris.iter().zip(workflows) {
            let changed = tx.execute("UPDATE task SET state='running', owner=?2, lease_until=?3, workflow=coalesce(workflow,?4), error=NULL, revision=revision+1 WHERE uri=?1 AND state='backlog' AND paused=0", params![uri, owner, lease_until, workflow])?;
            if changed != 1 {
                bail!("task could not be claimed: {uri}");
            }
        }
        tx.commit()?;
        uris.iter().map(|uri| self.get(uri)).collect()
    }

    pub fn heartbeat(&self, uri: &str, owner: &str, lease_until: i64) -> Result<()> {
        if self.0.execute(
            "UPDATE task SET lease_until=?3, revision=revision+1 WHERE uri=?1 AND owner=?2 AND state IN ('running','blocked') AND paused=0",
            params![uri, owner, lease_until],
        )? != 1
        {
            bail!("task is not owned by {owner}");
        }
        Ok(())
    }

    pub fn execution(&self, uri: &str, expected: &str, state: &str, step: usize, owner: Option<&str>, error: Option<&str>) -> Result<()> {
        if self.0.execute("UPDATE task SET state=?3,step=?4,owner=?5,lease_until=CASE WHEN ?5 IS NULL THEN NULL ELSE lease_until END,error=?6,revision=revision+1 WHERE uri=?1 AND state=?2 AND paused=0", params![uri, expected, state, step as i64, owner, error])? != 1 { bail!("task changed or is paused: {uri}"); }
        Ok(())
    }

    pub fn workspace(&self, uri: &str, expected: &str, branch: Option<&str>, worktree: Option<&str>) -> Result<()> {
        if self.0.execute(
            "UPDATE task SET branch=?3,worktree=?4,revision=revision+1 WHERE uri=?1 AND state=?2 AND paused=0",
            params![uri, expected, branch, worktree],
        )? != 1
        {
            bail!("task changed or is paused: {uri}");
        }
        Ok(())
    }

    pub fn control(&self, uri: &str, action: &str, number: Option<i64>, reason: Option<&str>) -> Result<()> {
        let sql = CONTROLS
            .iter()
            .find(|(name, _)| *name == action)
            .map(|(_, sql)| *sql)
            .with_context(|| format!("unknown control {action}"))?;
        if self.0.execute(sql, params![uri, number, reason])? != 1 {
            bail!("task cannot {action}: {uri}");
        }
        Ok(())
    }

    pub fn receipt(&self, uri: &str, step: &str, gate: &str, tree: &str, ok: bool, output: &str) -> Result<()> {
        self.0.execute("INSERT INTO receipt(task,step,gate,tree,ok,output,at) VALUES(?1,?2,?3,?4,?5,?6,unixepoch()) ON CONFLICT(task,step,gate,tree) DO UPDATE SET ok=excluded.ok,output=excluded.output,at=excluded.at", params![uri, step, gate, tree, ok, output])?;
        Ok(())
    }

    pub fn proof(&self, uri: &str, step: &str, gate: &str, tree: &str) -> Result<Option<bool>> {
        self.0
            .query_row(
                "SELECT ok FROM receipt WHERE task=?1 AND step=?2 AND gate=?3 AND tree=?4",
                params![uri, step, gate, tree],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn expire(&self, now: i64) -> Result<usize> {
        Ok(self.0.execute("UPDATE task SET state='backlog',owner=NULL,lease_until=NULL,error='lease expired',revision=revision+1 WHERE state='running' AND paused=0 AND lease_until < ?1", [now])?)
    }
}

fn row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskRow> {
    let doc: String = row.get(1)?;
    let task =
        serde_json::from_str(&doc).map_err(|error| rusqlite::Error::FromSqlConversionFailure(doc.len(), rusqlite::types::Type::Text, Box::new(error)))?;
    Ok(TaskRow {
        task,
        state: row.get(2)?,
        paused: row.get::<_, i64>(11)? != 0,
        queue_priority: row.get(12)?,
        active_workflow: row.get(3)?,
        step: row.get::<_, i64>(4)? as usize,
        owner: row.get(5)?,
        lease_until: row.get(6)?,
        branch: row.get(7)?,
        worktree: row.get(8)?,
        error: row.get(9)?,
        revision: row.get::<_, i64>(10)? as u64,
    })
}
