//! The statements the steps run. A step gets a [`Ctx`]: the pools, the
//! connection a bare table reference means, and everything the reply carries
//! back to the host: result variables, debug lines and evidence.

use std::collections::BTreeMap;
use std::future::Future;
use std::time::Instant;

use serde_json::{Value, json};
use sqlx::any::AnyArguments;
use sqlx::{Any, query::Query};

use crate::db::{Db, Pooled, text_col};
use crate::error::StepError;
use crate::sql::plan::{self, InsertPlan, PkSource};
use crate::sql::platform::Platform;
use crate::sql::reference::TableRef;
use crate::sql::{bind_all, db_read, sql_failure, value};

/// One dispatch: what it runs on and what it leaves behind.
pub struct Ctx<'a> {
    db: &'a Db,
    /// The connection a bare table reference, a call or a sequence means.
    instance: &'a str,
    debug: bool,
    evidence: bool,
    /// The variables the step sets; the reply publishes them on `passed` only.
    pub vars: BTreeMap<String, String>,
    output: Vec<Value>,
    diagnostics: Vec<Value>,
}

impl<'a> Ctx<'a> {
    pub fn new(db: &'a Db, instance: &'a str, debug: bool, evidence: bool) -> Self {
        Self {
            db,
            instance,
            debug,
            evidence,
            vars: BTreeMap::new(),
            output: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    /// What the reply carries besides the variables: debug lines, evidence.
    pub fn into_reply_parts(self) -> (Vec<Value>, Vec<Value>) {
        (self.output, self.diagnostics)
    }

    /// A debug line, only while the scenario is in debug mode.
    fn debug_line(&mut self, text: &str) {
        if self.debug {
            self.output.push(json!({"text": text, "syntax": null}));
        }
    }

    /// The SQL, its parameters and the notes the planner made.
    fn log_sql(&mut self, sql: &str, binds: &[Option<String>], logs: &[String]) {
        self.debug_line(&format!("SQL: {sql}"));
        self.debug_line(&format!("PARAMETERS: {binds:?}"));
        for l in logs {
            self.debug_line(&format!("  auto: {l}"));
        }
    }
}

/// What a statement is, for the evidence: the connection it ran over, the
/// SQL, and the values bound to it.
struct Stmt<'a> {
    connection: String,
    sql: &'a str,
    binds: &'a [Option<String>],
}

/// Runs a query future, printing its duration in debug mode right after
/// `log_sql`'s output — on failure too, since a slow query that ends in an
/// error is exactly what this is meant to diagnose (issue #54) — and keeping
/// the statement as evidence. The diagnostic is evidence only: a failed step's
/// dump never carries SQL.
async fn timed<T, E>(
    ctx: &mut Ctx<'_>,
    stmt: &Stmt<'_>,
    fut: impl Future<Output = Result<T, E>>,
) -> Result<T, E> {
    let start = Instant::now();
    let result = fut.await;
    let elapsed = start.elapsed();
    ctx.debug_line(&format!("TIME: {:.2} ms", elapsed.as_secs_f64() * 1000.0));
    if ctx.evidence {
        ctx.diagnostics.push(json!({
            "kind": "sql",
            "content": {
                "sql": stmt.sql,
                "binds": stmt.binds,
                "duration_us": u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX),
            },
            "resource": stmt.connection,
            "in_dump": false,
        }));
    }
    result
}

/// A table reference resolved against the connections.
struct Resolved<'a> {
    state: &'a Pooled,
    schema: std::sync::Arc<plan::TableSchema>,
    tref: TableRef,
    connection: String,
}

impl Resolved<'_> {
    fn platform(&self) -> &'static dyn Platform {
        self.state.engine.platform()
    }
}

/// The connection comes from the reference's prefix, else from the step's
/// instance.
async fn resolve<'a>(
    db: &'a Db,
    instance: &str,
    raw_table: &str,
) -> Result<Resolved<'a>, StepError> {
    let tref = TableRef::parse(raw_table).map_err(StepError::input)?;
    let connection = tref.conn.clone().unwrap_or_else(|| instance.to_string());
    let state = db.connection(&connection)?;
    let schema = db.schema(&connection, &tref).await?;
    Ok(Resolved {
        state,
        schema,
        tref,
        connection,
    })
}

