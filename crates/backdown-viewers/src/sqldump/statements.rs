//! What each statement defines: tables and their columns, keys, indexes and foreign keys (CREATE,
//! ALTER, COMMENT ON), the rows an INSERT or COPY holds, and the settings a dump changes as it goes.

use super::*;

// ---- statements

impl Parser {
    pub(super) fn statement(&mut self, st: &Stmt, len: u64) {
        self.f.statements += 1;
        let first = st.first();
        self.last_commit = first == b"commit";
        if matches!(first, b"insert" | b"replace") {
            return self.insert(st, len);
        }
        let text = String::from_utf8_lossy(&st.head);
        let t = tokenize(&text, self.backslashes());
        let Some(w) = t.first().filter(|x| x.k == K::Word) else {
            return;
        };
        match w.v.to_ascii_lowercase().as_str() {
            "create" => self.create(&t, &text),
            "alter" => self.alter(&t, &text),
            "copy" => self.copy_start(&t, len),
            "comment" => self.comment_on(&t),
            "use" => {
                self.hint(Dialect::Mysql);
                if let Some(x) = t.get(1).filter(|x| x.named()) {
                    self.db = x.v.clone();
                    push_unique(&mut self.f.databases, &x.v);
                }
            }
            "set" => self.set(&t),
            "pragma" => {
                self.hint(Dialect::Sqlite);
                if self.f.statements == 1 && self.dialect == Dialect::Sqlite {
                    self.f.tool.get_or_insert_with(|| "sqlite3 .dump".into());
                }
            }
            "lock" | "unlock" => self.hint(Dialect::Mysql),
            "select" if text.contains("pg_catalog.") => self.hint(Dialect::Postgres),
            _ => {}
        }
    }

    pub(super) fn ident(&self, t: &Tok) -> String {
        if t.k == K::Word && self.dialect == Dialect::Postgres {
            t.v.to_lowercase()
        } else {
            t.v.clone()
        }
    }

    /// A possibly qualified name at `i`: (schema, name, the index after it).
    pub(super) fn qname(&self, t: &[Tok], i: usize) -> Option<(Option<String>, String, usize)> {
        let mut parts = Vec::new();
        let mut j = i;
        while let Some(x) = t.get(j).filter(|x| x.named()) {
            parts.push(self.ident(x));
            j += 1;
            if pt(t, j, b'.') && t.get(j + 1).is_some_and(Tok::named) {
                j += 1;
            } else {
                break;
            }
        }
        let name = parts.pop()?;
        Some((parts.pop(), name, j))
    }

    pub(super) fn schema_of(&self, given: Option<String>) -> String {
        given.unwrap_or_else(|| match self.dialect {
            Dialect::Postgres => self.search_path.clone().unwrap_or_else(|| "public".into()),
            Dialect::Sqlite => String::new(),
            Dialect::Mysql | Dialect::Unknown => {
                if self.db.is_empty() {
                    self.f.database.clone().unwrap_or_default()
                } else {
                    self.db.clone()
                }
            }
        })
    }

    /// The pg_dumpall database a table is in ("" for everything else).
    pub(super) fn database(&self) -> String {
        if self.dialect == Dialect::Postgres {
            self.db.clone()
        } else {
            String::new()
        }
    }

    pub(super) fn table(&mut self, schema: Option<String>, name: String) -> Option<usize> {
        let key = (self.database(), self.schema_of(schema), name);
        if let Some(&i) = self.f.by_key.get(&key) {
            return Some(i);
        }
        if self.f.tables.len() >= MAX_TABLES {
            self.f.more_tables += 1;
            return None;
        }
        let i = self.f.tables.len();
        self.f.tables.push(Table {
            database: key.0.clone(),
            schema: key.1.clone(),
            name: key.2.clone(),
            ..Table::default()
        });
        self.f.by_key.insert(key, i);
        Some(i)
    }

    pub(super) fn existing(&self, schema: Option<String>, name: &str) -> Option<usize> {
        let key = (self.database(), self.schema_of(schema), name.to_string());
        self.f.by_key.get(&key).copied()
    }

