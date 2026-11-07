//! The 14 steps: what the host is told about each, and what each one does.
//! The index of a step in `ALL` is its identity, the `u32` the host passes
//! back, so the list, the specs and the dispatch live in this one file.

use serde_json::{Value, json};

use crate::error::StepError;
use crate::ops::{self, Ctx};
use crate::sql::{plan, value};

#[derive(Clone, Copy)]
pub enum Step {
    HaveWhere,
    HaveWith,
    HaveMulti,
    Update,
    DeleteAll,
    DeleteWhere,
    ExtractFromDb,
    ShouldNotHaveTable,
    ShouldNotHaveWith,
    ShouldHaveTable,
    ShouldHaveWith,
    CallProcedure,
    CallFunction,
    GetSequence,
}

/// In the order the host indexes them.
const ALL: [Step; 14] = [
    Step::HaveWhere,
    Step::HaveWith,
    Step::HaveMulti,
    Step::Update,
    Step::DeleteAll,
    Step::DeleteWhere,
    Step::ExtractFromDb,
    Step::ShouldNotHaveTable,
    Step::ShouldNotHaveWith,
    Step::ShouldHaveTable,
    Step::ShouldHaveWith,
    Step::CallProcedure,
    Step::CallFunction,
    Step::GetSequence,
];

struct Spec {
    expression: &'static str,
    assertion: bool,
    description: &'static str,
    /// The variables the step sets. Always declared, even when empty: a step
    /// that declares no effect is unknown to `doctor`.
    sets_vars: &'static [&'static str],
}

/// How an argument is read, for the checks that need no connection.
#[derive(Clone, Copy)]
enum Arg {
    /// `col: value` pairs a row is written with: no operator on a column.
    Written,
    /// `col: value` pairs a row is matched by: an operator may end a column.
    Condition,
    /// A column read by name: no operator.
    Column,
    /// Positional arguments of a call.
    Call,
}

impl Step {
    pub fn from_index(index: u32) -> Option<Self> {
        ALL.get(usize::try_from(index).ok()?).copied()
    }

    fn spec(self) -> Spec {
        let (expression, assertion, description, sets_vars): (_, _, _, &'static [&'static str]) =
            match self {
                Self::HaveWhere => (
                    r#"I have "{table}" where:"#,
                    false,
                    "inserts one row per line of the table below, its header naming the columns",
                    &["last_insert_*"],
                ),
                Self::HaveWith => (
                    r#"I have "{table}" with "{pairs}""#,
                    false,
                    "inserts one row, filling required columns the pairs omit from the table schema",
                    &["last_insert_*"],
                ),
                Self::HaveMulti => (
                    "I have:",
                    false,
                    "inserts rows into several tables, the first column of the table below naming each one",
                    &["last_insert_*"],
                ),
                Self::Update => (
                    r#"I update "{table}" with "{pairs}" where "{condition}""#,
                    false,
                    "updates matching rows and stores the affected count in updated_<table>",
                    &["updated_*"],
                ),
                Self::DeleteAll => (
                    r#"I delete all "{table}""#,
                    false,
                    "wipes the whole table — no WHERE, and no awareness of files running in parallel",
                    &["deleted_*"],
                ),
                Self::DeleteWhere => (
                    r#"I delete "{table}" where "{condition}""#,
                    false,
                    "deletes matching rows and stores the affected count in deleted_<table>",
                    &["deleted_*"],
                ),
                Self::ExtractFromDb => (
                    r#"I extract "{column}" from "{table}" with "{condition}" as "{name}""#,
                    false,
                    "reads a column of the first matching row into a variable",
                    &["{name}"],
                ),
                Self::ShouldNotHaveTable => (
                    r#"I should not have "{table}" with:"#,
                    true,
                    "asserts no row matches every line of the table below at once",
                    &[],
                ),
                Self::ShouldNotHaveWith => (
                    r#"I should not have "{table}" with "{pairs}""#,
                    true,
                    "asserts no row matches these column values",
                    &[],
                ),
                Self::ShouldHaveTable => (
                    r#"I should have "{table}" with:"#,
                    true,
                    "asserts a row matches every line of the table below",
                    &[],
                ),
                Self::ShouldHaveWith => (
                    r#"I should have "{table}" with "{pairs}""#,
                    true,
                    "asserts at least one row matches these column values",
                    &[],
                ),
                Self::CallProcedure => (
                    r#"I call procedure "{name}" with "{arguments}""#,
                    false,
                    "calls a stored procedure with these arguments",
                    &[],
                ),
                Self::CallFunction => (
                    r#"I call function "{name}" with "{arguments}" as "{variable}""#,
                    false,
                    "calls a function and stores its return value in a variable",
                    &["{variable}"],
                ),
                Self::GetSequence => (
                    r#"I get next value of sequence "{name}" as "{variable}""#,
                    false,
                    "advances a sequence and stores the new value in a variable",
                    &["{variable}"],
                ),
            };
        Spec {
            expression,
            assertion,
            description,
            sets_vars,
        }
    }

    /// The arguments `validate` can check, by position.
    fn checked(self) -> &'static [(usize, Arg)] {
        match self {
            Self::HaveWith => &[(1, Arg::Written)],
            Self::Update => &[(1, Arg::Written), (2, Arg::Condition)],
            Self::DeleteWhere | Self::ShouldHaveWith | Self::ShouldNotHaveWith => {
                &[(1, Arg::Condition)]
            }
            Self::ExtractFromDb => &[(0, Arg::Column), (2, Arg::Condition)],
            Self::CallProcedure | Self::CallFunction => &[(1, Arg::Call)],
            Self::HaveWhere
            | Self::HaveMulti
            | Self::DeleteAll
            | Self::ShouldNotHaveTable
            | Self::ShouldHaveTable
            | Self::GetSequence => &[],
        }
    }
}

