use std::{
    path::{Path, PathBuf},
    thread,
};

use crossbeam::channel::{unbounded, SendError, Sender};
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
        if let Some(path) = self.path.as_ref() {
            Client::open(&path, self.flags).await
        } else {
            Client::open(":memory:", self.flags).await
        }
    }
}

#[derive(Clone)]
pub struct Client {
    sender: Sender<Command>,
}

impl Client {
    async fn open<P: AsRef<Path>>(path: P, flags: OpenFlags) -> Result<Self, Error> {
        let (open_tx, open_rx) = oneshot::channel();

        let path = path.as_ref().to_owned();
        thread::spawn(move || {
            let (conn_tx, conn_rx) = unbounded();

            let mut conn = match Connection::open_with_flags(path, flags) {
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
            let self_ = Self { sender: conn_tx };
            _ = open_tx.send(Ok(self_));

            while let Ok(cmd) = conn_rx.recv() {
                match cmd {
                    Command::Func(func) => func(&mut conn),
                    Command::Shutdown(tx) => match conn.close() {
                        Ok(()) => {
                            _ = tx.send(Ok(()));
                            return;
                        }
                        Err((c, e)) => {
                            conn = c;
                            _ = tx.send(Err(e.into()));
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
        self.sender.send(Command::Func(Box::new(move |conn| {
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
        self.sender.send(Command::Func(Box::new(move |conn| {
            _ = tx.send(func(conn));
        })))?;
        rx.await?
    }

    pub async fn close(&mut self) -> Result<(), Error> {
        let (tx, rx) = oneshot::channel();
        if self.sender.send(Command::Shutdown(tx)).is_err() {
            // If the receiving thread has already shut down, return Ok here.
            return Ok(());
        }
        rx.await?
    }
}

enum Command {
    Func(Box<dyn FnOnce(&mut Connection) + Send>),
    Shutdown(oneshot::Sender<Result<(), Error>>),
}

#[derive(Debug)]
pub enum Error {
    ConnectionClosed,
    Rusqlite(rusqlite::Error),
}

impl std::error::Error for Error {}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::ConnectionClosed => write!(f, "connection to sqlite database closed"),
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
        Error::ConnectionClosed
    }
}

impl From<Canceled> for Error {
    fn from(_value: Canceled) -> Self {
        Error::ConnectionClosed
    }
}
