-- Fixture schema for the bddkit DB example. Applied automatically on the first
-- container start (docker-entrypoint-initdb.d), or manually:
--   docker compose exec -T db psql -U postgres -f - < examples/db/init.sql

CREATE SCHEMA IF NOT EXISTS apibdd_demo;

CREATE TABLE IF NOT EXISTS apibdd_demo.companies (
    id         int GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    slug       text NOT NULL,
    name       text,
    balance    numeric NOT NULL DEFAULT 0,
    deleted_at timestamptz
);

CREATE TABLE IF NOT EXISTS apibdd_demo.invoices (
    id         int GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    company_id int NOT NULL REFERENCES apibdd_demo.companies (id),
    number     text NOT NULL,
    amount     numeric NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE SEQUENCE IF NOT EXISTS apibdd_demo.invoice_seq;

-- Function: returns a scalar value → the "I call function ... as" step.
CREATE OR REPLACE FUNCTION apibdd_demo.next_invoice_number()
RETURNS text AS $$
    SELECT 'INV-' || nextval('apibdd_demo.invoice_seq')::text;
$$ LANGUAGE sql;

-- Procedure: returns nothing, effect only → the "I call procedure ... with" step.
-- bigint/numeric parameters: step arguments are bound as int8/f64 (see db/value.rs
-- infer_arg), and int8 → int4 is not an implicit cast in Postgres — only int8 → int4
-- via a matching bigint parameter resolves without a "does not exist" error.
CREATE OR REPLACE PROCEDURE apibdd_demo.recalc_balance(p_company_id bigint, p_delta numeric)
LANGUAGE sql AS $$
    UPDATE apibdd_demo.companies SET balance = balance + p_delta WHERE id = p_company_id;
$$;

-- A second schema/connection for the "I use ... connection" step.
CREATE SCHEMA IF NOT EXISTS apibdd_demo_reporting;

CREATE TABLE IF NOT EXISTS apibdd_demo_reporting.audit_log (
    id        int GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    message   text NOT NULL,
    logged_at timestamptz NOT NULL DEFAULT now()
);
