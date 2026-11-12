# Running the examples

Three suites, one per engine. Run them from the repository root with a bddkit that loads this plugin (see the README), against databases loaded with the scripts in `examples/db/`. The configs have no `resources.api`: a suite that declares one needs the API plugin too.

## Postgres — `examples/db.yaml`

```bash
psql -v ON_ERROR_STOP=1 postgres://postgres:postgres@127.0.0.1:5433/postgres -f examples/db/init.sql
bddkit run --config examples/db.yaml
```

`db-features/db.feature` walks through every database step: inserts (one-liner, table, multi-table), update, delete, extraction into a variable, row-presence assertions, the four condition operators (`col:`, `col!:`, `col~:`, `col!~:`), procedures, functions, sequences, and a second connection. It is deliberately Postgres-only: see the table below for what that costs.

`db-features/cleanup.feature` is the run-scoped cleanup pattern: `<<run_id>>` plus a `~:` on the column removes exactly the companies this run inserted. Both files carry `@serial(demo)`, so they are one chain, and the cleanup, tagged `@priority(-100)`, is the last file of it. Without the shared `@serial` tag the two files are separate chains and can run at the same time, which is the one way this pattern goes wrong.

## MySQL — `examples/db-mysql.yaml`

```bash
mysql -h127.0.0.1 -P3307 -uroot -proot < examples/db/init-mysql.sql
bddkit run --config examples/db-mysql.yaml
```

`db-features-mysql/db.feature` is the engine-neutral file: it uses only vocabulary that behaves identically on MySQL and MariaDB, and passes unchanged on both. If a step needs an engine-specific spelling it does not belong in that file.

## MariaDB — `examples/db-mariadb.yaml`

```bash
mysql -h127.0.0.1 -P3308 -uroot -proot < examples/db/init-mysql.sql
mysql -h127.0.0.1 -P3308 -uroot -proot < examples/db/init-mariadb.sql
bddkit run --config examples/db-mariadb.yaml
```

MariaDB is MySQL plus sequences and `RETURNING`, so this config runs two directories: the engine-neutral file above, reused rather than copied, and `db-features-mariadb/db.feature`, which is only the remainder. A key filled by a server `DEFAULT` that is not `AUTO_INCREMENT` (`tickets.id`) is the only way `RETURNING` shows from Gherkin; on MySQL the same step is refused before the insert, naming the column. `init-mariadb.sql` would fail MySQL's own startup because of `CREATE SEQUENCE`.

## What does not port between the three

| Feature | Postgres | MySQL | MariaDB | Why |
|---|---|---|---|---|
| `I get next value of sequence` | yes | **no** | yes | MySQL has no sequences, so a file using it is no longer engine-neutral |
| `search_path` in `resources.db` | yes | **no** | **no** | A schema is a database there. The key is refused at startup; name the database in the DSN, or qualify a table as `database.table` |
| `binary`/`varbinary` columns | n/a | **no** | **no** | `UUID_TO_BIN`'s byte order is recorded nowhere in `information_schema`, so the layer refuses rather than guess. Use `char(36)`, as `init-mysql.sql` does; a client-side UUIDv7 fills it. Every width is refused, since the layer binds and compares as text |
| A key filled by a server `DEFAULT` that is not `AUTO_INCREMENT` | yes | **no** | yes | Reading the value back needs `RETURNING`. Give the value explicitly, or use `AUTO_INCREMENT` |
| The text an `I extract` yields | each its own | each its own | each its own | Portable as a step, not as a value: `now()` is `2026-08-29 15:11:50.884052+00` on Postgres and `2026-08-29 15:11:50` on the others, a boolean is `true` or `1` |
| `col~:` / `col!~:` | case-sensitive | **case-insensitive** | **case-insensitive** | The operator and the escaping are identical; MySQL and MariaDB match under the column's collation. The run prefix `<<run_id>>` is lowercase, so the cleanup pattern is unaffected |
| `I call procedure` / `I call function` | yes | yes | yes | The steps are portable, the routine bodies are not. The Postgres routines are `LANGUAGE sql`, which is why they are absent from the others |

Every other step is spelled the same and behaves the same on all three, which `db-features-mysql/db.feature` shows by passing unchanged from two configs.
