use std::sync::{
    atomic::{AtomicU64, Ordering::Relaxed},
    Arc,
};

use crate::{Client, ClientBuilder, Error};

use futures::future::join_all;
use rusqlite::Connection;

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
    pub async fn new(builder: ClientBuilder, max_conns: usize) -> Result<Self, Error> {
        let opens = (0..max_conns).map(|_| builder.clone().open());
        let clients = join_all(opens)
            .await
            .into_iter()
            .collect::<Result<Vec<Client>, Error>>()?;
        Ok(Self {
            state: Arc::new(State {
                clients,
                counter: AtomicU64::new(0),
            }),
        })
    }

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

    fn get(&self) -> &Client {
        let n = self.state.counter.fetch_add(1, Relaxed);
        &self.state.clients[n as usize % self.state.clients.len()]
    }
}
