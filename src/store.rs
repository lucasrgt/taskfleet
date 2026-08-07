use std::{collections::HashSet, fs, path::Path};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

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
        connection.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS task(uri TEXT PRIMARY KEY, doc TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'backlog', workflow TEXT, step INTEGER NOT NULL DEFAULT 0, owner TEXT, lease_until INTEGER, branch TEXT, worktree TEXT, error TEXT, revision INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS dependency(task TEXT NOT NULL, blocker TEXT NOT NULL, PRIMARY KEY(task, blocker));
            CREATE TABLE IF NOT EXISTS receipt(task TEXT NOT NULL, step TEXT NOT NULL, gate TEXT NOT NULL, tree TEXT NOT NULL, ok INTEGER NOT NULL, output TEXT NOT NULL, at INTEGER NOT NULL, PRIMARY KEY(task, step, gate, tree));
            CREATE TABLE IF NOT EXISTS artifact_blob(digest TEXT PRIMARY KEY, size_bytes INTEGER NOT NULL, media_type TEXT NOT NULL, storage_path TEXT NOT NULL, created_at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS task_receipt(digest TEXT PRIMARY KEY, task_uri TEXT NOT NULL, step_id TEXT NOT NULL, base_tree TEXT NOT NULL, result_tree TEXT NOT NULL, receipt_json TEXT NOT NULL, created_at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS task_artifact(task_uri TEXT NOT NULL, receipt_digest TEXT NOT NULL, name TEXT NOT NULL, blob_digest TEXT NOT NULL, role TEXT NOT NULL, PRIMARY KEY(task_uri, receipt_digest, name));
            CREATE TABLE IF NOT EXISTS receipt_dependency(receipt_digest TEXT NOT NULL, dependency_task_uri TEXT NOT NULL, dependency_receipt_digest TEXT NOT NULL, PRIMARY KEY(receipt_digest, dependency_task_uri));
            CREATE TABLE IF NOT EXISTS artifact_pin(digest TEXT PRIMARY KEY, note TEXT, created_at INTEGER NOT NULL);
            CREATE INDEX IF NOT EXISTS task_state ON task(state);
            CREATE INDEX IF NOT EXISTS dependency_blocker ON dependency(blocker);
            CREATE INDEX IF NOT EXISTS task_receipt_uri ON task_receipt(task_uri, created_at);",
        )?;
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
        let cycle: Option<i64> = tx
            .query_row(
                "WITH RECURSIVE reach(start,node) AS (SELECT task,blocker FROM dependency UNION SELECT reach.start,d.blocker FROM reach JOIN dependency d ON d.task=reach.node) SELECT 1 FROM reach WHERE start=node LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
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
            "SELECT count(*) FROM dependency d LEFT JOIN task b ON b.uri=d.blocker WHERE d.task=?1 AND coalesce(b.state,'missing') NOT IN ('candidate','done')",
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
        if self.0.execute("UPDATE task SET state=?3,step=?4,owner=?5,lease_until=CASE WHEN ?5 IS NULL THEN NULL ELSE lease_until END,error=?6,revision=revision+1 WHERE uri=?1 AND state=?2 AND paused=0", params![uri, expected, state, step as i64, owner, error])? != 1 {
            bail!("task changed or is paused: {uri}");
        }
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

    pub fn live_workspaces(&self) -> Result<Vec<String>> {
        let mut statement = self.0.prepare("SELECT worktree FROM task WHERE worktree IS NOT NULL")?;
        let paths = statement.query_map([], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(paths)
    }

    pub fn receipt(&self, uri: &str, step: &str, gate: &str, tree: &str, ok: bool, output: &str) -> Result<()> {
        self.0.execute(
            "INSERT INTO receipt(task,step,gate,tree,ok,output,at) VALUES(?1,?2,?3,?4,?5,?6,unixepoch()) ON CONFLICT(task,step,gate,tree) DO UPDATE SET ok=excluded.ok,output=excluded.output,at=excluded.at",
            params![uri, step, gate, tree, ok, output],
        )?;
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

    pub fn record_blob(&self, digest: &str, size: u64, media_type: &str, storage_path: &str) -> Result<()> {
        self.0.execute(
            "INSERT INTO artifact_blob(digest,size_bytes,media_type,storage_path,created_at) VALUES(?1,?2,?3,?4,unixepoch()) ON CONFLICT(digest) DO UPDATE SET size_bytes=excluded.size_bytes,media_type=excluded.media_type,storage_path=excluded.storage_path",
            params![digest, size as i64, media_type, storage_path],
        )?;
        Ok(())
    }

    pub fn blob(&self, digest: &str) -> Result<Option<(u64, String, String)>> {
        self.0
            .query_row(
                "SELECT size_bytes,media_type,storage_path FROM artifact_blob WHERE digest=?1",
                [digest],
                |row| Ok((row.get::<_, i64>(0)? as u64, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn pin_blob(&self, digest: &str, note: Option<&str>) -> Result<()> {
        self.0.execute(
            "INSERT INTO artifact_pin(digest,note,created_at) VALUES(?1,?2,unixepoch()) ON CONFLICT(digest) DO UPDATE SET note=excluded.note",
            params![digest, note],
        )?;
        Ok(())
    }

    pub fn publish_task_receipt(&self, digest: &str, receipt: &Value) -> Result<()> {
        let task_uri = text(receipt, "task_uri")?;
        let step_id = text(receipt, "step_id")?;
        let base_tree = text(&receipt["source"], "base_tree")?;
        let result_tree = text(&receipt["source"], "result_tree")?;
        let json = serde_json::to_string(receipt)?;
        let tx = self.0.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO task_receipt(digest,task_uri,step_id,base_tree,result_tree,receipt_json,created_at) VALUES(?1,?2,?3,?4,?5,?6,unixepoch()) ON CONFLICT(digest) DO UPDATE SET receipt_json=excluded.receipt_json",
            params![digest, task_uri, step_id, base_tree, result_tree, json],
        )?;
        tx.execute("DELETE FROM task_artifact WHERE receipt_digest=?1", [digest])?;
        tx.execute("DELETE FROM receipt_dependency WHERE receipt_digest=?1", [digest])?;
        if let Some(artifacts) = receipt["artifacts"].as_array() {
            for artifact in artifacts {
                tx.execute(
                    "INSERT INTO task_artifact(task_uri,receipt_digest,name,blob_digest,role) VALUES(?1,?2,?3,?4,?5)",
                    params![task_uri, digest, text(artifact, "name")?, text(artifact, "digest")?, text(artifact, "role")?],
                )?;
            }
        }
        if let Some(deps) = receipt["dependencies"].as_array() {
            for dep in deps {
                tx.execute(
                    "INSERT INTO receipt_dependency(receipt_digest,dependency_task_uri,dependency_receipt_digest) VALUES(?1,?2,?3)",
                    params![digest, text(dep, "task_uri")?, text(dep, "receipt_digest")?],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn task_receipt(&self, digest: &str) -> Result<Value> {
        let json: String = self
            .0
            .query_row("SELECT receipt_json FROM task_receipt WHERE digest=?1", [digest], |row| row.get(0))
            .optional()?
            .with_context(|| format!("task receipt not found: {digest}"))?;
        Ok(serde_json::from_str(&json)?)
    }

    pub fn latest_task_receipt(&self, task_uri: &str) -> Result<Option<(String, Value)>> {
        self.0
            .query_row(
                "SELECT digest,receipt_json FROM task_receipt WHERE task_uri=?1 ORDER BY created_at DESC, digest DESC LIMIT 1",
                [task_uri],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
            .map(|(digest, json)| Ok((digest, serde_json::from_str(&json)?)))
            .transpose()
    }

    pub fn receipt_dependencies(&self, digest: &str) -> Result<Vec<(String, String)>> {
        let mut statement = self
            .0
            .prepare("SELECT dependency_task_uri,dependency_receipt_digest FROM receipt_dependency WHERE receipt_digest=?1 ORDER BY dependency_task_uri")?;
        let rows = statement.query_map([digest], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<rusqlite::Result<Vec<_>>>().map_err(Into::into)
    }

    pub fn reachable_digests(&self, retention_seconds: i64, now: i64) -> Result<HashSet<String>> {
        let mut keep = HashSet::new();
        let mut statement = self.0.prepare("SELECT digest FROM artifact_pin")?;
        for digest in statement.query_map([], |row| row.get::<_, String>(0))? {
            keep.insert(digest?);
        }
        let cutoff = now - retention_seconds;
        let mut statement = self.0.prepare(
            "SELECT digest FROM task_receipt WHERE (?1 > 0 AND created_at >= ?2) OR task_uri IN (SELECT uri FROM task WHERE state IN ('backlog','running','blocked','candidate'))",
        )?;
        for digest in statement.query_map(params![retention_seconds, cutoff], |row| row.get::<_, String>(0))? {
            let digest = digest?;
            keep.insert(digest.clone());
            if let Ok(receipt) = self.task_receipt(&digest) {
                collect_receipt_digests(&receipt, &mut keep);
            }
            for (_, dep) in self.receipt_dependencies(&digest)? {
                keep.insert(dep);
            }
        }
        if retention_seconds > 0 {
            let mut statement = self.0.prepare("SELECT digest FROM artifact_blob WHERE created_at >= ?1")?;
            for digest in statement.query_map([cutoff], |row| row.get::<_, String>(0))? {
                keep.insert(digest?);
            }
        }
        Ok(keep)
    }

    pub fn is_pinned(&self, digest: &str) -> Result<bool> {
        Ok(self
            .0
            .query_row("SELECT 1 FROM artifact_pin WHERE digest=?1", [digest], |_| Ok(()))
            .optional()?
            .is_some())
    }

    pub fn delete_blob_rows(&self, keep: &HashSet<String>) -> Result<usize> {
        let mut removed = 0;
        let mut statement = self.0.prepare("SELECT digest FROM artifact_blob")?;
        let digests = statement.query_map([], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        for digest in digests {
            if !keep.contains(&digest) {
                self.0.execute("DELETE FROM artifact_blob WHERE digest=?1", [&digest])?;
                removed += 1;
            }
        }
        Ok(removed)
    }
}

fn collect_receipt_digests(receipt: &Value, keep: &mut HashSet<String>) {
    if let Some(artifacts) = receipt["artifacts"].as_array() {
        for artifact in artifacts {
            if let Some(digest) = artifact["digest"].as_str() {
                keep.insert(digest.to_owned());
            }
        }
    }
    if let Some(digest) = receipt["changes"]["patch_digest"].as_str() {
        keep.insert(digest.to_owned());
    }
    if let Some(proofs) = receipt["proofs"].as_array() {
        for proof in proofs {
            for key in ["command_digest", "stdout_digest", "stderr_digest"] {
                if let Some(digest) = proof[key].as_str() {
                    keep.insert(digest.to_owned());
                }
            }
        }
    }
    if let Some(digest) = receipt["fingerprint"]["input_digest"].as_str() {
        keep.insert(digest.to_owned());
    }
}

fn text<'a>(value: &'a Value, name: &str) -> Result<&'a str> {
    value[name].as_str().with_context(|| format!("missing {name}"))
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
