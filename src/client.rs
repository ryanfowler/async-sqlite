use std::{
    path::{Path, PathBuf},
    thread,
};

use crossbeam::channel::{bounded, unbounded, SendError, Sender};
use futures_channel::oneshot::{self, Canceled};
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

#[derive(Clone)]
pub struct Client {
    conn_tx: Sender<Command>,
}

impl Client {
    async fn open(builder: ClientBuilder) -> Result<Self, Error> {
        let (open_tx, open_rx) = oneshot::channel();

        let flags = builder.flags;
        let path = builder.path;
        thread::spawn(move || {
            let (conn_tx, conn_rx) = unbounded();

            let conn_res = if let Some(path) = path {
                Connection::open_with_flags(path, flags)
            } else {
                Connection::open_with_flags(":memory:", flags)
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

    pub async fn conn<F, T>(&self, func: F) -> Result<T, Error>
    where
        F: FnOnce(&Connection) -> Result<T, Error> + Send + 'static,
        T: Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        self.conn_tx.send(Command::Func(Box::new(move |conn| {
            _ = tx.send(func(conn));
        })))?;
        rx.await?
    }

    pub async fn conn_mut<F, T>(&self, func: F) -> Result<T, Error>
    where
        F: FnOnce(&mut Connection) -> Result<T, Error> + Send + 'static,
        T: Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        self.conn_tx.send(Command::Func(Box::new(move |conn| {
            _ = tx.send(func(conn));
        })))?;
        rx.await?
    }

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

enum Command {
    Func(Box<dyn FnOnce(&mut Connection) + Send>),
    Shutdown(Box<dyn FnOnce(Result<(), Error>) + Send>),
}

#[derive(Debug)]
pub enum Error {
    Closed,
    Rusqlite(rusqlite::Error),
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Rusqlite(err) => Some(err),
            _ => None,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Closed => write!(f, "connection to sqlite database closed"),
            Error::Rusqlite(err) => err.fmt(f),
        }
    }
}

impl From<rusqlite::Error> for Error {
    fn from(value: rusqlite::Error) -> Self {
        Error::Rusqlite(value)
    }
}

impl<T> From<SendError<T>> for Error {
    fn from(_value: SendError<T>) -> Self {
        Error::Closed
    }
}

impl From<Canceled> for Error {
    fn from(_value: Canceled) -> Self {
        Error::Closed
    }
}
