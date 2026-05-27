use std::{
    path::{Path, PathBuf},
    thread,
};

use crate::Error;

use crossbeam_channel::{bounded, unbounded, Receiver, Sender, TrySendError};
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
    pub(crate) vfs: Option<String>,
    pub(crate) queue_capacity: Option<usize>,
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

    /// Specify the name of the [vfs](https://www.sqlite.org/vfs.html) to use.
    pub fn vfs(mut self, vfs: &str) -> Self {
        self.vfs = Some(vfs.to_owned());
        self
    }

    /// Limit the number of commands that may wait in the worker queue.
    ///
    /// By default, the queue is unbounded. If a capacity is configured, calls
    /// return [`Error::QueueFull`] when that many commands are already waiting
    /// for the worker thread. A capacity of `0` allows a command to be accepted
    /// only when the worker is ready to receive it immediately.
    pub fn queue_capacity(mut self, queue_capacity: usize) -> Self {
        self.queue_capacity = Some(queue_capacity);
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
        Client::open_async(self).await
    }

    /// Returns a new [`Client`] that uses the `ClientBuilder` configuration,
    /// blocking the current thread.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use async_sqlite::ClientBuilder;
    /// # fn run() -> Result<(), async_sqlite::Error> {
    /// let client = ClientBuilder::new().open_blocking()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn open_blocking(self) -> Result<Client, Error> {
        Client::open_blocking(self)
    }
}

enum Command {
    Func(Box<dyn QueuedFunc>),
    Shutdown(Box<dyn QueuedShutdown>),
}

trait QueuedFunc: Send {
    fn is_canceled(&self) -> bool;
    fn execute(self: Box<Self>, conn: &mut Connection);
}

struct AsyncFunc<F, T, E> {
    tx: oneshot::Sender<Result<T, E>>,
    func: F,
}

impl<F, T, E> QueuedFunc for AsyncFunc<F, T, E>
where
    F: FnOnce(&mut Connection) -> Result<T, E> + Send + 'static,
    T: Send + 'static,
    E: Send + 'static,
{
    fn is_canceled(&self) -> bool {
        self.tx.is_canceled()
    }

    fn execute(self: Box<Self>, conn: &mut Connection) {
        let Self { tx, func } = *self;
        _ = tx.send(func(conn));
    }
}

struct BlockingFunc<F, T, E> {
    tx: Sender<Result<T, E>>,
    func: F,
}

impl<F, T, E> QueuedFunc for BlockingFunc<F, T, E>
where
    F: FnOnce(&mut Connection) -> Result<T, E> + Send + 'static,
    T: Send + 'static,
    E: Send + 'static,
{
    fn is_canceled(&self) -> bool {
        false
    }

    fn execute(self: Box<Self>, conn: &mut Connection) {
        let Self { tx, func } = *self;
        _ = tx.send(func(conn));
    }
}

trait QueuedShutdown: Send {
    fn is_canceled(&self) -> bool;
    fn respond(self: Box<Self>, res: Result<(), Error>);
}

struct AsyncShutdown {
    tx: oneshot::Sender<Result<(), Error>>,
}

impl QueuedShutdown for AsyncShutdown {
    fn is_canceled(&self) -> bool {
        self.tx.is_canceled()
    }

    fn respond(self: Box<Self>, res: Result<(), Error>) {
        _ = self.tx.send(res);
    }
}

struct BlockingShutdown {
    tx: Sender<Result<(), Error>>,
}

impl QueuedShutdown for BlockingShutdown {
    fn is_canceled(&self) -> bool {
        false
    }

    fn respond(self: Box<Self>, res: Result<(), Error>) {
        _ = self.tx.send(res);
    }
}

fn run_catching<F, T>(conn: &mut Connection, func: F) -> Result<T, Error>
where
    F: FnOnce(&mut Connection) -> Result<T, rusqlite::Error>,
{
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| func(conn))) {
        Ok(res) => res.map_err(Error::from),
        Err(p) => {
            rollback_if_needed(conn);
            Err(Error::Panic {
                message: panic_message(&*p),
            })
        }
    }
}

fn run_catching_and_then<F, T, E>(conn: &mut Connection, func: F) -> Result<T, E>
where
    F: FnOnce(&mut Connection) -> Result<T, E>,
    E: From<Error>,
{
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| func(conn))) {
        Ok(res) => res,
        Err(p) => {
            rollback_if_needed(conn);
            Err(E::from(Error::Panic {
                message: panic_message(&*p),
            }))
        }
    }
}

fn rollback_if_needed(conn: &mut Connection) {
    if !conn.is_autocommit() {
        let _ = conn.execute_batch("ROLLBACK");
    }
}

fn panic_message(p: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = p.downcast_ref::<&'static str>() {
        (*s).to_owned()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic".to_owned()
    }
}

/// Client represents a single sqlite connection that can be used from async
/// contexts.
#[derive(Clone)]
pub struct Client {
    conn_tx: Sender<Command>,
}

