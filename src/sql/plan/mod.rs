use std::collections::HashMap;

use super::db_read;
use super::platform::Platform;
use crate::error::StepError;

/// A table column from introspection data, as reported by the current
/// platform's `Platform::introspect` query. `type_name` is the platform's
/// native type name, as consumed by `Platform::bind`/`returning`.
#[derive(Debug, Clone)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "one flag per fact the introspection query reports; an enum per flag would only rename them"
)]
pub struct Column {
    pub name: String,
    pub type_name: String,
    /// Declared length/precision (e.g. `char(36)`), where the platform needs
    /// it to tell columns of the same `type_name` apart. NULL where it never
    /// matters — Postgres always returns NULL here.
    pub length: Option<i64>,
    pub not_null: bool,
    pub has_default: bool,
    pub is_identity: bool,
    pub is_pk: bool,
}

#[derive(Debug, Clone)]
pub struct TableSchema {
    pub columns: Vec<Column>,
}

impl TableSchema {
    pub fn col(&self, name: &str) -> Option<&Column> {
        self.columns.iter().find(|c| c.name == name)
    }
    pub fn pk_columns(&self) -> Vec<&Column> {
        self.columns.iter().filter(|c| c.is_pk).collect()
    }
}

/// Where the text value of one PK column's `last_insert_*` variable comes
/// from. Only consulted when the platform produced no `RETURNING` clause —
/// `ops::insert` reads the row back for that instead. See `Platform::returning`.
#[derive(Debug, Clone, PartialEq)]
pub enum PkSource {
    /// The text value is already known before the INSERT runs: given in the
    /// step, or a client-generated UUIDv7 (`build_insert` step 2).
    Known(String),
    /// Not known yet — read it off the INSERT's own result
    /// (`AnyQueryResult::last_insert_id`), never a second query.
    AutoIncrement,
    /// No source exists: a server-side default that is not auto-increment,
    /// or (composite PK) one part filled that way. Carries the column name
    /// so the caller can fail naming it. `build_insert` already refuses this
    /// case itself when there is no RETURNING (see below) — a variant this
    /// well-typed still beats an early bailout with no trace of why.
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct InsertPlan {
    pub sql: String,
    pub binds: Vec<Option<String>>,
    pub logs: Vec<String>,
    /// Whether `sql` ends in a `RETURNING`-equivalent clause.
    pub has_returning: bool,
    /// One entry per PK column, `(last_insert_* variable name, where its
    /// value comes from)` — one `Vec`, not two zipped by position, so
    /// misalignment between a name and its source cannot compile in one
    /// column being renamed and not the other.
    pub pk_vars: Vec<(String, PkSource)>,
}

/// Builds an INSERT: applies the PK and NOT NULL fill rules from spec §8.
/// `values` are the pairs given in the step (`None` = SQL NULL). `index` is the
/// variable-name suffix for the table form (`Some(0)` → `_0`).
pub fn build_insert(
    platform: &dyn Platform,
    schema: &TableSchema,
    sql_name: &str,
    bare: &str,
    values: &[(String, Option<String>)],
    index: Option<usize>,
) -> Result<InsertPlan, StepError> {
    let mut draft = Draft::default();
    draft.add_given(platform, schema, sql_name, values)?;
    let generated = draft.add_primary_keys(platform, schema, values)?;
    draft.add_not_null(platform, schema, values)?;

    let pk_cols = schema.pk_columns();
    let body = if draft.cols.is_empty() {
        platform.insert_no_columns(sql_name)
    } else {
        format!(
            "INSERT INTO {sql_name} ({}) VALUES ({})",
            draft.cols.join(", "),
            draft.exprs.join(", ")
        )
    };
    let returning_clause = platform.returning(&pk_cols);
    let has_returning = returning_clause.is_some();
    let sql = match returning_clause {
        Some(clause) => format!("{body} {clause}"),
        None => body,
    };

    let pk_vars = pk_sources(&pk_cols, values, &generated, bare, index);
    refuse_unreadable_key(&pk_vars, has_returning)?;
    Ok(InsertPlan {
        sql,
        binds: draft.binds,
        logs: draft.logs,
        has_returning,
        pk_vars,
    })
}

/// The pieces of an INSERT while the fill rules add columns to it.
#[derive(Default)]
struct Draft {
    cols: Vec<String>,
    exprs: Vec<String>,
    binds: Vec<Option<String>>,
    logs: Vec<String>,
}

impl Draft {
    fn push(&mut self, col: &str, expr: String) {
        self.cols.push(col.to_string());
        self.exprs.push(expr);
    }

