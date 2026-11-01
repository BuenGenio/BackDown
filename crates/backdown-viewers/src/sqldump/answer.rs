//! The header's comments (tool, versions, host, database, date, charset), and the schema as the
//! viewer gets it.

use std::io::Seek;

use serde_json::{Value, json};

use crate::peek::Codec;

use super::*;

// ---- the header's comments, and the answer

impl Parser {
    pub(super) fn header_comment(&mut self) {
        let text = String::from_utf8_lossy(&self.note).trim().to_string();
        let at = self.note_at;
        if text.is_empty() {
            return;
        }
        let t = text.as_str();
        let val =
            |s: &str| Some(s.trim_start_matches(':').trim().to_string()).filter(|v| !v.is_empty());
        if let Some(d) = t.strip_prefix("Dump completed on") {
            self.f.completed = val(d);
            self.f.footer = true;
            return;
        }
        if t.starts_with("PostgreSQL database dump complete")
            || t.starts_with("PostgreSQL database cluster dump complete")
        {
            self.f.footer = true;
            return;
        }
        if let Some(d) = t.strip_prefix("Completed on") {
            self.f.completed = val(d);
            return;
        }
        if let Some(r) = t.strip_prefix("Database \"")
            && let Some((db, _)) = r.split_once('"')
        {
            push_unique(&mut self.f.databases, db);
            return;
        }
        if at > HEADER_ZONE {
            return;
        }
        let tool_line = |f: &mut Found, tool: &str, rest: &str| {
            // "10.13  Distrib 8.0.36, for Linux (x86_64)"
            f.tool = Some(tool.into());
            let mut w = rest.split_whitespace();
            f.version = w.next().map(str::to_string);
            if w.next() == Some("Distrib") {
                f.client = w.next().map(|v| v.trim_end_matches(',').to_string());
            }
        };
        if let Some(r) = t.strip_prefix("MySQL dump") {
            self.hint(Dialect::Mysql);
            tool_line(&mut self.f, "mysqldump", r);
        } else if let Some(r) = t.strip_prefix("MariaDB dump") {
            self.hint(Dialect::Mysql);
            tool_line(&mut self.f, "mariadb-dump", r);
        } else if let Some(r) = t.strip_prefix("Host:") {
            let (host, db) = r.split_once("Database:").unwrap_or((r, ""));
            self.f.host = val(host);
            if let Some(db) = val(db) {
                self.f.database = Some(db.trim_matches('`').to_string());
            }
        } else if let Some(r) = t.strip_prefix("Server version") {
            self.hint(Dialect::Mysql);
            self.f.server = val(r);
        } else if t.starts_with("phpMyAdmin SQL Dump") {
            self.hint(Dialect::Mysql);
            self.f.tool = Some("phpMyAdmin".into());
        } else if let Some(v) = t.strip_prefix("version ")
            && self.f.tool.as_deref() == Some("phpMyAdmin")
        {
            self.f.version = val(v);
        } else if let Some(v) = t.strip_prefix("Generation Time:") {
            self.f.dumped = val(v);
        } else if let Some(v) = t.strip_prefix("Database:") {
            if let Some(db) = val(v) {
                self.f.database = Some(db.trim_matches('`').to_string());
            }
        } else if let Some(r) = t.strip_prefix("Adminer ") {
            // "Adminer 4.8.1 MySQL 8.0.36 dump"
            let r = r.trim_end_matches("dump").trim();
            let (v, server) = r.split_once(' ').unwrap_or((r, ""));
            self.f.tool = Some("Adminer".into());
            self.f.version = Some(v.into());
            if server.starts_with("MySQL") || server.starts_with("MariaDB") {
                self.hint(Dialect::Mysql);
            } else if server.starts_with("PostgreSQL") {
                self.hint(Dialect::Postgres);
            }
            self.f.server = val(server);
        } else if let Some(v) = t.strip_prefix("HeidiSQL Version:") {
            self.f.tool = Some("HeidiSQL".into());
            self.f.version = val(v);
        } else if t == "PostgreSQL database dump" {
            self.hint(Dialect::Postgres);
            self.f.tool.get_or_insert_with(|| "pg_dump".into());
        } else if t == "PostgreSQL database cluster dump" {
            self.hint(Dialect::Postgres);
            self.f.tool = Some("pg_dumpall".into());
        } else if let Some(v) = t.strip_prefix("Dumped from database version") {
            self.f.server = val(v).map(|v| format!("PostgreSQL {v}"));
        } else if let Some(v) = t
            .strip_prefix("Dumped by pg_dump version")
            .or_else(|| t.strip_prefix("Dumped by pg_dumpall version"))
        {
            self.f.version = val(v);
        } else if let Some(v) = t.strip_prefix("Started on") {
            self.f.dumped = val(v);
        }
    }

