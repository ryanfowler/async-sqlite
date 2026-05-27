use std::{
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering::Relaxed},
        Arc,
    },
    thread::available_parallelism,
};

use crate::{Client, ClientBuilder, Error, JournalMode};

use futures_util::future::join_all;
use rusqlite::{Connection, OpenFlags};

/// A `PoolBuilder` can be used to create a [`Pool`] with custom
/// configuration.
///
/// See [`Client`] for more information.
///
/// # Examples
///
/// ```rust
/// # use async_sqlite::PoolBuilder;
/// # async fn run() -> Result<(), async_sqlite::Error> {
/// let pool = PoolBuilder::new().path("path/to/db.sqlite3").open().await?;
///
/// // ...
///
/// pool.close().await?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone, Debug, Default)]
pub struct PoolBuilder {
    path: Option<PathBuf>,
    shared_memory_name: Option<String>,
    flags: OpenFlags,
    journal_mode: Option<JournalMode>,
    vfs: Option<String>,
    num_conns: Option<usize>,
    queue_capacity: Option<usize>,
}

impl PoolBuilder {
    /// Returns a new [`PoolBuilder`] with the default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Specify the path of the sqlite3 database to open.
    ///
    /// By default, an in-memory database is used.
    pub fn path<P: AsRef<Path>>(mut self, path: P) -> Self {
        self.path = Some(path.as_ref().into());
        self.shared_memory_name = None;
        self
    }

    /// Use a named shared in-memory sqlite database.
    ///
    /// This opens connections with a URI of the form
    /// `file:<name>?mode=memory&cache=shared` and enables
    /// [`OpenFlags::SQLITE_OPEN_URI`] and
    /// [`OpenFlags::SQLITE_OPEN_SHARED_CACHE`].
    ///
    /// SQLite shared-cache mode has caveats and is discouraged by SQLite for
    /// many workloads. Prefer a file-backed database when possible. The
    /// in-memory database is deleted after the last connection using this name
    /// is closed.
    ///
    /// ```
    /// use async_sqlite::PoolBuilder;
    ///
    /// let builder = PoolBuilder::new().shared_memory("my-pool").num_conns(2);
    /// ```
    pub fn shared_memory<N: AsRef<str>>(mut self, name: N) -> Self {
        self.path = None;
        self.shared_memory_name = Some(name.as_ref().to_owned());
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

    /// Specify the number of sqlite connections to open as part of the pool.
    ///
    /// File-backed and shared-memory pools default to the number of logical
    /// CPUs of the current system. Anonymous in-memory pools, including
    /// `path(":memory:")`, default to `1` connection because each sqlite
    /// `:memory:` connection is a separate database. Values less than `1` are
    /// clamped to `1`.
    ///
    /// ```
    /// use async_sqlite::PoolBuilder;
    ///
    /// let builder = PoolBuilder::new().num_conns(2);
    /// ```
    pub fn num_conns(mut self, num_conns: usize) -> Self {
        self.num_conns = Some(num_conns.max(1));
        self
    }

    /// Limit the number of commands that may wait in each connection's worker
    /// queue.
    ///
    /// By default, each queue is unbounded. If a capacity is configured, calls
    /// return [`Error::QueueFull`] when a selected connection already has that
    /// many commands waiting for its worker thread. A capacity of `0` allows a
    /// command to be accepted only when the worker is ready to receive it
    /// immediately.
    pub fn queue_capacity(mut self, queue_capacity: usize) -> Self {
        self.queue_capacity = Some(queue_capacity);
        self
    }

    /// Returns a new [`Pool`] that uses the `PoolBuilder` configuration.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use async_sqlite::PoolBuilder;
    /// # async fn run() -> Result<(), async_sqlite::Error> {
    /// let pool = PoolBuilder::new().open().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn open(self) -> Result<Pool, Error> {
        let num_conns = self.get_num_conns();
        self.validate(num_conns)?;

        // Open the first connection with full config (including journal_mode).
        // This must complete before opening remaining connections to avoid
        // concurrent PRAGMA writes on a new database file.
        let first = self.client_builder().open().await?;

        // Open remaining connections with journal_mode too, so connection-local
        // modes are applied consistently across the pool.
        let opens = (1..num_conns).map(|_| self.client_builder().open());
        let mut clients = vec![first];
        clients.extend(
            join_all(opens)
                .await
                .into_iter()
                .collect::<Result<Vec<Client>, Error>>()?,
        );

        Ok(Pool {
            state: Arc::new(State {
                clients,
                counter: AtomicU64::new(0),
            }),
        })
    }