    /// 1. Given values: as-is, cast to the column's type. Each must exist and
    ///    be bindable.
    fn add_given(
        &mut self,
        platform: &dyn Platform,
        schema: &TableSchema,
        sql_name: &str,
        values: &[(String, Option<String>)],
    ) -> Result<(), StepError> {
        for (col, val) in values {
            plain_column(col).map_err(StepError::input)?;
            let Some(c) = schema.col(col) else {
                return Err(db_read(format!(
                    "column {col:?} is missing from table {sql_name}"
                )));
            };
            platform.check_bindable(c).map_err(StepError::input)?;
            let expr = platform.bind(self.binds.len() + 1, &c.type_name);
            self.push(col, expr);
            self.binds.push(val.clone());
        }
        Ok(())
    }

    /// 2. Primary keys with no value. Returns each client-generated UUIDv7 by
    ///    PK column name, for `pk_sources`.
    fn add_primary_keys(
        &mut self,
        platform: &dyn Platform,
        schema: &TableSchema,
        values: &[(String, Option<String>)],
    ) -> Result<HashMap<String, String>, StepError> {
        let mut generated = HashMap::new();
        for pk in schema.pk_columns() {
            if values.iter().any(|(c, _)| c == &pk.name) {
                continue;
            }
            if pk.has_default || pk.is_identity {
                continue; // the platform will generate it itself (default or identity/auto-increment).
            }
            if !platform.wants_client_uuid(pk) {
                return Err(StepError::input(format!(
                    "primary key {} ({}) has no value and is not generated (no default, not uuid)",
                    pk.name, pk.type_name
                )));
            }
            let id = uuid::Uuid::now_v7().to_string();
            self.logs.push(format!("PK {} := {id} (UUIDv7)", pk.name));
            let expr = platform.bind(self.binds.len() + 1, &pk.type_name);
            self.push(&pk.name, expr);
            self.binds.push(Some(id.clone()));
            generated.insert(pk.name.clone(), id);
        }
        Ok(generated)
    }

    /// 3. NOT NULL with no value and no default.
    fn add_not_null(
        &mut self,
        platform: &dyn Platform,
        schema: &TableSchema,
        values: &[(String, Option<String>)],
    ) -> Result<(), StepError> {
        for c in &schema.columns {
            if values.iter().any(|(n, _)| n == &c.name) || c.is_pk || c.has_default || !c.not_null {
                continue;
            }
            if !platform.is_timestamplike(&c.type_name) {
                return Err(StepError::input(format!(
                    "column {} ({}) is NOT NULL, has no value and no default",
                    c.name, c.type_name
                )));
            }
            self.logs.push(format!("{} := now()", c.name));
            // "now()" is portable across Postgres, MySQL and MariaDB: no need
            // to route it through Platform.
            self.push(&c.name, "now()".to_string());
        }
        Ok(())
    }
}

/// One (variable name, source) pair per PK column, in `pk_cols` order, built
/// in a single pass so the name and its source can never drift apart.
fn pk_sources(
    pk_cols: &[&Column],
    values: &[(String, Option<String>)],
    generated: &HashMap<String, String>,
    bare: &str,
    index: Option<usize>,
) -> Vec<(String, PkSource)> {
    let suffix = index.map(|i| format!("_{i}")).unwrap_or_default();
    let single = pk_cols.len() == 1;
    pk_cols
        .iter()
        .map(|pk| {
            let name = if single {
                format!("last_insert_id_{bare}{suffix}")
            } else {
                format!("last_insert_{bare}_{}{suffix}", pk.name)
            };
            // A value already given (Some or explicit NULL) always wins over
            // how the column would otherwise have been filled, matching the
            // PK step above, which also skips a given column outright.
            let source = if let Some((_, v)) = values.iter().find(|(c, _)| c == &pk.name) {
                match v {
                    Some(s) => PkSource::Known(s.clone()),
                    // Given <<null>>: MySQL/MariaDB treat NULL into an
                    // AUTO_INCREMENT column the same as omitting it, so it
                    // still generates; anything else given as NULL has
                    // nothing to report.
                    None if pk.is_identity => PkSource::AutoIncrement,
                    None => PkSource::Unknown(pk.name.clone()),
                }
            } else if pk.is_identity {
                PkSource::AutoIncrement
            } else if let Some(id) = generated.get(&pk.name) {
                PkSource::Known(id.clone())
            } else {
                // has_default but not identity: a server-side DEFAULT we
                // cannot read back without RETURNING.
                PkSource::Unknown(pk.name.clone())
            };
            (name, source)
        })
        .collect()
}

/// Refuse before the INSERT runs, not after: a platform with no RETURNING and
/// a PK column with no source would otherwise commit the row and only then
/// report failure (invariant 1: check everything checkable first).
fn refuse_unreadable_key(
    pk_vars: &[(String, PkSource)],
    has_returning: bool,
) -> Result<(), StepError> {
    if !has_returning
        && let Some((_, PkSource::Unknown(col))) = pk_vars
            .iter()
            .find(|(_, s)| matches!(s, PkSource::Unknown(_)))
    {
        return Err(StepError::input(format!(
            "primary key {col} is server-generated (a DEFAULT that is not auto-increment) \
             and this platform has no RETURNING to read it back with — give the value explicitly"
        )));
    }
    Ok(())
}

/// The comparison one condition pair asks for. It is written as a suffix on
/// the COLUMN name (`slug~: u123%`), never inferred from the value's text —
/// so a `%` that arrives inside a variable can never turn an equality into a
/// pattern behind the author's back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Eq,
    Ne,
    Like,
    NotLike,
}

