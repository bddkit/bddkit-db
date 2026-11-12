# bddkit-db

A bddkit plugin that runs database steps against Postgres, MySQL and MariaDB as a resource group.

## Install

```sh
bddkit plugin install bddkit/bddkit-db
```

Written against plugin ABI 4, so it needs a bddkit that loads that generation.

## Config

```yaml
resources:
  db:
    main:
      dsn: postgres://postgres:postgres@127.0.0.1:5433/postgres
      search_path: [app]
    reporting:
      dsn: mysql://root:root@127.0.0.1:3307/reporting
default_db: main
```

One pool per connection is opened at startup, so an unreachable database stops the run with exit 2 before any scenario. The engine comes from the DSN scheme (`postgres://`, `mysql://`); MySQL and MariaDB share one scheme and are told apart by a single `SELECT VERSION()`. Any scenario reaches any connection: `I use "reporting" db` switches the default for the rest of the scenario, and a table reference `reporting:audit.log` names one for one step. `I use "<name>" connection`, the old spelling, still works; `bddkit doctor` hints at the new one and `run` says nothing.

## Steps

`bddkit steps list db` prints them. Pairs and conditions are one-liners: comma-separated `col: value`, a literal comma written `\,`, SQL NULL written `<<null>>`.

| Step | Effect |
|---|---|
| `I have "<table>" with "<pairs>"` | inserts one row, filling the primary key and required columns from the table schema |
| `I have "<table>" where:` | one row per line of the table, its header naming the columns |
| `I have:` | rows into several tables, the first column naming each |
| `I update "<table>" with "<pairs>" where "<condition>"` | updates the matching rows |
| `I delete "<table>" where "<condition>"` / `I delete all "<table>"` | deletes the matching rows / every row |
| `I extract "<column>" from "<table>" with "<condition>" as "<name>"` | reads a column of the first matching row into a variable |
| `I should have "<table>" with "<pairs>"` / `with:` | a row matches; polled when the scenario arms an eventual assertion |
| `I should not have "<table>" with "<pairs>"` / `with:` | no row matches; polled the same way |
| `I call procedure "<name>" with "<arguments>"` | `CALL name(...)` |
| `I call function "<name>" with "<arguments>" as "<variable>"` | `SELECT name(...)` into a variable |
| `I get next value of sequence "<name>" as "<variable>"` | advances a sequence into a variable |

A condition column may end in an operator: `col:` is `=`, `col!:` is `<>`, `col~:` is `LIKE`, `col!~:` is `NOT LIKE`. The operator lives on the column, never in the value, so a `%` that arrives through a variable cannot widen a `DELETE`. Where a value is written (`I have`, the `SET` of `I update`) an operator is an error. With `<<null>>`, `col:` reads `IS NULL` and `col!:` reads `IS NOT NULL`.

Variables a step sets, readable as `<<...>>`:

- `last_insert_id_<table>` for a single-column `id` key, `last_insert_<table>_<column>` per column otherwise, and for `I have "<table>" where:` a `_<index>` suffix per row, a single row included. `I have "<table>" with` and `I have:` add no suffix, so in `I have:` a later row of one table overwrites the earlier one's variables.
- `updated_<table>` and `deleted_<table>`: the affected row count.

Variables travel back on a passed step only, so a failed step leaves none behind.

`I am in debug mode` prints each statement as `SQL:`, `PARAMETERS:` and `TIME:` lines, on a failing statement too. Under `concurrency` above 1 the time includes the wait for a free pooled connection. Under `bddkit run --events <file> --evidence` every statement is an `sql` evidence event naming the connection it ran on. A failed step's console dump never carries the SQL.

## Engines

PostgreSQL, MySQL 8 and MariaDB from 10.5, the release that adds `RETURNING`. Every step is spelled the same on all three; what does not carry over:

| Feature | Postgres | MySQL | MariaDB |
|---|---|---|---|
| `I get next value of sequence` | yes | no, MySQL has no sequences | yes |
| `search_path` | yes | refused at startup, a schema is a database there | refused at startup |
| `binary` / `varbinary` columns | n/a | refused, store a UUID as `char(36)` | refused |
| A key filled by a server `DEFAULT` that is not `AUTO_INCREMENT` | yes | refused before the insert, there is no `RETURNING` | yes |
| `col~:` | case-sensitive | case-insensitive under the default collation | case-insensitive |

What `I extract` yields is the engine's own text: `now()` reads back with a time zone on Postgres and without on the others, a boolean as `true` or `1`. It is portable over `varchar` columns and nowhere else.

## Running in parallel

Feature files run at the same time on one shared handle, and isolation is by data uniqueness, never by rollback.

- `I delete all "<table>"` and any fixed literal collide across files. Put such files in one `@serial(<name>)` chain, or write `<<unique()>>` values.
- A run-scoped cleanup is `I delete "companies" where "slug~: u<<run_id>>%"`, in a file tagged `@priority(-100)` inside the same `@serial` chain as the files whose rows it removes (`examples/db-features/cleanup.feature`).
- An auto-increment id is read off the insert's own result, never with `SELECT LAST_INSERT_ID()`: that is session state, and a pool hands the next statement to whichever connection is free.
- A negation with a value never matches a NULL column: `name!: keeper` is `name <> 'keeper'`, unknown where `name` is NULL.

Known ceilings: `I extract` cannot tell SQL NULL from an empty string and reads only the first matching row; table schemas are read once per run, so DDL run mid-suite stays invisible; `I call procedure` and `I call function` bind arguments by guessing int, float, bool or text from the text.

## Examples

`examples/` holds three suites, one per engine, run from the repository root against databases loaded with the scripts in `examples/db/`. `examples/README.md` says how.

## Develop

The tests need a `bddkit` host of `v0.5.0` or later and the three databases.

```sh
cargo install --git https://github.com/bddkit/bddkit --tag v0.5.0 --locked --root ~/bddkit-host bddkit
BDDKIT_BIN=~/bddkit-host/bin/bddkit cargo test -- --test-threads=1
```

The databases are expected at `127.0.0.1:5433` (Postgres), `:3307` (MySQL) and `:3308` (MariaDB), or at the DSNs in `BDDKIT_TEST_DSN`, `BDDKIT_TEST_MYSQL_DSN` and `BDDKIT_TEST_MARIADB_DSN`. Without `BDDKIT_BIN` the end-to-end tests skip, and under `CI` they fail instead. `tests/db.rs` and `tests/db_host.rs` run against one engine at a time, chosen by `BDDKIT_TEST_ENGINE` (`postgres` by default, `mysql`, `mariadb`); CI runs them once per value.