    pub(super) fn insert(&mut self, st: &Stmt, len: u64) {
        let cached = self
            .last_insert
            .as_ref()
            .filter(|(h, _)| *h == st.head)
            .map(|(_, i)| *i);
        let (idx, rows) = match cached {
            Some(i) => (Some(i), st.tuples),
            None => {
                let text = String::from_utf8_lossy(&st.head);
                let t = tokenize(&text, self.backslashes());
                let mut i = 1;
                while t.get(i).is_some_and(|x| {
                    ["low_priority", "delayed", "high_priority", "ignore"]
                        .iter()
                        .any(|k| x.is(k))
                }) {
                    i += 1;
                }
                if kw(&t, i, "or") {
                    i += 2; // SQLite: INSERT OR REPLACE
                }
                if kw(&t, i, "into") {
                    i += 1;
                }
                let Some((schema, name, mut j)) = self.qname(&t, i) else {
                    return;
                };
                if self.dialect == Dialect::Sqlite && name.starts_with("sqlite_") {
                    return; // sqlite_sequence and the like: SQLite's own bookkeeping
                }
                let idx = self.table(schema, name);
                let rows = if st.values {
                    if let Some(i) = idx {
                        self.last_insert = Some((st.head.clone(), i));
                    }
                    let default = t.len() >= 2 && t[t.len() - 2].is("default");
                    st.tuples.max(u64::from(default))
                } else {
                    if pt(&t, j, b'(') {
                        j = close(&t, j);
                    }
                    if kw(&t, j, "set") {
                        1
                    } else {
                        if let Some(tab) = idx.and_then(|i| self.f.tables.get_mut(i)) {
                            tab.uncounted = true;
                        }
                        0
                    }
                };
                (idx, rows)
            }
        };
        if let Some(tab) = idx.and_then(|i| self.f.tables.get_mut(i)) {
            tab.rows += rows;
            tab.bytes += len;
            tab.statements += 1;
        }
    }

    pub(super) fn copy_start(&mut self, t: &[Tok], len: u64) {
        let Some((schema, name, mut j)) = self.qname(t, 1) else {
            return;
        };
        if pt(t, j, b'(') {
            j = close(t, j);
        }
        if !(kw(t, j, "from") && kw(t, j + 1, "stdin")) {
            return;
        }
        self.hint(Dialect::Postgres);
        let idx = self.table(schema, name);
        if let Some(tab) = idx.and_then(|i| self.f.tables.get_mut(i)) {
            tab.copy = true;
            tab.statements += 1;
            tab.bytes += len;
        }
        self.copy = idx;
        self.lex = Lex::Copy(false);
    }

    pub(super) fn set(&mut self, t: &[Tok]) {
        let Some(what) = t.get(1) else {
            return;
        };
        if what.p(b'@') {
            return self.hint(Dialect::Mysql);
        }
        let value = |from: usize| -> Option<String> {
            let mut i = from;
            if pt(t, i, b'=') || kw(t, i, "to") {
                i += 1;
            }
            t.get(i).map(|x| x.v.clone())
        };
        match what.v.to_ascii_lowercase().as_str() {
            "names" => {
                self.hint(Dialect::Mysql);
                self.f.charset = value(2);
            }
            "character" if kw(t, 2, "set") => self.f.charset = value(3),
            "charset" => self.f.charset = value(2),
            "client_encoding" => {
                self.hint(Dialect::Postgres);
                self.f.charset = value(2);
            }
            "standard_conforming_strings" => {
                self.hint(Dialect::Postgres);
                self.pg_bs = value(2).is_some_and(|v| v.eq_ignore_ascii_case("off"));
            }
            "search_path" => {
                self.hint(Dialect::Postgres);
                let mut i = 2;
                if pt(t, i, b'=') || kw(t, i, "to") {
                    i += 1;
                }
                self.search_path = t
                    .get(i)
                    .filter(|x| {
                        x.named() && !x.v.is_empty() && !x.v.eq_ignore_ascii_case("pg_catalog")
                    })
                    .map(|x| self.ident(x));
            }
            "statement_timeout"
            | "lock_timeout"
            | "idle_in_transaction_session_timeout"
            | "transaction_timeout"
            | "check_function_bodies"
            | "xmloption"
            | "client_min_messages"
            | "row_security"
            | "default_tablespace"
            | "default_table_access_method"
            | "default_with_oids" => self.hint(Dialect::Postgres),
            "sql_mode" | "foreign_key_checks" | "unique_checks" | "time_zone" | "sql_notes"
            | "autocommit" => self.hint(Dialect::Mysql),
            _ => {}
        }
    }

