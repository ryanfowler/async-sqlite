use std::{
    path::{Path, PathBuf},
    thread,
};

use crate::Error;

use crossbeam::channel::{bounded, unbounded, Sender};
use futures_channel::oneshot;
use rusqlite::{Connection, OpenFlags};

#[derive(Clone, Debug, Default)]
pub struct ClientBuilder {
    path: Option<PathBuf>,
    flags: OpenFlags,
}

impl ClientBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn path<P: AsRef<Path>>(&mut self, path: P) -> &mut Self {
        self.path = Some(path.as_ref().into());
        self
    }

    pub fn flags(&mut self, flags: OpenFlags) -> &mut Self {
        self.flags = flags;
        self
    }

    pub async fn open(&self) -> Result<Client, Error> {
        Client::open(self.clone()).await
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
    pub async fn close(&mut self) -> Result<(), Error> {
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
