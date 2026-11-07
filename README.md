# bddkit-db

A bddkit plugin that runs database steps against Postgres, MySQL and MariaDB as a resource group.

Status: the plugin loads, validates its connections and opens one pool per connection at startup. The steps follow.

## Develop

The tests need a `bddkit` host built with `--features dev-unreserve` (a build that lets a plugin claim `db`) and the three databases.

```sh
cargo install --git https://github.com/bddkit/bddkit --tag v0.5.0-rc.1 --locked --features dev-unreserve --root ~/bddkit-host bddkit
BDDKIT_BIN=~/bddkit-host/bin/bddkit cargo test -- --test-threads=1
```

The databases are expected at `127.0.0.1:5433` (Postgres), `:3307` (MySQL) and `:3308` (MariaDB), or at the DSNs in `BDDKIT_TEST_DSN`, `BDDKIT_TEST_MYSQL_DSN` and `BDDKIT_TEST_MARIADB_DSN`. Without `BDDKIT_BIN` the end-to-end tests skip, and under `CI` they fail instead.