    pub(super) fn comment_on(&mut self, t: &[Tok]) {
        if !kw(t, 1, "on") {
            return;
        }
        let Some(is) = t.iter().position(|x| x.is("is")) else {
            return;
        };
        let Some(text) = t.get(is + 1).filter(|x| x.k == K::Str).map(|x| x.v.clone()) else {
            return;
        };
        let mut parts: Vec<String> = t[3..is]
            .iter()
            .filter(|x| x.named())
            .map(|x| self.ident(x))
            .collect();
        if kw(t, 2, "table") {
            let Some(name) = parts.pop() else {
                return;
            };
            if let Some(i) = self.existing(parts.pop(), &name) {
                self.f.tables[i].comment = Some(text);
            }
        } else if kw(t, 2, "column") && parts.len() >= 2 {
            let col = parts.pop().unwrap_or_default();
            let name = parts.pop().unwrap_or_default();
            if let Some(i) = self.existing(parts.pop(), &name)
                && let Some(c) = self.f.tables[i].columns.iter_mut().find(|c| c.name == col)
            {
                c.comment = Some(text);
            }
        }
    }

    pub(super) fn create(&mut self, t: &[Tok], src: &str) {
        let mut i = 1;
        let mut kind = "index";
        let mut materialized = false;
        let what = loop {
            let Some(x) = t.get(i).filter(|_| i < 40) else {
                return;
            };
            if x.k == K::Word {
                let w = x.v.to_ascii_lowercase();
                match w.as_str() {
                    "unique" => kind = "unique",
                    "fulltext" => kind = "fulltext",
                    "spatial" => kind = "spatial",
                    "materialized" => materialized = true,
                    "table" | "view" | "index" | "trigger" | "procedure" | "function"
                    | "aggregate" | "event" | "sequence" | "schema" | "database" | "type"
                    | "domain" | "extension" => break w,
                    _ => {}
                }
            }
            i += 1;
        };
        i += 1;
        loop {
            if kw(t, i, "if") && kw(t, i + 1, "not") && kw(t, i + 2, "exists") {
                i += 3;
            } else if kw(t, i, "concurrently") {
                i += 1;
            } else {
                break;
            }
        }
        match what.as_str() {
            "table" => self.create_table(t, src, i),
            "index" => self.create_index(t, src, i, kind),
            "database" | "schema" => {
                if let Some(x) = t.get(i).filter(|x| x.named() && !x.is("authorization")) {
                    let name = self.ident(x);
                    if what == "database" || self.dialect == Dialect::Mysql {
                        push_unique(&mut self.f.databases, &name);
                    } else {
                        push_unique(&mut self.f.schemas, &name);
                    }
                }
            }
            _ => {
                let Some((schema, name, j)) = self.qname(t, i) else {
                    return;
                };
                let schema = self.schema_of(schema);
                let kind = if what == "view" && materialized {
                    "materialized view".to_string()
                } else {
                    what.clone()
                };
                if what == "view" {
                    // mysqldump first creates a stand-in (a table, or a view of constants)
                    let key = (self.database(), schema.clone(), name.clone());
                    if let Some(&ti) = self.f.by_key.get(&key) {
                        let tab = &mut self.f.tables[ti];
                        if tab.rows == 0 && tab.statements == 0 {
                            tab.hidden = true;
                        }
                    }
                }
                let on = if what == "trigger" {
                    t[j.min(t.len())..]
                        .iter()
                        .position(|x| x.is("on"))
                        .and_then(|k| self.qname(t, j + k + 1))
                        .map(|(s, n, _)| key_of(&self.database(), &self.schema_of(s), &n))
                } else {
                    None
                };
                let f = &mut self.f;
                let list = match what.as_str() {
                    "view" => &mut f.views,
                    "trigger" => &mut f.triggers,
                    "procedure" | "function" | "aggregate" => &mut f.routines,
                    "event" => &mut f.events,
                    "sequence" => &mut f.sequences,
                    _ => &mut f.other,
                };
                if list
                    .iter()
                    .any(|o| o.schema == schema && o.name == name && o.kind == kind)
                {
                    return;
                }
                if list.len() >= MAX_OBJECTS {
                    f.more_objects += 1;
                    return;
                }
                list.push(Obj {
                    schema,
                    name,
                    kind,
                    on,
                });
            }
        }
    }