impl Client {
    async fn open_async(builder: ClientBuilder) -> Result<Self, Error> {
        let (open_tx, open_rx) = oneshot::channel();
        Self::open(builder, |res| {
            _ = open_tx.send(res);
        });
        open_rx.await?
    }

    fn open_blocking(builder: ClientBuilder) -> Result<Self, Error> {
        let (conn_tx, conn_rx) = bounded(1);
        Self::open(builder, move |res| {
            _ = conn_tx.send(res);
        });
        conn_rx.recv()?
    }

    fn open<F>(builder: ClientBuilder, func: F)
    where
        F: FnOnce(Result<Self, Error>) + Send + 'static,
    {
        thread::spawn(move || {
            let (conn_tx, conn_rx) = match builder.queue_capacity {
                Some(queue_capacity) => bounded(queue_capacity),
                None => unbounded(),
            };

            let mut conn = match Client::create_conn(builder) {
                Ok(conn) => conn,
                Err(err) => {
                    func(Err(err));
                    return;
                }
            };

            let client = Self { conn_tx };
            func(Ok(client));

            while let Ok(cmd) = conn_rx.recv() {
                match cmd {
                    Command::Func(func) => {
                        if !func.is_canceled() {
                            func.execute(&mut conn);
                        }
                    }
                    Command::Shutdown(func) => {
                        if !func.is_canceled() {
                            match conn.close() {
                                Ok(()) => {
                                    func.respond(Ok(()));
                                    return;
                                }
                                Err((c, e)) => {
                                    conn = c;
                                    func.respond(Err(e.into()));
                                }
                            }
                        }
                    }
                }
            }
        });
    }

    fn create_conn(mut builder: ClientBuilder) -> Result<Connection, Error> {
        let path = builder.path.take().unwrap_or_else(|| ":memory:".into());
        let conn = if let Some(vfs) = builder.vfs.take() {
            Connection::open_with_flags_and_vfs(path, builder.flags, vfs.as_str())?
        } else {
            Connection::open_with_flags(path, builder.flags)?
        };

        if let Some(journal_mode) = builder.journal_mode.take() {
            let val = journal_mode.as_str();
            let out: String =
                conn.pragma_update_and_check(None, "journal_mode", val, |row| row.get(0))?;
            if !out.eq_ignore_ascii_case(val) {
                return Err(Error::PragmaUpdate {
                    name: "journal_mode",
                    exp: val,
                    got: out,
                });
            }
        }

        Ok(conn)
    }