impl Op {
    /// The SQL keyword. A `match`, not an `if`, on purpose: this is the one
    /// place a new operator can be forgotten silently, because the arms of
    /// `build_where` below already pattern-match on `Op` and so refuse to
    /// compile until a new variant is placed. Adding `Gte` here without a
    /// keyword is a compile error; adding it to an `if op == Op::Eq` was not.
    fn sql(self) -> &'static str {
        match self {
            Op::Eq => "=",
            Op::Ne => "<>",
            Op::Like => "LIKE",
            Op::NotLike => "NOT LIKE",
        }
    }
}

/// Splits `slug!~` into (`slug`, `Op::NotLike`). Longest suffix first, so
/// `!~` cannot degrade into `!`. A leftover OPERATOR-SHAPED character is an
/// error naming what was written, because the alternative is a schema lookup
/// for a column called `price>=`, which reports a missing column and sends
/// the author to their DDL over a typo in their step. The guard covers
/// `!~<>=` only — `slug*:` still reports a missing column, which is the right
/// answer for a character no operator will ever start with.
fn split_operator(raw: &str) -> Result<(&str, Op), String> {
    const SUFFIXES: [(&str, Op); 3] = [("!~", Op::NotLike), ("~", Op::Like), ("!", Op::Ne)];
    let (name, op) = SUFFIXES
        .iter()
        .find_map(|(s, op)| raw.strip_suffix(s).map(|n| (n, *op)))
        .unwrap_or((raw, Op::Eq));
    let name = name.trim_end();
    if name.is_empty() || name.ends_with(|c: char| "!~<>=".contains(c)) {
        return Err(format!(
            "unknown operator in condition column {raw:?}; available: \
             `col:` (=), `col!:` (<>), `col~:` (LIKE), `col!~:` (NOT LIKE)"
        ));
    }
    Ok((name, op))
}

/// An operator belongs to a condition. Anywhere a value is WRITTEN it is a
/// typo, and letting it through would report a missing column `slug~` —
/// sending the author to look at their schema instead of at their step.
pub fn plain_column(raw: &str) -> Result<(), String> {
    match split_operator(raw)? {
        (_, Op::Eq) => Ok(()),
        _ => Err(format!(
            "column {raw:?}: an operator is only allowed in a condition, \
             not where a value is set"
        )),
    }
}