    pub(super) fn create_table(&mut self, t: &[Tok], src: &str, i: usize) {
        let Some((schema, name, mut i)) = self.qname(t, i) else {
            return;
        };
        let Some(ti) = self.table(schema, name) else {
            return;
        };
        let mut tab = std::mem::take(&mut self.f.tables[ti]);
        tab.defined = true;
        tab.hidden = false;
        tab.columns.clear();
        tab.pk.clear();
        tab.indexes.clear();
        tab.fks.clear();
        tab.checks = 0;
        tab.note = None;
        if kw(t, i, "partition")
            && kw(t, i + 1, "of")
            && let Some((s, n, j)) = self.qname(t, i + 2)
        {
            tab.note = Some(format!(
                "a partition of {}",
                key_of("", &self.schema_of(s), &n)
            ));
            i = j;
        }
        if pt(t, i, b'(') {
            let e = close(t, i);
            for (s, e) in commas(t, i + 1, inner_end(t, i, e)) {
                self.definition(&mut tab, t, src, s, e);
            }
            i = e;
            self.table_options(&mut tab, t, i);
        } else if kw(t, i, "as") {
            tab.note = Some("made from a query (CREATE TABLE … AS)".into());
        } else if kw(t, i, "like")
            && let Some((s, n, _)) = self.qname(t, i + 1)
        {
            tab.note = Some(format!("like {}", key_of("", &self.schema_of(s), &n)));
        }
        if self.f.sql_kept < SQL_TOTAL {
            let s = clip(src.trim(), SQL_CAP);
            self.f.sql_kept += s.len();
            tab.sql = Some(s);
        }
        self.f.tables[ti] = tab;
    }

    pub(super) fn table_options(&self, tab: &mut Table, t: &[Tok], mut i: usize) {
        let value = |i: usize| -> (Option<String>, usize) {
            let j = if pt(t, i, b'=') { i + 1 } else { i };
            (t.get(j).map(|x| x.v.clone()), j + 1)
        };
        while i < t.len() {
            if kw(t, i, "engine") || kw(t, i, "type") {
                (tab.engine, i) = value(i + 1);
            } else if kw(t, i, "charset") {
                (tab.charset, i) = value(i + 1);
            } else if kw(t, i, "character") && kw(t, i + 1, "set") {
                (tab.charset, i) = value(i + 2);
            } else if kw(t, i, "comment") {
                (tab.comment, i) = value(i + 1);
            } else if kw(t, i, "inherits")
                && let Some((s, n, j)) = self.qname(t, i + 2)
            {
                tab.note = Some(format!("inherits {}", key_of("", &self.schema_of(s), &n)));
                i = j;
            } else if kw(t, i, "partition") && kw(t, i + 1, "by") {
                tab.note.get_or_insert_with(|| "partitioned".into());
                i += 2;
            } else {
                i += 1;
            }
        }
    }

    pub(super) fn definition(&self, tab: &mut Table, t: &[Tok], src: &str, s: usize, e: usize) {
        let Some(x) = t.get(s) else {
            return;
        };
        let w = x.v.to_ascii_lowercase();
        let constraint = x.k == K::Word
            && match w.as_str() {
                "constraint" | "primary" | "foreign" | "check" | "exclude" | "unique" => true,
                // a column may be called key (SQLite); MySQL's KEY name (…) has its parenthesis
                "key" | "index" | "fulltext" | "spatial" => {
                    (s + 1..e.min(s + 4)).any(|j| pt(t, j, b'('))
                }
                _ => false,
            };
        if constraint {
            self.constraint(tab, t, src, s, e);
        } else if let Some(c) = self.column(tab, t, src, s, e) {
            tab.columns.push(c);
        }
    }