    fn enqueue_async<F, T, E>(
        &self,
        func: F,
    ) -> Result<oneshot::Receiver<Result<T, E>>, TrySendError<Command>>
    where
        F: FnOnce(&mut Connection) -> Result<T, E> + Send + 'static,
        T: Send + 'static,
        E: Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        self.conn_tx
            .try_send(Command::Func(Box::new(AsyncFunc { tx, func })))?;
        Ok(rx)
    }

    fn enqueue_blocking<F, T, E>(
        &self,
        func: F,
    ) -> Result<Receiver<Result<T, E>>, TrySendError<Command>>
    where
        F: FnOnce(&mut Connection) -> Result<T, E> + Send + 'static,
        T: Send + 'static,
        E: Send + 'static,
    {
        let (tx, rx) = bounded(1);
        self.conn_tx
            .try_send(Command::Func(Box::new(BlockingFunc { tx, func })))?;
        Ok(rx)
    }

    /// Invokes the provided function with a [`rusqlite::Connection`].
    pub async fn conn<F, T>(&self, func: F) -> Result<T, Error>
    where
        F: FnOnce(&Connection) -> Result<T, rusqlite::Error> + Send + 'static,
        T: Send + 'static,
    {
        let rx = self
            .enqueue_async(move |conn| run_catching(conn, |conn| func(conn)))
            .map_err(Error::from)?;
        rx.await?
    }

    /// Invokes the provided function with a mutable [`rusqlite::Connection`].
    pub async fn conn_mut<F, T>(&self, func: F) -> Result<T, Error>
    where
        F: FnOnce(&mut Connection) -> Result<T, rusqlite::Error> + Send + 'static,
        T: Send + 'static,
    {
        let rx = self
            .enqueue_async(move |conn| run_catching(conn, func))
            .map_err(Error::from)?;
        rx.await?
    }

    /// Invokes the provided function with a [`rusqlite::Connection`].
    ///
    /// Maps the result error type to a custom error; designed to be
    /// used in conjunction with [`query_and_then`](https://docs.rs/rusqlite/latest/rusqlite/struct.CachedStatement.html#method.query_and_then).
    pub async fn conn_and_then<F, T, E>(&self, func: F) -> Result<T, E>
    where
        F: FnOnce(&Connection) -> Result<T, E> + Send + 'static,
        T: Send + 'static,
        E: From<rusqlite::Error> + From<Error> + Send + 'static,
    {
        let rx = self
            .enqueue_async(move |conn| run_catching_and_then(conn, |conn| func(conn)))
            .map_err(Error::from)?;
        rx.await.map_err(Error::from)?
    }

    /// Invokes the provided function with a mutable [`rusqlite::Connection`].
    ///
    /// Maps the result error type to a custom error; designed to be
    /// used in conjunction with [`query_and_then`](https://docs.rs/rusqlite/latest/rusqlite/struct.CachedStatement.html#method.query_and_then).
    pub async fn conn_mut_and_then<F, T, E>(&self, func: F) -> Result<T, E>
    where
        F: FnOnce(&mut Connection) -> Result<T, E> + Send + 'static,
        T: Send + 'static,
        E: From<rusqlite::Error> + From<Error> + Send + 'static,
    {
        let rx = self
            .enqueue_async(move |conn| run_catching_and_then(conn, func))
            .map_err(Error::from)?;
        rx.await.map_err(Error::from)?
    }

    /// Closes the underlying sqlite connection.
    ///
    /// After this method returns, all calls to `self::conn()` or
    /// `self::conn_mut()` will return an [`Error::Closed`] error.
    pub async fn close(&self) -> Result<(), Error> {
        let (tx, rx) = oneshot::channel();
        match self
            .conn_tx
            .try_send(Command::Shutdown(Box::new(AsyncShutdown { tx })))
        {
            Ok(()) => {}
            Err(TrySendError::Disconnected(_)) => {
                // If the worker thread has already shut down, return Ok here.
                return Ok(());
            }
            Err(err) => return Err(err.into()),
        }
        // If receiving fails, the connection is already closed.
        rx.await.unwrap_or(Ok(()))
    }

    /// Invokes the provided function with a [`rusqlite::Connection`], blocking
    /// the current thread until completion.
    pub fn conn_blocking<F, T>(&self, func: F) -> Result<T, Error>
    where
        F: FnOnce(&Connection) -> Result<T, rusqlite::Error> + Send + 'static,
        T: Send + 'static,
    {
        let rx = self
            .enqueue_blocking(move |conn| run_catching(conn, |conn| func(conn)))
            .map_err(Error::from)?;
        rx.recv()?
    }

    /// Invokes the provided function with a mutable [`rusqlite::Connection`],
    /// blocking the current thread until completion.
    pub fn conn_mut_blocking<F, T>(&self, func: F) -> Result<T, Error>
    where
        F: FnOnce(&mut Connection) -> Result<T, rusqlite::Error> + Send + 'static,
        T: Send + 'static,
    {
        let rx = self
            .enqueue_blocking(move |conn| run_catching(conn, func))
            .map_err(Error::from)?;
        rx.recv()?
    }

    /// Invokes the provided function with a [`rusqlite::Connection`],
    /// blocking the current thread until completion.
    ///
    /// Maps the result error type to a custom error; designed to be
    /// used in conjunction with [`query_and_then`](https://docs.rs/rusqlite/latest/rusqlite/struct.CachedStatement.html#method.query_and_then).
    pub fn conn_and_then_blocking<F, T, E>(&self, func: F) -> Result<T, E>
    where
        F: FnOnce(&Connection) -> Result<T, E> + Send + 'static,
        T: Send + 'static,
        E: From<rusqlite::Error> + From<Error> + Send + 'static,
    {
        let rx = self
            .enqueue_blocking(move |conn| run_catching_and_then(conn, |conn| func(conn)))
            .map_err(Error::from)?;
        rx.recv().map_err(Error::from)?
    }

    /// Invokes the provided function with a mutable [`rusqlite::Connection`],
    /// blocking the current thread until completion.
    ///
    /// Maps the result error type to a custom error; designed to be
    /// used in conjunction with [`query_and_then`](https://docs.rs/rusqlite/latest/rusqlite/struct.CachedStatement.html#method.query_and_then).
    pub fn conn_mut_and_then_blocking<F, T, E>(&self, func: F) -> Result<T, E>
    where
        F: FnOnce(&mut Connection) -> Result<T, E> + Send + 'static,
        T: Send + 'static,
        E: From<rusqlite::Error> + From<Error> + Send + 'static,
    {
        let rx = self
            .enqueue_blocking(move |conn| run_catching_and_then(conn, func))
            .map_err(Error::from)?;
        rx.recv().map_err(Error::from)?
    }

    /// Closes the underlying sqlite connection, blocking the current thread
    /// until complete.
    ///
    /// After this method returns, all calls to `self::conn_blocking()` or
    /// `self::conn_mut_blocking()` will return an [`Error::Closed`] error.
    pub fn close_blocking(&self) -> Result<(), Error> {
        let (tx, rx) = bounded(1);
        match self
            .conn_tx
            .try_send(Command::Shutdown(Box::new(BlockingShutdown { tx })))
        {
            Ok(()) => {}
            Err(TrySendError::Disconnected(_)) => return Ok(()),
            Err(err) => return Err(err.into()),
        }
        // If receiving fails, the connection is already closed.
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