    /// Returns a new [`Pool`] that uses the `PoolBuilder` configuration,
    /// blocking the current thread.
    ///
    /// # Examples
    ///
    /// ```rust
    /// # use async_sqlite::PoolBuilder;
    /// # fn run() -> Result<(), async_sqlite::Error> {
    /// let pool = PoolBuilder::new().open_blocking()?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn open_blocking(self) -> Result<Pool, Error> {
        let num_conns = self.get_num_conns();
        self.validate(num_conns)?;

        // Open the first connection with full config (including journal_mode).
        let first = self.client_builder().open_blocking()?;

        // Open remaining connections with journal_mode too, so connection-local
        // modes are applied consistently across the pool.
        let mut clients = vec![first];
        clients.extend(
            (1..num_conns)
                .map(|_| self.client_builder().open_blocking())
                .collect::<Result<Vec<Client>, Error>>()?,
        );

        Ok(Pool {
            state: Arc::new(State {
                clients,
                counter: AtomicU64::new(0),
            }),
        })
    }

    fn get_num_conns(&self) -> usize {
        if let Some(num_conns) = self.num_conns {
            return num_conns;
        }

        if self.is_anonymous_memory() {
            return 1;
        }

        available_parallelism()
            .unwrap_or_else(|_| NonZeroUsize::new(1).unwrap())
            .into()
    }

    fn validate(&self, num_conns: usize) -> Result<(), Error> {
        if self
            .shared_memory_name
            .as_ref()
            .is_some_and(|name| name.is_empty())
        {
            return Err(Error::Config {
                message: "shared memory database name must not be empty",
            });
        }

        if self.is_anonymous_memory() && num_conns > 1 {
            return Err(Error::Config {
                message: "anonymous in-memory pools cannot use multiple connections; call path(...) for file-backed pools or shared_memory(...) for named shared in-memory pools",
            });
        }

        Ok(())
    }

    fn client_builder(&self) -> ClientBuilder {
        ClientBuilder {
            path: self.connection_path(),
            flags: self.connection_flags(),
            journal_mode: self.journal_mode,
            vfs: self.vfs.clone(),
            queue_capacity: self.queue_capacity,
        }
    }

    fn connection_path(&self) -> Option<PathBuf> {
        self.shared_memory_name
            .as_deref()
            .map(shared_memory_uri)
            .or_else(|| self.path.clone())
    }

    fn connection_flags(&self) -> OpenFlags {
        let mut flags = self.flags;
        if self.shared_memory_name.is_some() {
            flags.insert(OpenFlags::SQLITE_OPEN_URI);
            flags.insert(OpenFlags::SQLITE_OPEN_SHARED_CACHE);
            flags.remove(OpenFlags::SQLITE_OPEN_PRIVATE_CACHE);
        }
        flags
    }

    fn is_anonymous_memory(&self) -> bool {
        self.shared_memory_name.is_none()
            && self
                .path
                .as_deref()
                .is_none_or(|path| path == Path::new(":memory:"))
    }
}

fn shared_memory_uri(name: &str) -> PathBuf {
    let mut uri = String::from("file:");
    push_uri_encoded(name, &mut uri);
    uri.push_str("?mode=memory&cache=shared");
    uri.into()
}

fn push_uri_encoded(input: &str, out: &mut String) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte.into());
            }
            _ => {
                out.push('%');
                out.push(HEX[(byte >> 4) as usize].into());
                out.push(HEX[(byte & 0x0F) as usize].into());
            }
        }
    }
}

/// A simple Pool of sqlite connections.
///
/// A Pool has the same API as an individual [`Client`].
#[derive(Clone)]
pub struct Pool {
    state: Arc<State>,
}

struct State {
    clients: Vec<Client>,
    counter: AtomicU64,
}

impl Pool {
    /// Invokes the provided function with a [`rusqlite::Connection`].
    pub async fn conn<F, T>(&self, func: F) -> Result<T, Error>
    where
        F: FnOnce(&Connection) -> Result<T, rusqlite::Error> + Send + 'static,
        T: Send + 'static,
    {
        self.get().conn(func).await
    }

    /// Invokes the provided function with a mutable [`rusqlite::Connection`].
    pub async fn conn_mut<F, T>(&self, func: F) -> Result<T, Error>
    where
        F: FnOnce(&mut Connection) -> Result<T, rusqlite::Error> + Send + 'static,
        T: Send + 'static,
    {
        self.get().conn_mut(func).await
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
        self.get().conn_and_then(func).await
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
        self.get().conn_mut_and_then(func).await
    }