    pub(super) fn column(
        &self,
        tab: &mut Table,
        t: &[Tok],
        src: &str,
        s: usize,
        e: usize,
    ) -> Option<Column> {
        let x = t.get(s).filter(|x| x.named() || x.k == K::Str)?;
        let mut c = Column {
            name: self.ident(x),
            nullable: true,
            ..Column::default()
        };
        let mut j = s + 1;
        while j < e {
            if t[j].p(b'(') {
                j = close(t, j).min(e);
                continue;
            }
            if is_attr(t, j) {
                break;
            }
            j += 1;
        }
        c.ty = squash(&span(src, t, s + 1, j));
        while j < e {
            let x = &t[j];
            if x.p(b'(') {
                j = close(t, j).min(e);
                continue;
            }
            if x.k != K::Word {
                j += 1;
                continue;
            }
            match x.v.to_ascii_lowercase().as_str() {
                "not" if kw(t, j + 1, "null") => {
                    c.nullable = false;
                    j += 2;
                }
                "default" => {
                    let (d, k) = expr(src, t, j + 1, e);
                    c.default = Some(d);
                    j = k;
                }
                "primary" => {
                    c.nullable = false;
                    push_unique(&mut tab.pk, &c.name);
                    j += 1;
                }
                "unique" => {
                    tab.indexes.push(Index {
                        name: None,
                        columns: vec![c.name.clone()],
                        kind: "unique",
                    });
                    j += 1;
                }
                "references" => {
                    let (fk, k) = self.references(t, j + 1, e, None, vec![c.name.clone()]);
                    tab.fks.extend(fk);
                    j = k;
                }
                "check" => {
                    tab.checks += 1;
                    j += 1;
                }
                "constraint" => j += 2,
                "collate" => {
                    if let Some(v) = t.get(j + 1) {
                        c.extra.push(format!("collate {}", v.v));
                    }
                    j += 2;
                }
                "character" if kw(t, j + 1, "set") => {
                    if let Some(v) = t.get(j + 2) {
                        c.extra.push(format!("charset {}", v.v));
                    }
                    j += 3;
                }
                "charset" => {
                    if let Some(v) = t.get(j + 1) {
                        c.extra.push(format!("charset {}", v.v));
                    }
                    j += 2;
                }
                "auto_increment" | "autoincrement" | "identity" => {
                    c.auto = true;
                    j += 1;
                }
                "generated" | "as" => {
                    let mut k = j + 1;
                    while k < e && !kw(t, k, "as") && !pt(t, k, b'(') {
                        k += 1;
                    }
                    if kw(t, k, "as") {
                        k += 1;
                    }
                    if kw(t, k, "identity") {
                        c.auto = true;
                        j = k + 1;
                    } else if pt(t, k, b'(') {
                        let ke = close(t, k).min(e);
                        c.extra
                            .push(format!("generated as {}", squash(&span(src, t, k, ke))));
                        j = ke;
                    } else {
                        j = k.max(j + 1);
                    }
                }
                "stored" | "virtual" | "persistent" => {
                    if let Some(g) = c.extra.last_mut().filter(|g| g.starts_with("generated")) {
                        g.push(' ');
                        g.push_str(&x.v.to_ascii_lowercase());
                    }
                    j += 1;
                }
                "comment" => {
                    c.comment = t.get(j + 1).map(|v| v.v.clone());
                    j += 2;
                }
                "on" if kw(t, j + 1, "update") => {
                    let (d, k) = expr(src, t, j + 2, e);
                    c.extra.push(format!("on update {d}"));
                    j = k;
                }
                _ => j += 1,
            }
        }
        let ty = c.ty.to_ascii_lowercase();
        if c.default
            .as_deref()
            .is_some_and(|d| d.to_ascii_lowercase().starts_with("nextval("))
            || matches!(ty.as_str(), "serial" | "bigserial" | "smallserial")
        {
            c.auto = true;
        }
        Some(c)
    }

