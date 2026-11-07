-- Fixture schema for the bddkit MySQL/MariaDB example. Applied automatically on
-- the first container start (docker-entrypoint-initdb.d), or manually:
--   docker compose exec -T mysql   mysql -uroot -proot < examples/db/init-mysql.sql
--   docker compose exec -T mariadb mysql -uroot -proot < examples/db/init-mysql.sql
--
-- One file serves both engines: MariaDB accepts this DDL unchanged.
--
-- It deliberately does not touch `apibdd_it`, the database the integration
-- tests own (created by MYSQL_DATABASE/MARIADB_DATABASE in docker-compose.yml,
-- with its tables managed by the test harness).
--
-- Here a schema IS a database, so the "second schema" of the Postgres fixture
-- becomes a second database, reached by a second DSN — not by search_path.

CREATE DATABASE IF NOT EXISTS apibdd_demo;

CREATE TABLE IF NOT EXISTS apibdd_demo.companies (
    -- AUTO_INCREMENT: bddkit omits the column from the INSERT and reads the
    -- generated value back into <<last_insert_id_companies>>.
    id         int AUTO_INCREMENT PRIMARY KEY,
    slug       varchar(255) NOT NULL,
    name       varchar(255),
    deleted_at datetime NULL
);

CREATE TABLE IF NOT EXISTS apibdd_demo.users (
    -- char(36) primary key with no server default: bddkit generates a UUIDv7
    -- client-side. A binary(16) UUID column would be refused instead — see the
    -- portability table in examples/README.md.
    id    char(36) PRIMARY KEY,
    email varchar(255) NOT NULL
);

CREATE TABLE IF NOT EXISTS apibdd_demo.invoices (
    id         int AUTO_INCREMENT PRIMARY KEY,
    company_id int NOT NULL,
    number     varchar(64) NOT NULL,
    amount     decimal(12, 2) NOT NULL,
    -- NOT NULL with a server default: omitted from the INSERT, filled by the server.
    created_at datetime NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT fk_invoices_company FOREIGN KEY (company_id) REFERENCES apibdd_demo.companies (id)
);

-- A second database for the `I use "reporting" connection` step.
CREATE DATABASE IF NOT EXISTS apibdd_demo_reporting;

CREATE TABLE IF NOT EXISTS apibdd_demo_reporting.audit_log (
    id        int AUTO_INCREMENT PRIMARY KEY,
    message   varchar(255) NOT NULL,
    logged_at datetime NOT NULL DEFAULT CURRENT_TIMESTAMP
);
