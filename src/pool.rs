use std::{
    ops::Deref,
    sync::{Arc, Mutex},
};

use crate::{Client, ClientBuilder, Error};

use async_channel::unbounded;
use futures_channel::oneshot::{self, Receiver, Sender};
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
    closing: Option<async_channel::Sender<Option<Client>>>,
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
        if clients.closing.is_some() {
            clients.count -= 1;
            _ = clients
                .closing
                .as_ref()
                .unwrap()
                .send_blocking(Some(self.client.clone()));
            return;
        }
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

    pub async fn close(&mut self) -> Result<(), Error> {
        let (tx, rx) = unbounded();

        let mut idle = {
            let mut clients = self.inner.clients.lock().unwrap();
            if clients.closing.is_some() {
                return Err(Error::Closed);
            }
            clients.closing = Some(tx);
            std::mem::take(&mut clients.waiters); // Drop any waiters.
            std::mem::take(&mut clients.idle)
        };

        while let Some(mut client) = idle.pop() {
            // What should we do when we encounter individual errors?
            _ = client.close().await;
        }

        loop {
            if let Ok(Some(mut client)) = rx.try_recv() {
                _ = client.close().await;
            }

            if self.inner.clients.lock().unwrap().count == 0 {
                return Ok(());
            }

            if let Some(mut client) = rx.recv().await.unwrap() {
                _ = client.close().await;
            }
        }
    }

    async fn get(&self) -> Result<ClientWrapper, Error> {
        let state = {
            let mut clients = self.inner.clients.lock().unwrap();
            if clients.closing.is_some() {
                return Err(Error::Closed);
            }
            if let Some(client) = clients.idle.pop() {
                State::Available(client)
            } else if clients.count < self.inner.max_conns {
                clients.count += 1;
                State::Open
            } else {
                let (tx, rx) = oneshot::channel();
                clients.waiters.push(tx);
                State::Wait(rx)
            }
        };
        let client = match state {
            State::Available(client) => client,
            State::Open => match self.inner.client_builder.open().await {
                Ok(client) => client,
                Err(err) => {
                    let mut clients = self.inner.clients.lock().unwrap();
                    clients.count -= 1;
                    if let Some(tx) = clients.closing.as_ref() {
                        _ = tx.send_blocking(None);
                    }
                    return Err(err);
                }
            },
            State::Wait(rx) => rx.await?,
        };
        Ok(ClientWrapper {
            client,
            pool: &self.inner,
        })
    }
}

enum State {
    Available(Client),
    Open,
    Wait(Receiver<Client>),
}