    /// A table constraint (or MySQL key) from `s` to `e`.
    pub(super) fn constraint(&self, tab: &mut Table, t: &[Tok], src: &str, s: usize, e: usize) {
        let mut i = s;
        let mut name = None;
        if kw(t, i, "constraint") {
            let named = t.get(i + 1).is_some_and(|x| {
                x.named()
                    && !["primary", "unique", "foreign", "check", "exclude"]
                        .iter()
                        .any(|k| x.is(k))
            });
            if named {
                name = t.get(i + 1).map(|x| self.ident(x));
                i += 2;
            } else {
                i += 1;
            }
        }
        let Some(x) = t.get(i) else {
            return;
        };
        let w = x.v.to_ascii_lowercase();
        i += 1;
        // [KEY|INDEX] [name] [USING method] (columns)
        let cols = |i: &mut usize, name: &mut Option<String>| -> Vec<String> {
            if kw(t, *i, "key") || kw(t, *i, "index") {
                *i += 1;
            }
            while *i < e && !pt(t, *i, b'(') {
                if kw(t, *i, "using") {
                    *i += 2;
                } else {
                    if name.is_none() && t[*i].named() {
                        *name = Some(self.ident(&t[*i]));
                    }
                    *i += 1;
                }
            }
            if pt(t, *i, b'(') {
                let (c, j) = self.col_list(src, t, *i);
                *i = j;
                c
            } else {
                Vec::new()
            }
        };
        match w.as_str() {
            "primary" => {
                let mut ignored = None;
                for c in cols(&mut i, &mut ignored) {
                    push_unique(&mut tab.pk, &c);
                }
            }
            "unique" | "key" | "index" | "fulltext" | "spatial" => {
                let kind = match w.as_str() {
                    "unique" => "unique",
                    "fulltext" => "fulltext",
                    "spatial" => "spatial",
                    _ => "index",
                };
                let columns = cols(&mut i, &mut name);
                tab.indexes.push(Index {
                    name,
                    columns,
                    kind,
                });
            }
            "foreign" => {
                let columns = cols(&mut i, &mut name);
                if kw(t, i, "references") {
                    let (fk, _) = self.references(t, i + 1, e, name, columns);
                    tab.fks.extend(fk);
                }
            }
            "check" => tab.checks += 1,
            _ => {}
        }
    }

    /// REFERENCES table [(columns)] [ON DELETE …] [ON UPDATE …] …, from `i` (after REFERENCES).
    pub(super) fn references(
        &self,
        t: &[Tok],
        i: usize,
        e: usize,
        name: Option<String>,
        columns: Vec<String>,
    ) -> (Option<Fk>, usize) {
        let Some((schema, table, mut j)) = self.qname(t, i) else {
            return (None, i);
        };
        let mut ref_columns = Vec::new();
        if pt(t, j, b'(') {
            (ref_columns, j) = self.col_list("", t, j);
        }
        let mut fk = Fk {
            name,
            columns,
            schema,
            table,
            ref_columns,
            on_delete: None,
            on_update: None,
        };
        while j < e {
            if kw(t, j, "on") && (kw(t, j + 1, "delete") || kw(t, j + 1, "update")) {
                let del = kw(t, j + 1, "delete");
                let mut k = j + 2;
                let mut action = Vec::new();
                while k < e && t[k].k == K::Word && !t[k].is("on") && !t[k].is("match") {
                    let a = t[k].v.to_ascii_lowercase();
                    if ![
                        "cascade", "restrict", "no", "action", "set", "null", "default",
                    ]
                    .contains(&a.as_str())
                    {
                        break;
                    }
                    action.push(a);
                    k += 1;
                }
                let action = Some(action.join(" ")).filter(|a| !a.is_empty());
                if del {
                    fk.on_delete = action;
                } else {
                    fk.on_update = action;
                }
                j = k;
            } else if kw(t, j, "match") {
                j += 2;
            } else if t[j].k == K::Word
                && [
                    "deferrable",
                    "not",
                    "initially",
                    "deferred",
                    "immediate",
                    "enforced",
                    "valid",
                ]
                .iter()
                .any(|k| t[j].is(k))
            {
                j += 1;
            } else {
                break;
            }
        }
        (Some(fk), j)
    }

    /// `( a, b(10), c DESC, lower(d) )` at `i`: the column names (expressions as written).
    pub(super) fn col_list(&self, src: &str, t: &[Tok], i: usize) -> (Vec<String>, usize) {
        let e = close(t, i);
        let mut out = Vec::new();
        for (s, e2) in commas(t, i + 1, inner_end(t, i, e)) {
            let x = &t[s];
            let one = e2 - s == 1
                || (e2 - s == 2 && (t[s + 1].is("asc") || t[s + 1].is("desc")))
                || (x.named() && pt(t, s + 1, b'(') && close(t, s + 1) >= e2);
            if x.named() && one {
                let mut name = self.ident(x);
                if pt(t, s + 1, b'(') && !src.is_empty() {
                    name.push_str(&squash(&span(src, t, s + 1, close(t, s + 1).min(e2))));
                }
                out.push(name);
            } else if !src.is_empty() {
                out.push(squash(&span(src, t, s, e2)));
            } else {
                out.push(
                    t[s..e2]
                        .iter()
                        .map(|x| x.v.as_str())
                        .collect::<Vec<_>>()
                        .join(" "),
                );
            }
        }
        (out, e)
    }