/// Inserts one row; sets the PK values in `last_insert_*`.
pub async fn insert(
    ctx: &mut Ctx<'_>,
    raw_table: &str,
    values: &[(String, Option<String>)],
    index: Option<usize>,
) -> Result<(), StepError> {
    let r = resolve(ctx.db, ctx.instance, raw_table).await?;
    let InsertPlan {
        sql,
        binds,
        logs,
        has_returning,
        pk_vars,
    } = plan::build_insert(
        r.platform(),
        &r.schema,
        &r.tref.sql_name(),
        &r.tref.table,
        values,
        index,
    )?;
    ctx.log_sql(&sql, &binds, &logs);
    let stmt = Stmt {
        connection: r.connection.clone(),
        sql: &sql,
        binds: &binds,
    };
    let pool = &r.state.pool;
    let sql_name = r.tref.sql_name();

    if pk_vars.is_empty() {
        timed(
            ctx,
            &stmt,
            bind_all(sqlx::query(&sql), &binds).execute(pool),
        )
        .await
        .map_err(|e| sql_failure(&e, format!("INSERT into {sql_name}: {e}")))?;
        return Ok(());
    }

    let assignments: Vec<(String, String)> = if has_returning {
        let row = timed(
            ctx,
            &stmt,
            bind_all(sqlx::query(&sql), &binds).fetch_one(pool),
        )
        .await
        .map_err(|e| sql_failure(&e, format!("INSERT into {sql_name}: {e}")))?;
        // PK values come back as text, per Platform::returning, in pk_vars order.
        let mut assignments = Vec::new();
        for (i, (name, _)) in pk_vars.iter().enumerate() {
            let v = text_col(&row, i)
                .map_err(|e| db_read(format!("reading RETURNING: {e}")))?
                .ok_or_else(|| db_read("reading RETURNING: unexpected NULL"))?;
            assignments.push((name.clone(), v));
        }
        assignments
    } else {
        // No RETURNING (MySQL): a plain INSERT. build_insert has already
        // refused any PkSource::Unknown here, so only Known/AutoIncrement
        // reach this match — reading an auto-increment id off the INSERT's
        // own AnyQueryResult, never a second query: that query would run over
        // a pooled connection that hands the next statement to whichever
        // connection is free, and under concurrency that is normally another
        // file's id, not rarely.
        let result = timed(
            ctx,
            &stmt,
            bind_all(sqlx::query(&sql), &binds).execute(pool),
        )
        .await
        .map_err(|e| sql_failure(&e, format!("INSERT into {sql_name}: {e}")))?;
        let mut assignments = Vec::new();
        for (name, source) in &pk_vars {
            let v = match source {
                PkSource::Known(v) => v.clone(),
                PkSource::AutoIncrement => result
                    .last_insert_id()
                    // 0 is what MySQL's OK packet carries when nothing was
                    // generated — not a real id, so it must not become one.
                    .filter(|id| *id > 0)
                    .ok_or_else(|| {
                        db_read(format!(
                            "INSERT into {sql_name}: expected an auto-increment id in the INSERT result, got none"
                        ))
                    })?
                    .to_string(),
                PkSource::Unknown(col) => {
                    return Err(db_read(format!(
                        "INSERT into {sql_name}: no source for the primary key {col}"
                    )));
                }
            };
            assignments.push((name.clone(), v));
        }
        assignments
    };
    ctx.vars.extend(assignments);
    Ok(())
}

/// UPDATE ... SET ... WHERE ...; sets the number of affected rows in `updated_<table>`.
pub async fn update(
    ctx: &mut Ctx<'_>,
    raw_table: &str,
    set: &str,
    where_: &str,
) -> Result<(), StepError> {
    let r = resolve(ctx.db, ctx.instance, raw_table).await?;
    let set_pairs = value::parse_oneliner(set).map_err(StepError::input)?;
    let where_pairs = value::parse_oneliner(where_).map_err(StepError::input)?;
    let sql_name = r.tref.sql_name();
    let (sql, binds) =
        plan::build_update(r.platform(), &r.schema, &sql_name, &set_pairs, &where_pairs)?;
    ctx.log_sql(&sql, &binds, &[]);
    let stmt = Stmt {
        connection: r.connection.clone(),
        sql: &sql,
        binds: &binds,
    };
    let done = timed(
        ctx,
        &stmt,
        bind_all(sqlx::query(&sql), &binds).execute(&r.state.pool),
    )
    .await
    .map_err(|e| sql_failure(&e, format!("UPDATE {sql_name}: {e}")))?;
    ctx.vars.insert(
        format!("updated_{}", r.tref.table),
        done.rows_affected().to_string(),
    );
    Ok(())
}

