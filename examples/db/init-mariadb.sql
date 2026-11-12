-- MariaDB-only additions to examples/db/init-mysql.sql, which both engines share.
--
-- Kept as a separate file, loaded only into MariaDB, rather than guarded inside
-- the shared one with MariaDB's executable comments
-- (`/*M!100300 CREATE SEQUENCE ... */`): a second file is obvious to anyone
-- reading examples/README.md, whereas a version-gated comment reads as a
-- comment right up until it does not. MySQL 8 has no CREATE SEQUENCE at all,
-- so an unguarded one in the shared file would fail on MySQL.
--
-- Load it after init-mysql.sql: apibdd_demo must exist before this runs.

CREATE SEQUENCE IF NOT EXISTS apibdd_demo.ticket_seq;

CREATE TABLE IF NOT EXISTS apibdd_demo.tickets (
    -- A primary key filled by a server-side DEFAULT that is NOT AUTO_INCREMENT.
    -- Reading the generated value back needs RETURNING, so bddkit refuses this
    -- table's insert on MySQL before it runs, and handles it on MariaDB.
    id      bigint NOT NULL DEFAULT NEXTVAL(apibdd_demo.ticket_seq) PRIMARY KEY,
    subject varchar(255) NOT NULL
);
