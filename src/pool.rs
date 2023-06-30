use std::{
    ops::Deref,
    sync::{Arc, Mutex},
};

use crate::{Client, ClientBuilder, Error};

use futures_channel::oneshot::{self, Sender};
use rusqlite::Connection;

#[derive(Clone)]
pub struct Pool {
    inner: Arc<InnerPool>,
}

struct InnerPool {
    client_builder: ClientBuilder,
    clients: Mutex<Clients>,
    max_conns: usize,
}

#[derive(Default)]
struct Clients {
    count: usize,
    idle: Vec<Client>,
    waiters: Vec<Sender<Client>>,
}

struct ClientWrapper<'a> {
    client: Client,
    pool: &'a InnerPool,
}

impl Deref for ClientWrapper<'_> {
    type Target = Client;

    fn deref(&self) -> &Self::Target {
        &self.client
    }
}

impl Drop for ClientWrapper<'_> {
    fn drop(&mut self) {
        let mut clients = self.pool.clients.lock().unwrap();
        while let Some(tx) = clients.waiters.pop() {
            if tx.send(self.client.clone()).is_ok() {
                return;
            }
        }
        clients.idle.push(self.client.clone());
    }
}

impl Pool {
    pub fn new(builder: ClientBuilder, max_conns: usize) -> Self {
        Self {
            inner: Arc::new(InnerPool {
                client_builder: builder,
                clients: Mutex::default(),
                max_conns,
            }),
        }
    }

    pub async fn conn<F, T>(&self, func: F) -> Result<T, Error>
    where
        F: FnOnce(&Connection) -> Result<T, Error> + Send + 'static,
        T: Send + 'static,
    {
        let client = self.get().await?;
        client.conn(func).await
    }

    async fn get(&self) -> Result<ClientWrapper, Error> {
        let client = {
            let mut clients = self.inner.clients.lock().unwrap();
            if let Some(client) = clients.idle.pop() {
                client
            } else if clients.count < self.inner.max_conns {
                clients.count += 1;
                drop(clients);
                self.inner.client_builder.open().await?
            } else {
                let (tx, rx) = oneshot::channel();
                clients.waiters.push(tx);
                drop(clients);
                rx.await?
            }
        };
        Ok(ClientWrapper {
            client,
            pool: &self.inner,
        })
    }
}