/// Builds `col <op> <bind> AND …`, in the platform's own bind syntax.
/// Parameter numbering starts at `start` (for UPDATE, where SET takes the
/// first parameters).
///
/// NULL is `col: <<null>>` → `IS NULL` and `col!: <<null>>` → `IS NOT NULL`,
/// both with no bind; a LIKE against NULL is refused rather than built, since
/// it can only ever match nothing.
///
/// Under `~`/`!~` the value is an SQL LIKE pattern in full: `%` and `_` both
/// match, and `\%` / `\_` are the literal characters — backslash is the
/// default LIKE escape on all three engines, so no ESCAPE clause is built.
/// The COLUMN side goes through `cast_text`, so a non-text column is matched
/// against its own text rendering instead of failing on the operator.
pub fn build_where(
    platform: &dyn Platform,
    schema: &TableSchema,
    pairs: &[(String, Option<String>)],
    start: usize,
) -> Result<(String, Vec<Option<String>>), StepError> {
    let mut parts = Vec::new();
    let mut binds = Vec::new();
    let mut param = start;
    for (raw, val) in pairs {
        let (col, op) = split_operator(raw).map_err(StepError::input)?;
        let c = schema
            .col(col)
            .ok_or_else(|| db_read(format!("column {col:?} is missing from the table")))?;
        platform.check_bindable(c).map_err(StepError::input)?;
        match (op, val) {
            (Op::Eq, None) => parts.push(format!("{col} IS NULL")),
            (Op::Ne, None) => parts.push(format!("{col} IS NOT NULL")),
            (Op::Like | Op::NotLike, None) => {
                return Err(StepError::input(format!(
                    "column {col:?}: a LIKE has nothing to match against <<null>>; \
                     write `{col}: <<null>>` or `{col}!: <<null>>`"
                )));
            }
            // A pattern: the COLUMN is rendered as text and the value binds
            // as text. An ordering operator must NOT be added here — under
            // `cast_text` a comparison is lexicographic, and `amount>=: 9`
            // would answer `'9' >= '10'`. It belongs in the arm below, which
            // binds the column's own type and lets the engine compare.
            (Op::Like | Op::NotLike, Some(_)) => {
                parts.push(format!(
                    "{} {} {}",
                    platform.cast_text(col),
                    op.sql(),
                    platform.bind(param, "text")
                ));
                binds.push(val.clone());
                param += 1;
            }
            (Op::Eq | Op::Ne, Some(_)) => {
                parts.push(format!(
                    "{col} {} {}",
                    op.sql(),
                    platform.bind(param, &c.type_name)
                ));
                binds.push(val.clone());
                param += 1;
            }
        }
    }
    Ok((parts.join(" AND "), binds))
}

pub fn build_update(
    platform: &dyn Platform,
    schema: &TableSchema,
    sql_name: &str,
    set: &[(String, Option<String>)],
    where_: &[(String, Option<String>)],
) -> Result<(String, Vec<Option<String>>), StepError> {
    if where_.is_empty() {
        return Err(StepError::input(
            "UPDATE without WHERE is forbidden; give a condition for a bulk change",
        ));
    }
    let mut sets = Vec::new();
    let mut binds = Vec::new();
    let mut param = 1usize;
    for (col, val) in set {
        plain_column(col).map_err(StepError::input)?;
        let c = schema
            .col(col)
            .ok_or_else(|| db_read(format!("column {col:?} is missing from the table")))?;
        platform.check_bindable(c).map_err(StepError::input)?;
        sets.push(format!("{col} = {}", platform.bind(param, &c.type_name)));
        binds.push(val.clone());
        param += 1;
    }
    let (where_sql, where_binds) = build_where(platform, schema, where_, param)?;
    binds.extend(where_binds);
    Ok((
        format!(
            "UPDATE {sql_name} SET {} WHERE {where_sql}",
            sets.join(", ")
        ),
        binds,
    ))
}

pub fn build_delete(
    platform: &dyn Platform,
    schema: &TableSchema,
    sql_name: &str,
    where_: &[(String, Option<String>)],
) -> Result<(String, Vec<Option<String>>), StepError> {
    if where_.is_empty() {
        return Err(StepError::input(
            "DELETE without WHERE is forbidden; use the \"I delete all\" step for a full wipe",
        ));
    }
    let (where_sql, binds) = build_where(platform, schema, where_, 1)?;
    Ok((format!("DELETE FROM {sql_name} WHERE {where_sql}"), binds))
}

pub fn build_delete_all(sql_name: &str) -> String {
    format!("DELETE FROM {sql_name}")
}

pub fn build_exists(
    platform: &dyn Platform,
    schema: &TableSchema,
    sql_name: &str,
    where_: &[(String, Option<String>)],
) -> Result<(String, Vec<Option<String>>), StepError> {
    if where_.is_empty() {
        return Err(StepError::input("an existence check requires a condition"));
    }
    let (where_sql, binds) = build_where(platform, schema, where_, 1)?;
    Ok((
        format!("SELECT 1 FROM {sql_name} WHERE {where_sql} LIMIT 1"),
        binds,
    ))
}

#[cfg(test)]
mod tests;