    // ---- the answer

    pub(super) fn json(&mut self, done: bool) -> Value {
        let read = self
            .raw
            .stream_position()
            .unwrap_or(self.size)
            .min(self.size);
        let f = &self.f;
        let mut facts: Vec<[String; 2]> = Vec::new();
        let mut fact = |k: &str, v: Option<String>| {
            if let Some(v) = v.filter(|v| !v.is_empty()) {
                facts.push([k.into(), v]);
            }
        };
        let client = f.client.as_ref().map(|c| {
            let product = if f.tool.as_deref() == Some("mariadb-dump") || c.contains("MariaDB") {
                "MariaDB"
            } else {
                "MySQL"
            };
            format!(" (with {product} {})", c.trim_end_matches("-MariaDB"))
        });
        fact(
            "Made by",
            f.tool.as_ref().map(|t| {
                format!(
                    "{t}{}{}",
                    f.version
                        .as_ref()
                        .map(|v| format!(" {v}"))
                        .unwrap_or_default(),
                    client.unwrap_or_default()
                )
            }),
        );
        fact("Server", f.server.clone());
        fact("Host", f.host.clone());
        fact(
            if f.databases.len() > 1 {
                "Databases"
            } else {
                "Database"
            },
            if f.databases.is_empty() {
                f.database.clone()
            } else {
                Some(f.databases.join(", "))
            },
        );
        fact("Dumped", f.dumped.clone().or_else(|| f.completed.clone()));
        if f.dumped.is_some() && f.completed.is_some() {
            fact("Completed", f.completed.clone());
        }
        fact("Character set", f.charset.clone());

        let tables: Vec<Value> = f
            .tables
            .iter()
            .filter(|t| !t.hidden)
            .map(|t| self.table_json(t))
            .collect();
        let mut schemas: Vec<String> = f.schemas.clone();
        for t in f.tables.iter().filter(|t| !t.hidden) {
            push_unique(&mut schemas, &key_of(&t.database, &t.schema, ""));
        }
        schemas.retain(|s| !s.is_empty());
        let objs = |list: &[Obj]| -> Vec<Value> {
            list.iter()
                .map(|o| json!({ "schema": o.schema, "name": o.name, "kind": o.kind, "on": o.on }))
                .collect()
        };
        let mut notes = Vec::new();
        if f.more_tables > 0 {
            notes.push(format!(
                "{} more tables aren't listed (the first {MAX_TABLES} are).",
                f.more_tables
            ));
        }
        if f.more_objects > 0 {
            notes.push(format!(
                "{} more views, triggers or routines aren't listed.",
                f.more_objects
            ));
        }
        let mut unfinished = false;
        if done && self.truncated.is_none() && !f.footer {
            let tool = f.tool.as_deref().unwrap_or("");
            let line = match tool {
                "mysqldump" | "mariadb-dump" => Some("“Dump completed on …”"),
                "pg_dump" => Some("“PostgreSQL database dump complete”"),
                "pg_dumpall" => Some("“PostgreSQL database cluster dump complete”"),
                "sqlite3 .dump" if !self.last_commit => Some("COMMIT"),
                _ => None,
            };
            if let Some(line) = line {
                unfinished = true;
                notes.push(format!(
                    "There's no {line} at the end: {tool} didn't finish, or the file was cut short."
                ));
            }
        }
        json!({
            "format": "sql",
            "dialect": self.dialect.name(),
            "tool": f.tool,
            "compression": match self.codec {
                Codec::Plain => None,
                Codec::Gzip => Some("gzip"),
                Codec::Bzip2 => Some("bzip2"),
                Codec::Xz => Some("xz"),
                Codec::Zstd => Some("zstd"),
            },
            "facts": facts,
            "databases": f.databases,
            "schemas": schemas,
            "tables": tables,
            "views": objs(&f.views),
            "triggers": objs(&f.triggers),
            "routines": objs(&f.routines),
            "events": objs(&f.events),
            "sequences": objs(&f.sequences),
            "other": objs(&f.other),
            "statements": f.statements,
            "size": self.size,
            "read": if done { self.size } else { read },
            "pos": self.base + self.lo as u64,
            "complete": done,
            "truncated": self.truncated.is_some() || unfinished,
            "cut": self.truncated,
            "notes": notes,
            "elapsed_ms": self.elapsed.as_millis() as u64,
            "more": Value::Null,
        })
    }

