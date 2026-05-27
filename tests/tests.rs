use std::sync::atomic::{AtomicUsize, Ordering};

use async_sqlite::{ClientBuilder, Error, JournalMode, PoolBuilder};
use futures_util::FutureExt;

static SHARED_MEMORY_ID: AtomicUsize = AtomicUsize::new(0);

fn shared_memory_name(prefix: &str) -> String {
    let id = SHARED_MEMORY_ID.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{}-{id}", std::process::id())
}

fn assert_config_message(err: Error, expected: &str) {
    match err {
        Error::Config { message } => assert_eq!(message, expected),
        other => panic!("expected Error::Config, got {other:?}"),
    }
}

fn journal_modes() -> [(JournalMode, &'static str); 6] {
    [
        (JournalMode::Delete, "delete"),
        (JournalMode::Truncate, "truncate"),
        (JournalMode::Persist, "persist"),
        (JournalMode::Memory, "memory"),
        (JournalMode::Wal, "wal"),
        (JournalMode::Off, "off"),
    ]
}

#[test]
fn test_blocking_client() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let client = ClientBuilder::new()
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
fn test_blocking_default_pool_in_memory_uses_one_connection() {
    let pool = PoolBuilder::new()
        .open_blocking()
        .expect("pool unable to be opened");

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

    let results = pool.conn_for_each_blocking(|_| Ok(()));
    assert_eq!(results.len(), 1);

    pool.close_blocking().expect("closing pool");

    let pool = PoolBuilder::new()
        .path(":memory:")
        .open_blocking()
        .expect("pool unable to be opened");
    let results = pool.conn_for_each_blocking(|_| Ok(()));
    assert_eq!(results.len(), 1);
    pool.close_blocking().expect("closing pool");
}

#[test]
fn test_blocking_pool() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let pool = PoolBuilder::new()
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

#[test]
fn test_blocking_pool_rejects_multi_connection_anonymous_memory() {
    let err = match PoolBuilder::new().num_conns(2).open_blocking() {
        Ok(pool) => {
            pool.close_blocking().expect("closing unexpected pool");
            panic!("expected pool open to fail");
        }
        Err(err) => err,
    };

    assert_config_message(
        err,
        "anonymous in-memory pools cannot use multiple connections; call path(...) for file-backed pools or shared_memory(...) for named shared in-memory pools",
    );

    let err = match PoolBuilder::new()
        .path(":memory:")
        .num_conns(2)
        .open_blocking()
    {
        Ok(pool) => {
            pool.close_blocking().expect("closing unexpected pool");
            panic!("expected pool open to fail");
        }
        Err(err) => err,
    };

    assert_config_message(
        err,
        "anonymous in-memory pools cannot use multiple connections; call path(...) for file-backed pools or shared_memory(...) for named shared in-memory pools",
    );
}

#[test]
fn test_blocking_pool_journal_mode() {
    for (journal_mode, expected) in journal_modes() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let pool = PoolBuilder::new()
            .journal_mode(journal_mode)
            .path(tmp_dir.path().join("sqlite.db"))
            .num_conns(4)
            .open_blocking()
            .expect("pool unable to be opened");

        let results = pool.conn_for_each_blocking(|conn| {
            conn.query_row("PRAGMA journal_mode", (), |row| row.get(0))
        });
        for (idx, result) in results.into_iter().enumerate() {
            let mode: String = result.unwrap();
            assert_eq!(
                mode, expected,
                "{journal_mode:?} journal mode mismatch on connection {idx}"
            );
        }

        pool.close_blocking().expect("closing pool");
    }
}