/// DELETE FROM ... WHERE ... (an empty WHERE is rejected by the builder);
/// sets the number of deleted rows in `deleted_<table>`.
pub async fn delete(ctx: &mut Ctx<'_>, raw_table: &str, where_: &str) -> Result<(), StepError> {
    let r = resolve(ctx.db, ctx.instance, raw_table).await?;
    let where_pairs = value::parse_oneliner(where_).map_err(StepError::input)?;
    let sql_name = r.tref.sql_name();
    let (sql, binds) = plan::build_delete(r.platform(), &r.schema, &sql_name, &where_pairs)?;
    ctx.log_sql(&sql, &binds, &[]);
    let stmt = Stmt {
        connection: r.connection.clone(),
        sql: &sql,
        binds: &binds,
    };
    let done = timed(
        ctx,
        &stmt,
        bind_all(sqlx::query(&sql), &binds).execute(&r.state.pool),
    )
    .await
    .map_err(|e| sql_failure(&e, format!("DELETE {sql_name}: {e}")))?;
    ctx.vars.insert(
        format!("deleted_{}", r.tref.table),
        done.rows_affected().to_string(),
    );
    Ok(())
}

/// Checks row presence with a fresh query on every assertion attempt.
pub async fn exists(
    ctx: &mut Ctx<'_>,
    raw_table: &str,
    where_pairs: &[(String, Option<String>)],
) -> Result<bool, StepError> {
    let r = resolve(ctx.db, ctx.instance, raw_table).await?;
    let sql_name = r.tref.sql_name();
    let (sql, binds) = plan::build_exists(r.platform(), &r.schema, &sql_name, where_pairs)?;
    ctx.log_sql(&sql, &binds, &[]);
    let stmt = Stmt {
        connection: r.connection.clone(),
        sql: &sql,
        binds: &binds,
    };
    Ok(timed(
        ctx,
        &stmt,
        bind_all(sqlx::query(&sql), &binds).fetch_optional(&r.state.pool),
    )
    .await
    .map_err(|e| sql_failure(&e, format!("checking presence in {sql_name}: {e}")))?
    .is_some())
}

/// Reads one column (cast to text via `Platform::cast_text`) from the first
/// row matching a WHERE clause; sets the value in a variable.
pub async fn extract(
    ctx: &mut Ctx<'_>,
    column: &str,
    raw_table: &str,
    where_str: &str,
    var: &str,
) -> Result<(), StepError> {
    let r = resolve(ctx.db, ctx.instance, raw_table).await?;
    let sql_name = r.tref.sql_name();
    // The read column is a plain name — the one position where a name is
    // neither a condition nor a written value, and so takes no operator.
    plan::plain_column(column).map_err(StepError::input)?;
    let col = r
        .schema
        .col(column)
        .ok_or_else(|| db_read(format!("column {column:?} is missing from {sql_name}")))?;
    r.platform().check_bindable(col).map_err(StepError::input)?;
    let where_pairs = value::parse_oneliner(where_str).map_err(StepError::input)?;
    let (where_sql, binds) = plan::build_where(r.platform(), &r.schema, &where_pairs, 1)?;
    let sql = format!(
        "SELECT {} FROM {sql_name} WHERE {where_sql} LIMIT 1",
        r.platform().cast_text(column),
    );
    ctx.log_sql(&sql, &binds, &[]);
    let stmt = Stmt {
        connection: r.connection.clone(),
        sql: &sql,
        binds: &binds,
    };
    let row = timed(
        ctx,
        &stmt,
        bind_all(sqlx::query(&sql), &binds).fetch_optional(&r.state.pool),
    )
    .await
    .map_err(|e| sql_failure(&e, format!("SELECT {sql_name}: {e}")))?
    .ok_or_else(|| {
        // The statement ran: the data the test expects is not there.
        StepError::assertion(format!("no row in {sql_name} matched the condition"))
    })?;
    let v = text_col(&row, 0).map_err(|e| db_read(format!("reading value: {e}")))?;
    ctx.vars.insert(var.to_string(), v.unwrap_or_default());
    Ok(())
}

/// DELETE FROM ... with no WHERE — a full table clear;
/// sets the number of deleted rows in `deleted_<table>`.
pub async fn delete_all(ctx: &mut Ctx<'_>, raw_table: &str) -> Result<(), StepError> {
    let r = resolve(ctx.db, ctx.instance, raw_table).await?;
    let sql_name = r.tref.sql_name();
    let sql = plan::build_delete_all(&sql_name);
    ctx.log_sql(&sql, &[], &[]);
    let stmt = Stmt {
        connection: r.connection.clone(),
        sql: &sql,
        binds: &[],
    };
    let done = timed(ctx, &stmt, sqlx::query(&sql).execute(&r.state.pool))
        .await
        .map_err(|e| sql_failure(&e, format!("DELETE ALL {sql_name}: {e}")))?;
    ctx.vars.insert(
        format!("deleted_{}", r.tref.table),
        done.rows_affected().to_string(),
    );
    Ok(())
}