/// The `bddkit_list_steps` reply.
pub fn list_json() -> String {
    let steps: Vec<Value> = ALL
        .iter()
        .map(|step| {
            let spec = step.spec();
            json!({
                "expression": spec.expression,
                "group": "db",
                "kind": if spec.assertion { "assertion" } else { "action" },
                "description": spec.description,
                "sets_vars": spec.sets_vars,
            })
        })
        .collect();
    Value::Array(steps).to_string()
}

/// Refuses what fails at the step for sure, and nothing else: an argument
/// that holds a `<<slot>>` anywhere is not read, since its value is unknown
/// here and a macro body is checked whether or not a scenario calls it.
pub fn validate(step: Step, args: &[String]) -> Result<(), String> {
    for &(index, kind) in step.checked() {
        let Some(text) = args.get(index).filter(|t| !t.contains("<<")) else {
            continue;
        };
        match kind {
            Arg::Column => plan::plain_column(text)?,
            Arg::Written => {
                for (column, _) in value::parse_oneliner(text)? {
                    plan::plain_column(&column)?;
                }
            }
            Arg::Condition => {
                for (column, _) in value::parse_oneliner(text)? {
                    plan::condition_column(&column)?;
                }
            }
            Arg::Call => {
                value::parse_args(text)?;
            }
        }
    }
    Ok(())
}

/// Why a step did not pass. An assertion that does not hold yet may be asked
/// again; everything else is final.
pub struct Failed {
    pub error: StepError,
    pub not_yet: bool,
}

impl From<StepError> for Failed {
    fn from(error: StepError) -> Self {
        Self {
            error,
            not_yet: false,
        }
    }
}

type Rows = Option<Vec<Vec<String>>>;

fn arg(args: &[String], index: usize) -> &str {
    args.get(index).map_or("", String::as_str)
}

fn rows(table: &Rows) -> Result<&[Vec<String>], StepError> {
    table
        .as_deref()
        .ok_or_else(|| StepError::input("step requires a table"))
}

/// Presence of a row, asked afresh on every attempt. A mismatch may change
/// by the next attempt; a statement that failed will not.
async fn presence(
    ctx: &mut Ctx<'_>,
    table: &str,
    pairs: &[(String, Option<String>)],
    negate: bool,
) -> Result<(), Failed> {
    let found = ops::exists(ctx, table, pairs).await?;
    let message = match (found, negate) {
        (true, false) | (false, true) => return Ok(()),
        (false, false) => format!("expected a row in {table}, but there is none"),
        (true, true) => format!("a row in {table} exists, but must not"),
    };
    Err(Failed {
        error: StepError::assertion(message),
        not_yet: true,
    })
}

pub async fn run(
    step: Step,
    ctx: &mut Ctx<'_>,
    args: &[String],
    table: &Rows,
) -> Result<(), Failed> {
    let a = |i| arg(args, i);
    match step {
        Step::HaveWith => {
            let values = value::parse_oneliner(a(1)).map_err(StepError::input)?;
            ops::insert(ctx, a(0), &values, None).await?;
        }
        Step::HaveWhere => {
            let sets = value::pairs_from_wide(rows(table)?).map_err(StepError::input)?;
            for (i, values) in sets.iter().enumerate() {
                ops::insert(ctx, a(0), values, Some(i)).await?;
            }
        }
        Step::HaveMulti => {
            // The first row is the header (| table | record |), then (table, kv) pairs.
            for row in rows(table)?.iter().skip(1) {
                let name = row
                    .first()
                    .ok_or_else(|| StepError::input("empty table row"))?;
                let kv = row.get(1).map_or("", String::as_str);
                let values = value::parse_oneliner(kv).map_err(StepError::input)?;
                ops::insert(ctx, name, &values, None).await?;
            }
        }
        Step::Update => ops::update(ctx, a(0), a(1), a(2)).await?,
        Step::DeleteWhere => ops::delete(ctx, a(0), a(1)).await?,
        Step::DeleteAll => ops::delete_all(ctx, a(0)).await?,
        Step::ExtractFromDb => ops::extract(ctx, a(0), a(1), a(2), a(3)).await?,
        Step::ShouldHaveWith | Step::ShouldNotHaveWith => {
            let pairs = value::parse_oneliner(a(1)).map_err(StepError::input)?;
            presence(ctx, a(0), &pairs, matches!(step, Step::ShouldNotHaveWith)).await?;
        }
        Step::ShouldHaveTable | Step::ShouldNotHaveTable => {
            let pairs = value::pairs_from_tall(rows(table)?).map_err(StepError::input)?;
            presence(ctx, a(0), &pairs, matches!(step, Step::ShouldNotHaveTable)).await?;
        }
        Step::CallProcedure => ops::call_procedure(ctx, a(0), a(1)).await?,
        Step::CallFunction => ops::call_function(ctx, a(0), a(1), a(2)).await?,
        Step::GetSequence => ops::next_sequence(ctx, a(0), a(1)).await?,
    }
    Ok(())
}