    /// Closes the underlying sqlite connections.
    ///
    /// After this method returns, all calls to `self::conn()` or
    /// `self::conn_mut()` will return an [`Error::Closed`] error.
    pub async fn close(&self) -> Result<(), Error> {
        let closes = self.state.clients.iter().map(|client| client.close());
        let res = join_all(closes).await;
        res.into_iter().collect::<Result<Vec<_>, Error>>()?;
        Ok(())
    }

    /// Invokes the provided function with a [`rusqlite::Connection`], blocking
    /// the current thread.
    pub fn conn_blocking<F, T>(&self, func: F) -> Result<T, Error>
    where
        F: FnOnce(&Connection) -> Result<T, rusqlite::Error> + Send + 'static,
        T: Send + 'static,
    {
        self.get().conn_blocking(func)
    }

    /// Invokes the provided function with a mutable [`rusqlite::Connection`],
    /// blocking the current thread.
    pub fn conn_mut_blocking<F, T>(&self, func: F) -> Result<T, Error>
    where
        F: FnOnce(&mut Connection) -> Result<T, rusqlite::Error> + Send + 'static,
        T: Send + 'static,
    {
        self.get().conn_mut_blocking(func)
    }

    /// Invokes the provided function with a [`rusqlite::Connection`], blocking
    /// the current thread.
    ///
    /// Maps the result error type to a custom error; designed to be
    /// used in conjunction with [`query_and_then`](https://docs.rs/rusqlite/latest/rusqlite/struct.CachedStatement.html#method.query_and_then).
    pub fn conn_and_then_blocking<F, T, E>(&self, func: F) -> Result<T, E>
    where
        F: FnOnce(&Connection) -> Result<T, E> + Send + 'static,
        T: Send + 'static,
        E: From<rusqlite::Error> + From<Error> + Send + 'static,
    {
        self.get().conn_and_then_blocking(func)
    }

    /// Invokes the provided function with a mutable [`rusqlite::Connection`],
    /// blocking the current thread.
    ///
    /// Maps the result error type to a custom error; designed to be
    /// used in conjunction with [`query_and_then`](https://docs.rs/rusqlite/latest/rusqlite/struct.CachedStatement.html#method.query_and_then).
    pub fn conn_mut_and_then_blocking<F, T, E>(&self, func: F) -> Result<T, E>
    where
        F: FnOnce(&mut Connection) -> Result<T, E> + Send + 'static,
        T: Send + 'static,
        E: From<rusqlite::Error> + From<Error> + Send + 'static,
    {
        self.get().conn_mut_and_then_blocking(func)
    }

    /// Closes the underlying sqlite connections, blocking the current thread.
    ///
    /// After this method returns, all calls to `self::conn_blocking()` or
    /// `self::conn_mut_blocking()` will return an [`Error::Closed`] error.
    pub fn close_blocking(&self) -> Result<(), Error> {
        let mut first_err = None;
        for client in self.state.clients.iter() {
            if let Err(e) = client.close_blocking() {
                if first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn get(&self) -> &Client {
        let n = self.state.counter.fetch_add(1, Relaxed);
        &self.state.clients[n as usize % self.state.clients.len()]
    }

    /// Runs a function on all connections in the pool asynchronously.
    ///
    /// The function is executed on each connection concurrently.
    pub async fn conn_for_each<F, T>(&self, func: F) -> Vec<Result<T, Error>>
    where
        F: Fn(&Connection) -> Result<T, rusqlite::Error> + Send + Sync + 'static,
        T: Send + 'static,
    {
        let func = Arc::new(func);
        let futures = self.state.clients.iter().map(|client| {
            let func = func.clone();
            async move { client.conn(move |conn| func(conn)).await }
        });
        join_all(futures).await
    }

    /// Runs a function on all connections in the pool, blocking the current thread.
    pub fn conn_for_each_blocking<F, T>(&self, func: F) -> Vec<Result<T, Error>>
    where
        F: Fn(&Connection) -> Result<T, rusqlite::Error> + Send + Sync + 'static,
        T: Send + 'static,
    {
        let func = Arc::new(func);
        self.state
            .clients
            .iter()
            .map(|client| {
                let func = func.clone();
                client.conn_blocking(move |conn| func(conn))
            })
            .collect()
    }
}
