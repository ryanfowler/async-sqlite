use std::{
    path::{Path, PathBuf},
    thread,
};

use crate::Error;

use crossbeam::channel::{bounded, unbounded, Sender};
use futures_channel::oneshot;
use rusqlite::{Connection, OpenFlags};

/// A `ClientBuilder` can be used to create a [`Client`] with custom
/// configuration.
///
/// For more information on creating a sqlite connection, see the
/// [rusqlite docs](rusqlite::Connection::open()).
///
/// # Examples
///
/// ```rust
/// # use async_sqlite::ClientBuilder;
/// # async fn run() -> Result<(), async_sqlite::Error> {
/// let client = ClientBuilder::new().path("path/to/db.sqlite3").open().await?;
///
/// // ...
///
/// client.close().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug, Default)]
pub struct ClientBuilder {
    pub(crate) path: Option<PathBuf>,
    pub(crate) flags: OpenFlags,
    pub(crate) journal_mode: Option<JournalMode>,
}

impl ClientBuilder {
    /// Returns a new [`ClientBuilder`] with the default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Specify the path of the sqlite3 database to open.
    ///
    /// By default, an in-memory database is used.
    pub fn path<P: AsRef<Path>>(mut self, path: P) -> Self {
        self.path = Some(path.as_ref().into());
        self
    }

    /// Specify the [`OpenFlags`] to use when opening a new connection.
    ///
    /// By default, [`OpenFlags::default()`] is used.
    pub fn flags(mut self, flags: OpenFlags) -> Self {
        self.flags = flags;
        self
    }

    /// Specify the [`JournalMode`] to set when opening a new connection.
    ///
    /// By default, no `journal_mode` is explicity set.
    pub fn journal_mode(mut self, journal_mode: JournalMode) -> Self {
        self.journal_mode = Some(journal_mode);
        self
    }

    /// Returns a new [`Client`] that uses the `ClientBuilder` configuration.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use async_sqlite::ClientBuilder;
    /// # async fn run() -> Result<(), async_sqlite::Error> {
    /// let client = ClientBuilder::new().open().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn open(self) -> Result<Client, Error> {
        Client::open(self).await
    }
}

enum Command {
    Func(Box<dyn FnOnce(&mut Connection) + Send>),
    Shutdown(Box<dyn FnOnce(Result<(), Error>) + Send>),
}

/// Client represents a single sqlite connection that can be used from async
/// contexts.
#[derive(Clone)]
pub struct Client {
    conn_tx: Sender<Command>,
}

impl Client {
    async fn open(builder: ClientBuilder) -> Result<Self, Error> {
        let (open_tx, open_rx) = oneshot::channel();

        let mut builder = builder;
        thread::spawn(move || {
            let (conn_tx, conn_rx) = unbounded();

            let conn_res = if let Some(path) = builder.path {
                Connection::open_with_flags(path, builder.flags)
            } else {
                Connection::open_with_flags(":memory:", builder.flags)
            };
            let mut conn = match conn_res {
                Ok(conn) => conn,
                Err(err) => {
                    _ = open_tx.send(Err(err));
                    return;
                }
            };

            if let Some(journal_mode) = builder.journal_mode.take() {
                let val = journal_mode.as_str();
                if let Err(err) = conn.pragma_update(None, "journal_mode", val) {
                    _ = open_tx.send(Err(err));
                    return;
                }
            }

            // If the calling promise is dropped, the Client created here
            // should also be dropped by failing the send into the onshot
            // channel below. This thread will exit below when listening on the
            // conn_rx which should be disconnected.
            let self_ = Self { conn_tx };
            _ = open_tx.send(Ok(self_));

            while let Ok(cmd) = conn_rx.recv() {
                match cmd {
                    Command::Func(func) => func(&mut conn),
                    Command::Shutdown(func) => match conn.close() {
                        Ok(()) => {
                            func(Ok(()));
                            return;
                        }
                        Err((c, e)) => {
                            conn = c;
                            func(Err(e.into()));
                        }
                    },
                }
            }
        });

        Ok(open_rx.await??)
    }

