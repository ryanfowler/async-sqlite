use async_sqlite::{ClientBuilder, Error, JournalMode, PoolBuilder};

#[test]
fn test_blocking_client() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let mut client = ClientBuilder::new()
        .journal_mode(JournalMode::Wal)
        .path(tmp_dir.path().join("sqlite.db"))
        .open_blocking()
        .expect("client unable to be opened");

    client
        .conn_blocking(|conn| {
            conn.execute(
                "CREATE TABLE testing (id INTEGER PRIMARY KEY, val TEXT NOT NULL)",
                (),
            )?;
            conn.execute("INSERT INTO testing VALUES (1, ?)", ["value1"])
        })
        .expect("writing schema and seed data");

    client
        .conn_blocking(|conn| {
            let val: String =
                conn.query_row("SELECT val FROM testing WHERE id=?", [1], |row| row.get(0))?;
            assert_eq!(val, "value1");
            Ok(())
        })
        .expect("querying for result");

    client.close_blocking().expect("closing client conn");
}

#[test]
fn test_blocking_pool() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let mut pool = PoolBuilder::new()
        .journal_mode(JournalMode::Wal)
        .path(tmp_dir.path().join("sqlite.db"))
        .open_blocking()
        .expect("client unable to be opened");

    pool.conn_blocking(|conn| {
        conn.execute(
            "CREATE TABLE testing (id INTEGER PRIMARY KEY, val TEXT NOT NULL)",
            (),
        )?;
        conn.execute("INSERT INTO testing VALUES (1, ?)", ["value1"])
    })
    .expect("writing schema and seed data");

    pool.conn_blocking(|conn| {
        let val: String =
            conn.query_row("SELECT val FROM testing WHERE id=?", [1], |row| row.get(0))?;
        assert_eq!(val, "value1");
        Ok(())
    })
    .expect("querying for result");

    pool.close_blocking().expect("closing client conn");
}

macro_rules! async_test {
    ($name:ident) => {
        paste::item! {
            #[::core::prelude::v1::test]
            fn [< $name _async_std >] () {
                ::async_std::task::block_on($name());
            }

            #[::core::prelude::v1::test]
            fn [< $name _tokio >] () {
                ::tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on($name());
            }
        }
    };
}

async_test!(test_journal_mode);
async_test!(test_concurrency);
async_test!(test_pool);

async fn test_journal_mode() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let client = ClientBuilder::new()
        .journal_mode(JournalMode::Wal)
        .path(tmp_dir.path().join("sqlite.db"))
        .open()
        .await
        .expect("client unable to be opened");
    let mode: String = client
        .conn(|conn| conn.query_row("PRAGMA journal_mode", (), |row| row.get(0)))
        .await
        .expect("client unable to fetch journal_mode");
    assert_eq!(mode, "wal");
}

async fn test_concurrency() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let client = ClientBuilder::new()
        .path(tmp_dir.path().join("sqlite.db"))
        .open()
        .await
        .expect("client unable to be opened");

    client
        .conn(|conn| {
            conn.execute(
                "CREATE TABLE testing (id INTEGER PRIMARY KEY, val TEXT NOT NULL)",
                (),
            )?;
            conn.execute("INSERT INTO testing VALUES (1, ?)", ["value1"])
        })
        .await
        .expect("writing schema and seed data");

    let fs = (0..10).map(|_| {
        client.conn(|conn| {
            let val: String =
                conn.query_row("SELECT val FROM testing WHERE id=?", [1], |row| row.get(0))?;
            assert_eq!(val, "value1");
            Ok(())
        })
    });
    futures_util::future::join_all(fs)
        .await
        .into_iter()
        .collect::<Result<(), Error>>()
        .expect("collecting query results");
}

async fn test_pool() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let pool = PoolBuilder::new()
        .path(tmp_dir.path().join("sqlite.db"))
        .num_conns(2)
        .open()
        .await
        .expect("client unable to be opened");

    pool.conn(|conn| {
        conn.execute(
            "CREATE TABLE testing (id INTEGER PRIMARY KEY, val TEXT NOT NULL)",
            (),
        )?;
        conn.execute("INSERT INTO testing VALUES (1, ?)", ["value1"])
    })
    .await
    .expect("writing schema and seed data");

    let fs = (0..10).map(|_| {
        pool.conn(|conn| {
            let val: String =
                conn.query_row("SELECT val FROM testing WHERE id=?", [1], |row| row.get(0))?;
            assert_eq!(val, "value1");
            Ok(())
        })
    });
    futures_util::future::join_all(fs)
        .await
        .into_iter()
        .collect::<Result<(), Error>>()
        .expect("collecting query results");
}
