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
    flags: OpenFlags,
    journal_mode: Option<JournalMode>,
    vfs: Option<String>,
    num_conns: Option<usize>,
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
    /// Defaults to the number of logical CPUs of the current system.
    pub fn num_conns(mut self, num_conns: usize) -> Self {
        self.num_conns = Some(num_conns);
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
        let opens = (0..num_conns).map(|_| {
            ClientBuilder {
                path: self.path.clone(),
                flags: self.flags,
                journal_mode: self.journal_mode,
                vfs: self.vfs.clone(),
            }
            .open()
        });
        let clients = join_all(opens)
            .await
            .into_iter()
            .collect::<Result<Vec<Client>, Error>>()?;
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
        let clients = (0..num_conns)
            .map(|_| {
                ClientBuilder {
                    path: self.path.clone(),
                    flags: self.flags,
                    journal_mode: self.journal_mode,
                    vfs: self.vfs.clone(),
                }
                .open_blocking()
            })
            .collect::<Result<Vec<Client>, Error>>()?;
        Ok(Pool {
            state: Arc::new(State {
                clients,
                counter: AtomicU64::new(0),
            }),
        })
    }

    fn get_num_conns(&self) -> usize {
        self.num_conns.unwrap_or_else(|| {
            available_parallelism()
                .unwrap_or_else(|_| NonZeroUsize::new(1).unwrap())
                .into()
        })
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

    /// Closes the underlying sqlite connections.
    ///
    /// After this method returns, all calls to `self::conn()` or
    /// `self::conn_mut()` will return an [`Error::Closed`] error.
    pub async fn close(self) -> Result<(), Error> {
        let futures = self
            .state
            .clients
            .iter()
            .map(|client| client.clone().close());
        join_all(futures)
            .await
            .into_iter()
            .collect::<Result<(), Error>>()
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

    /// Closes the underlying sqlite connections, blocking the current thread.
    ///
    /// After this method returns, all calls to `self::conn_blocking()` or
    /// `self::conn_mut_blocking()` will return an [`Error::Closed`] error.
    pub fn close_blocking(&mut self) -> Result<(), Error> {
        self.state
            .clients
            .iter()
            .try_for_each(|client| client.clone().close_blocking())
    }

    fn get(&self) -> &Client {
        let n = self.state.counter.fetch_add(1, Relaxed);
        &self.state.clients[n as usize % self.state.clients.len()]
    }
}