    pub(super) fn table_json(&self, t: &Table) -> Value {
        let columns: Vec<Value> = t
            .columns
            .iter()
            .map(|c| {
                json!({
                    "name": c.name, "type": c.ty, "null": c.nullable && !t.pk.contains(&c.name),
                    "default": c.default, "pk": t.pk.contains(&c.name), "auto": c.auto,
                    "extra": c.extra, "comment": c.comment,
                })
            })
            .collect();
        let fks: Vec<Value> = t
            .fks
            .iter()
            .map(|k| {
                let target = self.resolve(t, k);
                json!({
                    "name": k.name, "columns": k.columns, "ref": target,
                    "ref_columns": k.ref_columns, "on_delete": k.on_delete, "on_update": k.on_update,
                })
            })
            .collect();
        let indexes: Vec<Value> = t
            .indexes
            .iter()
            .map(|x| json!({ "name": x.name, "columns": x.columns, "kind": x.kind }))
            .collect();
        json!({
            "key": key_of(&t.database, &t.schema, &t.name),
            "schema": key_of(&t.database, &t.schema, ""),
            "name": t.name,
            "defined": t.defined,
            "columns": columns,
            "primary_key": t.pk,
            "indexes": indexes,
            "foreign_keys": fks,
            "checks": t.checks,
            "rows": t.rows,
            "uncounted": t.uncounted,
            "bytes": t.bytes,
            "statements": t.statements,
            "copy": t.copy,
            "engine": t.engine,
            "charset": t.charset,
            "comment": t.comment,
            "note": t.note,
            "sql": t.sql,
        })
    }

    /// A foreign key's table, as the key of a table in the dump when there is one.
    pub(super) fn resolve(&self, from: &Table, k: &Fk) -> String {
        let schema = k.schema.clone().unwrap_or_else(|| from.schema.clone());
        let exact = (from.database.clone(), schema.clone(), k.table.clone());
        if self.f.by_key.contains_key(&exact) {
            return key_of(&exact.0, &exact.1, &exact.2);
        }
        let near = self.f.tables.iter().find(|t| {
            t.database == from.database
                && t.name.eq_ignore_ascii_case(&k.table)
                && (k.schema.is_none() || t.schema.eq_ignore_ascii_case(&schema))
        });
        match near {
            Some(t) => key_of(&t.database, &t.schema, &t.name),
            None => key_of(&from.database, &schema, &k.table),
        }
    }
}
