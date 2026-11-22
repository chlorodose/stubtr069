//! SQLite state: virtual data model, pending events, transfers and the ACS command log.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, Row, params};

use crate::fault::Fault;
use crate::model::Seed;
use crate::soap::{AttrSet, DownloadReq, ParamSet};
use crate::util::unix_now;

const SCHEMA: &str = "
PRAGMA journal_mode = WAL;
CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS parameters (
    name TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    type TEXT NOT NULL,
    writable INTEGER NOT NULL,
    notification INTEGER NOT NULL DEFAULT 0,
    access_list TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS object_tables (
    path TEXT PRIMARY KEY,
    next_instance INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS objects (
    path TEXT PRIMARY KEY
);
CREATE TABLE IF NOT EXISTS events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    code TEXT NOT NULL,
    command_key TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS downloads (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    command_key TEXT NOT NULL,
    file_type TEXT NOT NULL,
    url TEXT NOT NULL,
    username TEXT NOT NULL,
    password TEXT NOT NULL,
    file_size INTEGER NOT NULL,
    target_file_name TEXT NOT NULL,
    delay_seconds INTEGER NOT NULL,
    state TEXT NOT NULL,
    received_at INTEGER NOT NULL,
    start_time TEXT NOT NULL DEFAULT '0001-01-01T00:00:00Z',
    complete_time TEXT NOT NULL DEFAULT '0001-01-01T00:00:00Z',
    fault_code INTEGER NOT NULL DEFAULT 0,
    fault_string TEXT NOT NULL DEFAULT '',
    local_path TEXT NOT NULL DEFAULT '',
    size INTEGER NOT NULL DEFAULT 0,
    sha256 TEXT NOT NULL DEFAULT '',
    reported INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS commands (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    received_at TEXT NOT NULL,
    session INTEGER NOT NULL,
    rpc_id TEXT NOT NULL,
    method TEXT NOT NULL,
    summary TEXT NOT NULL,
    raw TEXT NOT NULL
);
";

pub struct Db {
    conn: Mutex<Connection>,
    root: String,
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub value: String,
    pub ty: String,
    pub writable: bool,
    pub notification: u8,
    pub access_list: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Event {
    pub id: i64,
    pub code: String,
    pub command_key: String,
}

#[derive(Debug, Clone)]
pub struct Download {
    pub id: i64,
    pub command_key: String,
    pub url: String,
    pub username: String,
    pub password: String,
    pub file_size: i64,
    pub target_file_name: String,
    pub delay_seconds: i64,
    pub received_at: i64,
    pub start_time: String,
    pub complete_time: String,
    pub fault_code: i64,
    pub fault_string: String,
}

const PARAM_COLS: &str = "name, value, type, writable, notification, access_list";
const DOWNLOAD_COLS: &str = "id, command_key, url, username, password, file_size, \
    target_file_name, delay_seconds, received_at, start_time, complete_time, fault_code, fault_string";

fn row_param(r: &Row) -> rusqlite::Result<Param> {
    let acl: String = r.get(5)?;
    Ok(Param {
        name: r.get(0)?,
        value: r.get(1)?,
        ty: r.get(2)?,
        writable: r.get::<_, i64>(3)? != 0,
        notification: r.get::<_, i64>(4)? as u8,
        access_list: acl
            .split(',')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
    })
}

fn row_download(r: &Row) -> rusqlite::Result<Download> {
    Ok(Download {
        id: r.get(0)?,
        command_key: r.get(1)?,
        url: r.get(2)?,
        username: r.get(3)?,
        password: r.get(4)?,
        file_size: r.get(5)?,
        target_file_name: r.get(6)?,
        delay_seconds: r.get(7)?,
        received_at: r.get(8)?,
        start_time: r.get(9)?,
        complete_time: r.get(10)?,
        fault_code: r.get(11)?,
        fault_string: r.get(12)?,
    })
}

/// Every proper `a.b.` prefix of a dotted name.
fn ancestors(name: &str) -> impl Iterator<Item = &str> {
    name.match_indices('.')
        .map(move |(i, _)| &name[..=i])
        .filter(move |p| p.len() < name.len())
}

fn normalize(ty: &str, value: &str) -> Option<String> {
    match ty {
        "xsd:boolean" => match value.trim() {
            "1" | "true" => Some("1".into()),
            "0" | "false" => Some("0".into()),
            _ => None,
        },
        "xsd:int" => value.trim().parse::<i32>().ok().map(|v| v.to_string()),
        "xsd:unsignedInt" => value.trim().parse::<u32>().ok().map(|v| v.to_string()),
        _ => Some(value.to_string()),
    }
}

impl Db {
    pub fn open(path: &Path, root: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
            root: root.to_string(),
        })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap()
    }

    pub fn root(&self) -> &str {
        &self.root
    }

    pub fn param_key_name(&self) -> String {
        format!("{}.ManagementServer.ParameterKey", self.root)
    }

    // ---- meta ----

    pub fn meta_get(&self, key: &str) -> Option<String> {
        self.conn()
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .optional()
            .ok()
            .flatten()
    }

    pub fn meta_set(&self, key: &str, value: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn next_session_id(&self) -> Result<i64> {
        let n = self
            .meta_get("session_seq")
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(0)
            + 1;
        self.meta_set("session_seq", &n.to_string())?;
        Ok(n)
    }

    // ---- data model ----

    fn insert_seeds(conn: &Connection, seeds: &[Seed]) -> Result<()> {
        let mut st = conn.prepare(
            "INSERT OR IGNORE INTO parameters (name, value, type, writable) VALUES (?1, ?2, ?3, ?4)",
        )?;
        for s in seeds {
            st.execute(params![s.name, s.value, s.ty, i64::from(s.writable)])?;
        }
        Ok(())
    }

    /// Adds seeds that are missing; existing values are kept.
    pub fn apply_seeds(&self, seeds: &[Seed]) -> Result<()> {
        Self::insert_seeds(&self.conn(), seeds)
    }

    /// Wipes the data model back to the given defaults.
    pub fn factory_reset(&self, seeds: &[Seed]) -> Result<()> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute_batch("DELETE FROM parameters; DELETE FROM objects; DELETE FROM object_tables;")?;
        Self::insert_seeds(&tx, seeds)?;
        tx.commit()?;
        Ok(())
    }

    /// Sets a value bypassing writability checks (used for CLI overrides).
    pub fn force_value(&self, name: &str, value: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE parameters SET value = ?1 WHERE name = ?2",
            params![value, name],
        )?;
        Ok(())
    }

    pub fn param(&self, name: &str) -> Option<Param> {
        self.conn()
            .query_row(
                &format!("SELECT {PARAM_COLS} FROM parameters WHERE name = ?1"),
                [name],
                row_param,
            )
            .optional()
            .ok()
            .flatten()
    }

    pub fn value(&self, name: &str) -> String {
        self.param(name).map(|p| p.value).unwrap_or_default()
    }

    fn all_params(conn: &Connection) -> Result<Vec<Param>, Fault> {
        let mut st = conn
            .prepare(&format!("SELECT {PARAM_COLS} FROM parameters ORDER BY name"))
            .map_err(Fault::internal)?;
        st.query_map([], row_param)
            .map_err(Fault::internal)?
            .collect::<Result<_, _>>()
            .map_err(Fault::internal)
    }

    /// Every addressable name (parameters and objects) with its writable flag.
    fn name_map(conn: &Connection) -> Result<BTreeMap<String, bool>, Fault> {
        let mut map = BTreeMap::new();
        for p in Self::all_params(conn)? {
            for a in ancestors(&p.name) {
                map.entry(a.to_string()).or_insert(false);
            }
            map.insert(p.name, p.writable);
        }
        for table in ["object_tables", "objects"] {
            let mut st = conn
                .prepare(&format!("SELECT path FROM {table}"))
                .map_err(Fault::internal)?;
            let paths: Vec<String> = st
                .query_map([], |r| r.get(0))
                .map_err(Fault::internal)?
                .collect::<Result<_, _>>()
                .map_err(Fault::internal)?;
            for path in paths {
                for a in ancestors(&path) {
                    map.entry(a.to_string()).or_insert(false);
                }
                map.insert(path, true);
            }
        }
        Ok(map)
    }

    pub fn get_values(&self, path: &str) -> Result<Vec<Param>, Fault> {
        let conn = self.conn();
        if path.is_empty() || path.ends_with('.') {
            if !path.is_empty() && !Self::name_map(&conn)?.contains_key(path) {
                return Err(Fault::invalid_name(path));
            }
            Ok(Self::all_params(&conn)?
                .into_iter()
                .filter(|p| p.name.starts_with(path))
                .collect())
        } else {
            conn.query_row(
                &format!("SELECT {PARAM_COLS} FROM parameters WHERE name = ?1"),
                [path],
                row_param,
            )
            .optional()
            .map_err(Fault::internal)?
            .map(|p| vec![p])
            .ok_or_else(|| Fault::invalid_name(path))
        }
    }

    pub fn names(&self, path: &str, next_level: bool) -> Result<Vec<(String, bool)>, Fault> {
        let map = Self::name_map(&self.conn())?;
        if !path.is_empty() && !path.ends_with('.') {
            if next_level {
                return Err(Fault::new(9003, "NextLevel must be false for a parameter path"));
            }
            return map
                .get(path)
                .map(|w| vec![(path.to_string(), *w)])
                .ok_or_else(|| Fault::invalid_name(path));
        }
        if !path.is_empty() && !map.contains_key(path) {
            return Err(Fault::invalid_name(path));
        }
        Ok(map
            .into_iter()
            .filter(|(n, _)| n.starts_with(path))
            .filter(|(n, _)| {
                !next_level || (n != path && !n[path.len()..].trim_end_matches('.').contains('.'))
            })
            .collect())
    }

    /// Atomically applies SetParameterValues. Returns the applied (name, value) pairs.
    pub fn set_values(
        &self,
        items: &[ParamSet],
        key: &str,
    ) -> Result<Vec<(String, String)>, Fault> {
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(Fault::internal)?;
        let mut fault = Fault::new(9003, "Invalid arguments");
        let mut applied = Vec::new();
        for it in items {
            let p = tx
                .query_row(
                    &format!("SELECT {PARAM_COLS} FROM parameters WHERE name = ?1"),
                    [&it.name],
                    row_param,
                )
                .optional()
                .map_err(Fault::internal)?;
            match p {
                None => fault.details.push((it.name.clone(), 9005, "Invalid parameter name".into())),
                Some(p) if !p.writable => fault.details.push((
                    it.name.clone(),
                    9008,
                    "Attempt to set a non-writable parameter".into(),
                )),
                Some(p) => match normalize(&p.ty, &it.value) {
                    Some(v) => applied.push((it.name.clone(), v)),
                    None => fault.details.push((it.name.clone(), 9007, "Invalid parameter value".into())),
                },
            }
        }
        if !fault.details.is_empty() {
            return Err(fault);
        }
        for (n, v) in &applied {
            tx.execute("UPDATE parameters SET value = ?1 WHERE name = ?2", params![v, n])
                .map_err(Fault::internal)?;
        }
        tx.execute(
            "UPDATE parameters SET value = ?1 WHERE name = ?2",
            params![key, self.param_key_name()],
        )
        .map_err(Fault::internal)?;
        tx.commit().map_err(Fault::internal)?;
        Ok(applied)
    }

    pub fn set_attrs(&self, items: &[AttrSet]) -> Result<(), Fault> {
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(Fault::internal)?;
        let params_all = Self::all_params(&tx)?;
        let names = Self::name_map(&tx)?;
        for it in items {
            if !names.contains_key(&it.name) {
                return Err(Fault::invalid_name(&it.name));
            }
            for p in params_all.iter().filter(|p| p.name.starts_with(&it.name)) {
                if it.notification_change {
                    tx.execute(
                        "UPDATE parameters SET notification = ?1 WHERE name = ?2",
                        params![i64::from(it.notification), p.name],
                    )
                    .map_err(Fault::internal)?;
                }
                if it.access_list_change {
                    tx.execute(
                        "UPDATE parameters SET access_list = ?1 WHERE name = ?2",
                        params![it.access_list.join(","), p.name],
                    )
                    .map_err(Fault::internal)?;
                }
            }
        }
        tx.commit().map_err(Fault::internal)?;
        Ok(())
    }

    pub fn get_attrs(&self, path: &str) -> Result<Vec<Param>, Fault> {
        self.get_values(path)
    }

    pub fn add_object(&self, path: &str, key: &str) -> Result<u32, Fault> {
        let last_numeric = path
            .trim_end_matches('.')
            .rsplit('.')
            .next()
            .is_some_and(|s| s.parse::<u32>().is_ok());
        let in_root = path.starts_with(&format!("{}.", self.root));
        if !path.ends_with('.') || !in_root || last_numeric {
            return Err(Fault::invalid_name(path));
        }
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(Fault::internal)?;
        // Fixed (non multi-instance) objects own parameters and cannot be instantiated.
        let fixed: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM parameters WHERE substr(name, 1, length(?1)) = ?1)",
                [path],
                |r| r.get(0),
            )
            .map_err(Fault::internal)?;
        if fixed {
            return Err(Fault::new(9005, format!("{path} is not a multi-instance object")));
        }
        let next: i64 = tx
            .query_row(
                "SELECT next_instance FROM object_tables WHERE path = ?1",
                [path],
                |r| r.get(0),
            )
            .optional()
            .map_err(Fault::internal)?
            .unwrap_or(1);
        tx.execute(
            "INSERT INTO object_tables (path, next_instance) VALUES (?1, ?2) \
             ON CONFLICT(path) DO UPDATE SET next_instance = excluded.next_instance",
            params![path, next + 1],
        )
        .map_err(Fault::internal)?;
        tx.execute(
            "INSERT INTO objects (path) VALUES (?1)",
            [format!("{path}{next}.")],
        )
        .map_err(Fault::internal)?;
        tx.execute(
            "UPDATE parameters SET value = ?1 WHERE name = ?2",
            params![key, self.param_key_name()],
        )
        .map_err(Fault::internal)?;
        tx.commit().map_err(Fault::internal)?;
        Ok(next as u32)
    }

    pub fn delete_object(&self, path: &str, key: &str) -> Result<(), Fault> {
        let mut conn = self.conn();
        let tx = conn.transaction().map_err(Fault::internal)?;
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM objects WHERE path = ?1)",
                [path],
                |r| r.get(0),
            )
            .map_err(Fault::internal)?;
        if !path.ends_with('.') || !exists {
            return Err(Fault::invalid_name(path));
        }
        tx.execute(
            "DELETE FROM objects WHERE substr(path, 1, length(?1)) = ?1",
            [path],
        )
        .map_err(Fault::internal)?;
        tx.execute(
            "DELETE FROM parameters WHERE substr(name, 1, length(?1)) = ?1",
            [path],
        )
        .map_err(Fault::internal)?;
        tx.execute(
            "UPDATE parameters SET value = ?1 WHERE name = ?2",
            params![key, self.param_key_name()],
        )
        .map_err(Fault::internal)?;
        tx.commit().map_err(Fault::internal)?;
        Ok(())
    }

    // ---- events ----

    pub fn add_event(&self, code: &str, command_key: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO events (code, command_key) VALUES (?1, ?2)",
            params![code, command_key],
        )?;
        Ok(())
    }

    /// Adds an event unless an identical one is still pending.
    pub fn add_event_unique(&self, code: &str, command_key: &str) -> Result<()> {
        self.conn().execute(
            "INSERT INTO events (code, command_key) SELECT ?1, ?2 \
             WHERE NOT EXISTS (SELECT 1 FROM events WHERE code = ?1 AND command_key = ?2)",
            params![code, command_key],
        )?;
        Ok(())
    }

    pub fn pending_events(&self) -> Result<Vec<Event>> {
        let conn = self.conn();
        let mut st = conn.prepare("SELECT id, code, command_key FROM events ORDER BY id")?;
        let rows = st.query_map([], |r| {
            Ok(Event {
                id: r.get(0)?,
                code: r.get(1)?,
                command_key: r.get(2)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn ack_events(&self, max_id: i64) -> Result<()> {
        self.conn()
            .execute("DELETE FROM events WHERE id <= ?1", [max_id])?;
        Ok(())
    }

    // ---- downloads ----

    pub fn insert_download(&self, d: &DownloadReq) -> Result<i64> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO downloads (command_key, file_type, url, username, password, file_size, \
             target_file_name, delay_seconds, state, received_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'queued', ?9)",
            params![
                d.command_key,
                d.file_type,
                d.url,
                d.username,
                d.password,
                d.file_size as i64,
                d.target_file_name,
                d.delay_seconds as i64,
                unix_now()
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn download(&self, id: i64) -> Option<Download> {
        self.conn()
            .query_row(
                &format!("SELECT {DOWNLOAD_COLS} FROM downloads WHERE id = ?1"),
                [id],
                row_download,
            )
            .optional()
            .ok()
            .flatten()
    }

    pub fn downloads_where(&self, cond: &str) -> Result<Vec<Download>> {
        let conn = self.conn();
        let mut st = conn.prepare(&format!(
            "SELECT {DOWNLOAD_COLS} FROM downloads WHERE {cond} ORDER BY id"
        ))?;
        let rows = st.query_map([], row_download)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn download_started(&self, id: i64, start: &str) -> Result<()> {
        self.conn().execute(
            "UPDATE downloads SET state = 'downloading', start_time = ?1 WHERE id = ?2",
            params![start, id],
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn download_finished(
        &self,
        id: i64,
        complete: &str,
        fault: Option<(u32, &str)>,
        local_path: &str,
        size: u64,
        sha256: &str,
    ) -> Result<()> {
        let (code, text) = fault.unwrap_or((0, ""));
        self.conn().execute(
            "UPDATE downloads SET state = ?1, complete_time = ?2, fault_code = ?3, fault_string = ?4, \
             local_path = ?5, size = ?6, sha256 = ?7 WHERE id = ?8",
            params![
                if fault.is_some() { "failed" } else { "completed" },
                complete,
                code,
                text,
                local_path,
                size as i64,
                sha256,
                id
            ],
        )?;
        Ok(())
    }

    pub fn download_reported(&self, id: i64) -> Result<()> {
        self.conn()
            .execute("UPDATE downloads SET reported = 1 WHERE id = ?1", [id])?;
        Ok(())
    }

    // ---- command log ----

    pub fn log_command(
        &self,
        received_at: &str,
        session: i64,
        rpc_id: &str,
        method: &str,
        summary: &str,
        raw: &str,
    ) -> Result<()> {
        self.conn().execute(
            "INSERT INTO commands (received_at, session, rpc_id, method, summary, raw) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![received_at, session, rpc_id, method, summary, raw],
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use clap::Parser;

    fn db() -> (Db, Config) {
        let cfg = Config::parse_from(["x"]);
        let db = Db::open(Path::new(":memory:"), "Device").unwrap();
        db.apply_seeds(&crate::model::seeds(&cfg, "http://1.2.3.4:7547/")).unwrap();
        (db, cfg)
    }

    fn set(name: &str, value: &str) -> ParamSet {
        ParamSet { name: name.into(), value: value.into() }
    }

    #[test]
    fn names_next_level() {
        let (db, _) = db();
        let top = db.names("Device.", true).unwrap();
        let names: Vec<_> = top.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"Device.DeviceInfo."));
        assert!(names.contains(&"Device.ManagementServer."));
        assert!(names.contains(&"Device.RootDataModelVersion"));
        assert!(!names.contains(&"Device.DeviceInfo.Manufacturer"));
        assert_eq!(db.names("", true).unwrap().len(), 1);
        assert!(db.names("Device.Nope.", true).is_err());
    }

    #[test]
    fn set_is_atomic_and_typed() {
        let (db, _) = db();
        let bad = [
            set("Device.DeviceInfo.ProvisioningCode", "abc"),
            set("Device.DeviceInfo.Manufacturer", "x"),
            set("Device.ManagementServer.PeriodicInformEnable", "maybe"),
            set("Device.Missing", "1"),
        ];
        let f = db.set_values(&bad, "k").unwrap_err();
        assert_eq!(f.details.len(), 3);
        assert_eq!(db.value("Device.DeviceInfo.ProvisioningCode"), "");
        let ok = [
            set("Device.DeviceInfo.ProvisioningCode", "abc"),
            set("Device.ManagementServer.PeriodicInformEnable", "true"),
        ];
        db.set_values(&ok, "key1").unwrap();
        assert_eq!(db.value("Device.DeviceInfo.ProvisioningCode"), "abc");
        assert_eq!(db.value("Device.ManagementServer.PeriodicInformEnable"), "1");
        assert_eq!(db.value("Device.ManagementServer.ParameterKey"), "key1");
    }

    #[test]
    fn object_lifecycle() {
        let (db, _) = db();
        assert!(db.add_object("Device.DeviceInfo.", "k").is_err());
        assert!(db.add_object("Device.Foo.1.", "k").is_err());
        assert_eq!(db.add_object("Device.Foo.", "k").unwrap(), 1);
        assert_eq!(db.add_object("Device.Foo.", "k").unwrap(), 2);
        let n = db.names("Device.Foo.", true).unwrap();
        assert_eq!(n.len(), 2);
        db.delete_object("Device.Foo.1.", "k2").unwrap();
        assert!(db.delete_object("Device.Foo.1.", "k2").is_err());
        assert_eq!(db.add_object("Device.Foo.", "k").unwrap(), 3);
        assert_eq!(db.value("Device.ManagementServer.ParameterKey"), "k");
    }
}