macro_rules! async_test {
    ($name:ident) => {
        paste::item! {
            #[::core::prelude::v1::test]
            fn [< $name _smol >] () {
                ::smol::block_on($name());
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
async_test!(test_default_pool_in_memory_uses_one_connection);
async_test!(test_pool);
async_test!(test_pool_rejects_multi_connection_anonymous_memory);
async_test!(test_shared_memory_pool);
async_test!(test_shared_memory_rejects_empty_name);
async_test!(test_pool_journal_mode);
async_test!(test_pool_conn_for_each);
async_test!(test_pool_close_concurrent);
async_test!(test_canceled_async_command_is_skipped);
async_test!(test_client_queue_capacity_reports_full);
async_test!(test_pool_num_conns_zero_clamps);
async_test!(test_closure_panic_surfaces_error);
async_test!(test_panic_after_begin_immediate_rolls_back);

async fn test_journal_mode() {
    for (journal_mode, expected) in journal_modes() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let client = ClientBuilder::new()
            .journal_mode(journal_mode)
            .path(tmp_dir.path().join("sqlite.db"))
            .open()
            .await
            .expect("client unable to be opened");
        let mode: String = client
            .conn(|conn| conn.query_row("PRAGMA journal_mode", (), |row| row.get(0)))
            .await
            .expect("client unable to fetch journal_mode");
        assert_eq!(mode, expected, "{journal_mode:?} journal mode mismatch");
        client.close().await.expect("closing client");
    }
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

async fn test_default_pool_in_memory_uses_one_connection() {
    let pool = PoolBuilder::new()
        .open()
        .await
        .expect("pool unable to be opened");

    pool.conn(|conn| {
        conn.execute(
            "CREATE TABLE testing (id INTEGER PRIMARY KEY, val TEXT NOT NULL)",
            (),
        )?;
        conn.execute("INSERT INTO testing VALUES (1, ?)", ["value1"])
    })
    .await
    .expect("writing schema and seed data");

    pool.conn(|conn| {
        let val: String =
            conn.query_row("SELECT val FROM testing WHERE id=?", [1], |row| row.get(0))?;
        assert_eq!(val, "value1");
        Ok(())
    })
    .await
    .expect("querying for result");

    let results = pool.conn_for_each(|_| Ok(())).await;
    assert_eq!(results.len(), 1);

    pool.close().await.expect("closing pool");

    let pool = PoolBuilder::new()
        .path(":memory:")
        .open()
        .await
        .expect("pool unable to be opened");
    let results = pool.conn_for_each(|_| Ok(())).await;
    assert_eq!(results.len(), 1);
    pool.close().await.expect("closing pool");
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

async fn test_pool_rejects_multi_connection_anonymous_memory() {
    let err = match PoolBuilder::new().num_conns(2).open().await {
        Ok(pool) => {
            pool.close().await.expect("closing unexpected pool");
            panic!("expected pool open to fail");
        }
        Err(err) => err,
    };

    assert_config_message(
        err,
        "anonymous in-memory pools cannot use multiple connections; call path(...) for file-backed pools or shared_memory(...) for named shared in-memory pools",
    );

    let err = match PoolBuilder::new()
        .path(":memory:")
        .num_conns(2)
        .open()
        .await
    {
        Ok(pool) => {
            pool.close().await.expect("closing unexpected pool");
            panic!("expected pool open to fail");
        }
        Err(err) => err,
    };

    assert_config_message(
        err,
        "anonymous in-memory pools cannot use multiple connections; call path(...) for file-backed pools or shared_memory(...) for named shared in-memory pools",
    );
}

async fn test_shared_memory_pool() {
    let name = shared_memory_name("shared-pool");
    let pool = PoolBuilder::new()
        .shared_memory(&name)
        .num_conns(2)
        .open()
        .await
        .expect("pool unable to be opened");

    let results = pool.conn_for_each(|_| Ok(())).await;
    assert_eq!(results.len(), 2);

    pool.conn(|conn| {
        conn.execute(
            "CREATE TABLE testing (id INTEGER PRIMARY KEY, val TEXT NOT NULL)",
            (),
        )?;
        conn.execute("INSERT INTO testing VALUES (1, ?)", ["value1"])
    })
    .await
    .expect("writing schema and seed data");

    let results = pool
        .conn_for_each(|conn| {
            conn.query_row("SELECT val FROM testing WHERE id=?", [1], |row| {
                row.get::<_, String>(0)
            })
        })
        .await;

    for result in results {
        assert_eq!(result.unwrap(), "value1");
    }

    pool.close().await.expect("closing pool");
}

async fn test_shared_memory_rejects_empty_name() {
    let err = match PoolBuilder::new().shared_memory("").open().await {
        Ok(pool) => {
            pool.close().await.expect("closing unexpected pool");
            panic!("expected pool open to fail");
        }
        Err(err) => err,
    };

    assert_config_message(err, "shared memory database name must not be empty");
}

async fn test_pool_journal_mode() {
    for (journal_mode, expected) in journal_modes() {
        let tmp_dir = tempfile::tempdir().unwrap();
        let pool = PoolBuilder::new()
            .journal_mode(journal_mode)
            .path(tmp_dir.path().join("sqlite.db"))
            .num_conns(4)
            .open()
            .await
            .expect("pool unable to be opened");

        let results = pool
            .conn_for_each(|conn| conn.query_row("PRAGMA journal_mode", (), |row| row.get(0)))
            .await;
        for (idx, result) in results.into_iter().enumerate() {
            let mode: String = result.unwrap();
            assert_eq!(
                mode, expected,
                "{journal_mode:?} journal mode mismatch on connection {idx}"
            );
        }

        pool.close().await.expect("closing pool");
    }
}

async fn test_pool_conn_for_each() {
    // make dummy db
    let tmp_dir = tempfile::tempdir().unwrap();
    {
        let client = ClientBuilder::new()
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
    }

    let pool = PoolBuilder::new()
        .path(tmp_dir.path().join("another-sqlite.db"))
        .num_conns(2)
        .open()
        .await
        .expect("pool unable to be opened");

    let dummy_db_path = tmp_dir.path().join("sqlite.db");
    let attach_fn = move |conn: &rusqlite::Connection| {
        conn.execute(
            "ATTACH DATABASE ? AS dummy",
            [dummy_db_path.to_str().unwrap()],
        )
    };
    // attach to the dummy db via conn_for_each
    let results = pool.conn_for_each(attach_fn).await;
    for result in results {
        result.unwrap();
    }

    // check that the dummy db is attached
    fn check_fn(conn: &rusqlite::Connection) -> Result<Vec<String>, rusqlite::Error> {
        let mut stmt = conn
            .prepare_cached("SELECT name FROM dummy.sqlite_master WHERE type='table'")
            .unwrap();
        let names = stmt
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect::<Vec<String>>();

        Ok(names)
    }
    let res = pool.conn_for_each(check_fn).await;
    for r in res {
        assert_eq!(r.unwrap(), vec!["testing"]);
    }

    // cleanup
    pool.close().await.expect("closing client conn");
}

async fn test_pool_close_concurrent() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let pool = PoolBuilder::new()
        .path(tmp_dir.path().join("sqlite.db"))
        .num_conns(2)
        .open()
        .await
        .expect("pool unable to be opened");

    let c1 = pool.close();
    let c2 = pool.close();
    futures_util::future::join_all([c1, c2])
        .await
        .into_iter()
        .collect::<Result<Vec<_>, Error>>()
        .expect("closing concurrently");

    let res = pool.conn(|c| c.execute("SELECT 1", ())).await;
    assert!(matches!(res, Err(Error::Closed)));
}

async fn test_canceled_async_command_is_skipped() {
    let client = ClientBuilder::new()
        .open()
        .await
        .expect("client unable to be opened");

    client
        .conn(|conn| {
            conn.execute("CREATE TABLE testing (id INTEGER PRIMARY KEY)", ())?;
            Ok(())
        })
        .await
        .expect("creating table");

    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let mut blocker = Box::pin(client.conn(move |_| {
        started_tx.send(()).expect("notifying blocker started");
        release_rx.recv().expect("waiting for blocker release");
        Ok(())
    }));

    assert!(blocker.as_mut().now_or_never().is_none());
    started_rx.recv().expect("waiting for blocker to start");

    let mut canceled = Box::pin(client.conn(|conn| {
        conn.execute("INSERT INTO testing VALUES (1)", ())?;
        Ok(())
    }));
    assert!(canceled.as_mut().now_or_never().is_none());
    drop(canceled);

    release_tx.send(()).expect("releasing blocker");
    blocker.await.expect("blocker finished");

    let row_count: i64 = client
        .conn(|conn| conn.query_row("SELECT COUNT(*) FROM testing", (), |row| row.get(0)))
        .await
        .expect("counting rows");
    assert_eq!(row_count, 0);

    client.close().await.expect("closing client");
}

async fn test_client_queue_capacity_reports_full() {
    let client = ClientBuilder::new()
        .queue_capacity(1)
        .open()
        .await
        .expect("client unable to be opened");

    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let mut blocker = Box::pin(client.conn(move |_| {
        started_tx.send(()).expect("notifying blocker started");
        release_rx.recv().expect("waiting for blocker release");
        Ok(())
    }));

    assert!(blocker.as_mut().now_or_never().is_none());
    started_rx.recv().expect("waiting for blocker to start");

    let mut queued = Box::pin(client.conn(|_| Ok(())));
    assert!(queued.as_mut().now_or_never().is_none());

    let res: Result<(), Error> = client.conn(|_| Ok(())).await;
    assert!(matches!(res, Err(Error::QueueFull)));

    release_tx.send(()).expect("releasing blocker");
    blocker.await.expect("blocker finished");
    queued.await.expect("queued command finished");

    client.close().await.expect("closing client");
}

async fn test_closure_panic_surfaces_error() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let client = ClientBuilder::new()
        .path(tmp_dir.path().join("sqlite.db"))
        .open()
        .await
        .expect("client unable to be opened");

    let res: Result<(), Error> = client.conn(|_| panic!("boom: &str")).await;
    match res {
        Err(Error::Panic { message }) => assert!(message.contains("boom"), "got {message}"),
        other => panic!("expected Error::Panic, got {other:?}"),
    }

    let res: Result<(), Error> = client
        .conn(|_| panic!("{}", String::from("boom: String")))
        .await;
    match res {
        Err(Error::Panic { message }) => assert!(message.contains("boom"), "got {message}"),
        other => panic!("expected Error::Panic, got {other:?}"),
    }

    // Connection must remain usable after a panic.
    client
        .conn(|conn| conn.query_row("SELECT 1", (), |row| row.get::<_, i64>(0)))
        .await
        .expect("connection still usable after panic");

    client.close().await.expect("closing client");
}

async fn test_panic_after_begin_immediate_rolls_back() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let db_path = tmp_dir.path().join("sqlite.db");
    let client = ClientBuilder::new()
        .path(&db_path)
        .open()
        .await
        .expect("client unable to be opened");

    client
        .conn(|conn| {
            conn.execute(
                "CREATE TABLE testing (id INTEGER PRIMARY KEY, val TEXT NOT NULL)",
                (),
            )?;
            Ok(())
        })
        .await
        .expect("creating table");

    let res: Result<(), Error> = client
        .conn(|conn| {
            conn.execute_batch("BEGIN IMMEDIATE")?;
            conn.execute("INSERT INTO testing VALUES (1, ?)", ["panic"])?;
            panic!("boom after BEGIN IMMEDIATE");
        })
        .await;
    match res {
        Err(Error::Panic { message }) => assert!(message.contains("boom"), "got {message}"),
        other => panic!("expected Error::Panic, got {other:?}"),
    }

    let row_count: i64 = client
        .conn(|conn| conn.query_row("SELECT COUNT(*) FROM testing", (), |row| row.get(0)))
        .await
        .expect("counting rows after rollback");
    assert_eq!(row_count, 0);

    let other = rusqlite::Connection::open(&db_path).expect("opening second connection");
    other
        .busy_timeout(std::time::Duration::from_millis(0))
        .expect("setting busy timeout");
    other
        .execute("INSERT INTO testing VALUES (2, ?)", ["other"])
        .expect("second connection can write after panic rollback");

    client.close().await.expect("closing client");
}

async fn test_pool_num_conns_zero_clamps() {
    let tmp_dir = tempfile::tempdir().unwrap();
    let pool = PoolBuilder::new()
        .path(tmp_dir.path().join("clamp.db"))
        .num_conns(0)
        .open()
        .await
        .expect("pool unable to be opened");
    let results = pool.conn_for_each(|_| Ok(())).await;
    assert_eq!(results.len(), 1);
}