/// Binds typed arguments for a procedure/function call.
fn bind_args<'q>(
    mut q: Query<'q, Any, AnyArguments<'q>>,
    args: &'q [value::Arg],
) -> Query<'q, Any, AnyArguments<'q>> {
    for a in args {
        q = match a {
            value::Arg::Null => q.bind(Option::<String>::None),
            value::Arg::Int(i) => q.bind(*i),
            value::Arg::Float(f) => q.bind(*f),
            value::Arg::Bool(b) => q.bind(*b),
            value::Arg::Text(t) => q.bind(t.as_str()),
        };
    }
    q
}

/// The arguments of a call as the text the evidence stream shows.
fn arg_texts(args: &[value::Arg]) -> Vec<Option<String>> {
    args.iter()
        .map(|a| match a {
            value::Arg::Null => None,
            value::Arg::Int(i) => Some(i.to_string()),
            value::Arg::Float(f) => Some(f.to_string()),
            value::Arg::Bool(b) => Some(b.to_string()),
            value::Arg::Text(t) => Some(t.clone()),
        })
        .collect()
}

/// Joins `n` placeholders in the platform's own syntax (e.g. `$1, $2, $3`),
/// for use in a `CALL`/`SELECT` argument list.
fn placeholder_list(p: &dyn Platform, n: usize) -> String {
    (1..=n)
        .map(|i| p.placeholder(i))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The state a call step runs on: the pool, the dialect and the connection.
fn call_state<'a>(ctx: &Ctx<'a>) -> Result<(&'a Pooled, &'static dyn Platform), StepError> {
    let state = ctx.db.connection(ctx.instance)?;
    Ok((state, state.engine.platform()))
}

/// Calls a procedure: `CALL name(...)` with positional arguments.
pub async fn call_procedure(
    ctx: &mut Ctx<'_>,
    name: &str,
    args_str: &str,
) -> Result<(), StepError> {
    let args = value::parse_args(args_str).map_err(StepError::input)?;
    let (state, platform) = call_state(ctx)?;
    let sql = format!("CALL {name}({})", placeholder_list(platform, args.len()));
    ctx.debug_line(&format!("SQL: {sql}"));
    ctx.debug_line(&format!("ARGUMENTS: {args:?}"));
    let binds = arg_texts(&args);
    let stmt = Stmt {
        connection: ctx.instance.to_string(),
        sql: &sql,
        binds: &binds,
    };
    timed(
        ctx,
        &stmt,
        bind_args(sqlx::query(&sql), &args).execute(&state.pool),
    )
    .await
    .map_err(|e| sql_failure(&e, format!("CALL {name}: {e}")))?;
    Ok(())
}

/// Calls a function, reading its result (cast to text via `Platform::cast_text`)
/// into a variable.
pub async fn call_function(
    ctx: &mut Ctx<'_>,
    name: &str,
    args_str: &str,
    var: &str,
) -> Result<(), StepError> {
    let args = value::parse_args(args_str).map_err(StepError::input)?;
    let (state, platform) = call_state(ctx)?;
    let call = format!("{name}({})", placeholder_list(platform, args.len()));
    let sql = format!("SELECT {}", platform.cast_text(&call));
    ctx.debug_line(&format!("SQL: {sql}"));
    ctx.debug_line(&format!("ARGUMENTS: {args:?}"));
    let binds = arg_texts(&args);
    let stmt = Stmt {
        connection: ctx.instance.to_string(),
        sql: &sql,
        binds: &binds,
    };
    let row = timed(
        ctx,
        &stmt,
        bind_args(sqlx::query(&sql), &args).fetch_one(&state.pool),
    )
    .await
    .map_err(|e| sql_failure(&e, format!("SELECT {name}(...): {e}")))?;
    let v = text_col(&row, 0).map_err(|e| db_read(format!("reading function result: {e}")))?;
    ctx.vars.insert(var.to_string(), v.unwrap_or_default());
    Ok(())
}

/// Advances a sequence via `Platform::next_sequence`; sets the value in a variable.
pub async fn next_sequence(ctx: &mut Ctx<'_>, seq: &str, var: &str) -> Result<(), StepError> {
    let (state, platform) = call_state(ctx)?;
    let (sql, binds) = platform.next_sequence(seq).ok_or_else(|| {
        StepError::input(format!(
            "sequences are not supported on {}",
            platform.name()
        ))
    })?;
    ctx.debug_line(&format!("SQL: {sql} [{seq}]"));
    let stmt = Stmt {
        connection: ctx.instance.to_string(),
        sql: &sql,
        binds: &binds,
    };
    let row = timed(
        ctx,
        &stmt,
        bind_all(sqlx::query(&sql), &binds).fetch_one(&state.pool),
    )
    .await
    .map_err(|e| sql_failure(&e, format!("next value of sequence {seq:?}: {e}")))?;
    let v = text_col(&row, 0)
        .map_err(|e| db_read(format!("reading sequence: {e}")))?
        .unwrap_or_default();
    ctx.vars.insert(var.to_string(), v);
    Ok(())
}