    pub(super) fn create_index(&mut self, t: &[Tok], src: &str, mut i: usize, kind: &'static str) {
        let mut name = None;
        if !kw(t, i, "on")
            && let Some((_, n, j)) = self.qname(t, i)
        {
            name = Some(n);
            i = j;
        }
        while i < t.len() && !kw(t, i, "on") {
            i += 1;
        }
        i += 1;
        if kw(t, i, "only") {
            i += 1;
        }
        let Some((schema, table, mut j)) = self.qname(t, i) else {
            return;
        };
        if kw(t, j, "using") {
            j += 2;
        }
        let columns = if pt(t, j, b'(') {
            self.col_list(src, t, j).0
        } else {
            Vec::new()
        };
        if let Some(ti) = self.table(schema, table) {
            self.f.tables[ti].indexes.push(Index {
                name,
                columns,
                kind,
            });
        }
    }

    pub(super) fn alter(&mut self, t: &[Tok], src: &str) {
        let mut i = 1;
        while t
            .get(i)
            .is_some_and(|x| x.is("online") || x.is("offline") || x.is("ignore"))
        {
            i += 1;
        }
        if !kw(t, i, "table") {
            return;
        }
        i += 1;
        loop {
            if kw(t, i, "if") && kw(t, i + 1, "exists") {
                i += 2;
            } else if kw(t, i, "only") {
                i += 1;
            } else {
                break;
            }
        }
        let Some((schema, name, i)) = self.qname(t, i) else {
            return;
        };
        let Some(ti) = self.table(schema, name) else {
            return;
        };
        let mut tab = std::mem::take(&mut self.f.tables[ti]);
        for (s, e) in commas(t, i, t.len()) {
            let Some(x) = t.get(s) else {
                continue;
            };
            let mut j = s + 1;
            if x.is("add") {
                if kw(t, j, "column") {
                    j += 1;
                }
                if kw(t, j, "if") && kw(t, j + 1, "not") && kw(t, j + 2, "exists") {
                    j += 3;
                }
                let starts_constraint = t.get(j).is_some_and(|y| {
                    y.k == K::Word
                        && [
                            "constraint",
                            "primary",
                            "unique",
                            "key",
                            "index",
                            "foreign",
                            "fulltext",
                            "spatial",
                            "check",
                        ]
                        .iter()
                        .any(|k| y.is(k))
                });
                if starts_constraint {
                    self.constraint(&mut tab, t, src, j, e);
                } else if let Some(c) = self.column(&mut tab, t, src, j, e) {
                    replace_column(&mut tab, None, c);
                }
            } else if x.is("modify") || x.is("change") {
                if kw(t, j, "column") {
                    j += 1;
                }
                let old = if x.is("change") {
                    j += 1;
                    t.get(j - 1).map(|y| self.ident(y))
                } else {
                    None
                };
                if let Some(c) = self.column(&mut tab, t, src, j, e) {
                    replace_column(&mut tab, old, c);
                }
            } else if x.is("alter") {
                if kw(t, j, "column") {
                    j += 1;
                }
                let Some(col) = t.get(j).map(|y| self.ident(y)) else {
                    continue;
                };
                let Some(c) = tab.columns.iter_mut().find(|c| c.name == col) else {
                    continue;
                };
                if kw(t, j + 1, "set") && kw(t, j + 2, "default") {
                    let d = expr(src, t, j + 3, e).0;
                    if d.to_ascii_lowercase().starts_with("nextval(") {
                        c.auto = true;
                    }
                    c.default = Some(d);
                } else if kw(t, j + 1, "drop") && kw(t, j + 2, "default") {
                    c.default = None;
                } else if kw(t, j + 1, "set") && kw(t, j + 2, "not") {
                    c.nullable = false;
                } else if kw(t, j + 1, "drop") && kw(t, j + 2, "not") {
                    c.nullable = true;
                } else if kw(t, j + 1, "add") && t[j..e].iter().any(|y| y.is("identity")) {
                    c.auto = true;
                }
            }
        }
        self.f.tables[ti] = tab;
    }
}
