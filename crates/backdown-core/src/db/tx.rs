//! Transactions that can't be left open. `Tx` begins one and, unless it's committed, rolls it back
//! when it's dropped, so an error part way (a `?`, a panic) leaves the connection as it was before.
//!
//! That matters most for the daemon, whose worker keeps one connection for every job: a transaction
//! a failed job left open there would make the next job's `BEGIN` fail ("cannot start a transaction
//! within a transaction"), and keep everything written after it uncommitted until the daemon stops.

use anyhow::Result;
use rusqlite::Connection;

/// When the write lock is taken.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lock {
    /// at the first write (`BEGIN`)
    Deferred,
    /// at once (`BEGIN IMMEDIATE`), so what's read under it can't change before it's written
    Immediate,
}

impl Lock {
    fn begin(self) -> &'static str {
        match self {
            Lock::Deferred => "BEGIN",
            Lock::Immediate => "BEGIN IMMEDIATE",
        }
    }
}

/// An open transaction on a connection: committed by `commit`, rolled back if dropped first.
#[must_use = "a transaction that isn't committed is rolled back when it's dropped"]
pub struct Tx<'c> {
    conn: &'c Connection,
    lock: Lock,
    /// this guard began the transaction (it didn't join the caller's), so it ends it
    own: bool,
    /// a transaction of this guard's is open, to be rolled back if the guard is dropped
    open: bool,
}

impl<'c> Tx<'c> {
    /// Begin a transaction. Like `BEGIN`, this fails if one is already open on `conn`.
    pub fn begin(conn: &'c Connection, lock: Lock) -> Result<Tx<'c>> {
        conn.execute_batch(lock.begin())?;
        Ok(Tx {
            conn,
            lock,
            own: true,
            open: true,
        })
    }

    /// Work inside the caller's transaction when one is open on `conn` (committing it or rolling
    /// it back is then the caller's, and this guard does neither), else in a new one, as `begin`.
    pub fn join(conn: &'c Connection, lock: Lock) -> Result<Tx<'c>> {
        if !conn.is_autocommit() {
            return Ok(Tx {
                conn,
                lock,
                own: false,
                open: false,
            });
        }
        Tx::begin(conn, lock)
    }

    /// Commit what's written so far and go on in a new transaction, for long jobs that keep their
    /// work a batch at a time. If the commit fails, the batch is rolled back when the guard drops.
    /// In a joined transaction it does nothing.
    pub fn checkpoint(&mut self) -> Result<()> {
        if !self.own {
            return Ok(());
        }
        self.conn.execute_batch("COMMIT")?;
        self.open = false;
        self.conn.execute_batch(self.lock.begin())?;
        self.open = true;
        Ok(())
    }

    /// Commit. If that fails, the transaction is rolled back (as the guard drops). In a joined
    /// transaction it does nothing: the caller commits.
    pub fn commit(mut self) -> Result<()> {
        if self.own {
            self.conn.execute_batch("COMMIT")?;
            self.open = false;
        }
        Ok(())
    }
}

impl Drop for Tx<'_> {
    fn drop(&mut self) {
        if self.open {
            // fails only when SQLite already rolled back by itself (a full disk, an I/O error)
            let _ = self.conn.execute_batch("ROLLBACK");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE t(v INTEGER);
             CREATE TABLE p(id INTEGER PRIMARY KEY);
             CREATE TABLE c(pid INTEGER REFERENCES p(id) DEFERRABLE INITIALLY DEFERRED);",
        )
        .unwrap();
        c
    }

    fn put(c: &Connection, v: i64) {
        c.execute("INSERT INTO t(v) VALUES (?1)", [v]).unwrap();
    }

    fn values(c: &Connection) -> Vec<i64> {
        let mut s = c.prepare("SELECT v FROM t ORDER BY v").unwrap();
        s.query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    fn fails_part_way(c: &Connection) -> Result<()> {
        let tx = Tx::begin(c, Lock::Deferred)?;
        put(c, 1);
        let failing: Result<()> = Err(anyhow::anyhow!("something failed"));
        failing?;
        tx.commit()
    }

    #[test]
    fn an_error_part_way_rolls_back_and_leaves_the_connection_usable() {
        let c = conn();
        assert!(fails_part_way(&c).is_err());
        assert!(c.is_autocommit(), "no transaction is left open");
        assert!(values(&c).is_empty());
        // and the next one begins as usual
        let tx = Tx::begin(&c, Lock::Immediate).unwrap();
        put(&c, 2);
        tx.commit().unwrap();
        assert_eq!(values(&c), [2]);
    }

    #[test]
    fn checkpoints_keep_each_finished_batch() {
        let c = conn();
        {
            let mut tx = Tx::begin(&c, Lock::Deferred).unwrap();
            put(&c, 1);
            tx.checkpoint().unwrap();
            put(&c, 2);
            tx.checkpoint().unwrap();
            put(&c, 3);
            // dropped: the batch it was in goes
        }
        assert!(c.is_autocommit());
        assert_eq!(values(&c), [1, 2]);
    }

    #[test]
    fn a_failed_commit_is_rolled_back() {
        let c = conn();
        let tx = Tx::begin(&c, Lock::Deferred).unwrap();
        put(&c, 1);
        // a deferred foreign key that doesn't hold: COMMIT fails and leaves the transaction open
        c.execute("INSERT INTO c(pid) VALUES (99)", []).unwrap();
        assert!(tx.commit().is_err());
        assert!(c.is_autocommit());
        assert!(values(&c).is_empty());
    }

    #[test]
    fn joining_leaves_the_callers_transaction_to_the_caller() {
        let c = conn();
        c.execute_batch("BEGIN").unwrap();
        put(&c, 1);
        {
            let mut tx = Tx::join(&c, Lock::Immediate).unwrap();
            put(&c, 2);
            tx.checkpoint().unwrap();
            tx.commit().unwrap();
        }
        {
            let _dropped = Tx::join(&c, Lock::Deferred).unwrap();
            put(&c, 3);
        }
        assert!(!c.is_autocommit(), "still the caller's transaction");
        c.execute_batch("ROLLBACK").unwrap();
        assert!(values(&c).is_empty());
        // with nothing open, join begins its own
        let tx = Tx::join(&c, Lock::Deferred).unwrap();
        assert!(!c.is_autocommit());
        put(&c, 4);
        tx.commit().unwrap();
        assert_eq!(values(&c), [4]);
    }

    #[test]
    fn begin_inside_a_transaction_fails_and_touches_nothing() {
        let c = conn();
        c.execute_batch("BEGIN").unwrap();
        put(&c, 1);
        assert!(Tx::begin(&c, Lock::Deferred).is_err());
        assert!(!c.is_autocommit(), "the caller's transaction is still open");
        c.execute_batch("COMMIT").unwrap();
        assert_eq!(values(&c), [1]);
    }
}
