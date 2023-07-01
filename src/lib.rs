pub use rusqlite;

mod client;
mod error;

pub use client::{Client, ClientBuilder};
pub use error::Error;