    /// Invokes the provided function with a [`rusqlite::Connection`].
    pub async fn conn<F, T>(&self, func: F) -> Result<T, Error>
    where
        F: FnOnce(&Connection) -> Result<T, rusqlite::Error> + Send + 'static,
        T: Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        self.conn_tx.send(Command::Func(Box::new(move |conn| {
            _ = tx.send(func(conn));
        })))?;
        Ok(rx.await??)
    }

    /// Invokes the provided function with a mutable [`rusqlite::Connection`].
    pub async fn conn_mut<F, T>(&self, func: F) -> Result<T, Error>
    where
        F: FnOnce(&mut Connection) -> Result<T, rusqlite::Error> + Send + 'static,
        T: Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        self.conn_tx.send(Command::Func(Box::new(move |conn| {
            _ = tx.send(func(conn));
        })))?;
        Ok(rx.await??)
    }

    /// Closes the underlying sqlite connection.
    ///
    /// After this method returns, all calls to `self::conn()` or
    /// `self::conn_mut()` will return an [`Error::Closed`] error.
    pub async fn close(self) -> Result<(), Error> {
        let (tx, rx) = oneshot::channel();
        let func = Box::new(|res| _ = tx.send(res));
        if self.conn_tx.send(Command::Shutdown(func)).is_err() {
            // If the worker thread has already shut down, return Ok here.
            return Ok(());
        }
        // If receiving fails, the
        rx.await.unwrap_or(Ok(()))
    }

    fn _close_sync(&mut self) -> Result<(), Error> {
        let (tx, rx) = bounded(1);
        let func = Box::new(move |res| _ = tx.send(res));
        if self.conn_tx.send(Command::Shutdown(func)).is_err() {
            return Ok(());
        }
        rx.recv().unwrap_or(Ok(()))
    }
}

/// The possible sqlite journal modes.
///
/// For more information, please see the [sqlite docs](https://www.sqlite.org/pragma.html#pragma_journal_mode).
#[derive(Clone, Copy, Debug)]
pub enum JournalMode {
    Delete,
    Truncate,
    Persist,
    Memory,
    Wal,
    Off,
}

impl JournalMode {
    /// Returns the appropriate string representation of the journal mode.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Delete => "DELETE",
            Self::Truncate => "TRUNCATE",
            Self::Persist => "PERSIST",
            Self::Memory => "MEMORY",
            Self::Wal => "WAL",
            Self::Off => "OFF",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_async_macro::test_async;

    #[test_async]
    async fn test_journal_mode() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let client = ClientBuilder::new()
            .journal_mode(JournalMode::Wal)
            .path(tmp_dir.path().join("sqlite.db"))
            .open()
            .await
            .expect("client unable to be opened");
        let mode: String = client
            .conn(|conn| conn.query_row("PRAGMA journal_mode", (), |row| row.get(0)))
            .await
            .expect("client unable to fetch journal_mode");
        assert_eq!(mode, "wal");
    }

    #[test_async]
    async fn test_concurrency() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let client = ClientBuilder::new()
            .path(tmp_dir.path().join("sqlite.db"))
            .open()
            .await
            .expect("client unable to be opened");

        client
            .conn(|conn| {
                conn.execute(
                    "CREATE TABLE testing (id INTEGER PRIMARY KEY, val TEXT NOT NULL)",
                    (),
                )?;
                conn.execute("INSERT INTO testing VALUES (1, ?)", ["value1"])
            })
            .await
            .expect("writing schema and seed data");

        let fs = (0..10).map(|_| {
            client.conn(|conn| {
                let val: String =
                    conn.query_row("SELECT val FROM testing WHERE id=?", [1], |row| row.get(0))?;
                assert_eq!(val, "value1");
                Ok(())
            })
        });
        futures::future::join_all(fs)
            .await
            .into_iter()
            .collect::<Result<(), Error>>()
            .expect("collecting query results");
    }
}
