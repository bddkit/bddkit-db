//! One `bddkit_dispatch`: the request in, the reply out.

use serde::Deserialize;

use crate::db::Db;
use crate::ops::Ctx;
use crate::reply;
use crate::steps::{self, Step};

/// What a step reads of the dispatch request.
#[derive(Deserialize)]
struct Request {
    #[serde(default)]
    args: Vec<String>,
    table: Option<Vec<Vec<String>>>,
    #[serde(default)]
    debug: bool,
    #[serde(default)]
    evidence: bool,
    /// The connection a bare table reference means.
    #[serde(default)]
    instance: String,
}

pub async fn run(db: &Db, step: Step, request: &str) -> String {
    let request: Request = match serde_json::from_str(request) {
        Ok(request) => request,
        Err(error) => return reply::fatal(&format!("malformed dispatch request: {error}")),
    };
    let mut ctx = Ctx::new(db, &request.instance, request.debug, request.evidence);
    let result = steps::run(step, &mut ctx, &request.args, &request.table).await;
    let vars = std::mem::take(&mut ctx.vars);
    let (output, diagnostics) = ctx.into_reply_parts();
    match result {
        Ok(()) => reply::passed(&vars, &output, &diagnostics),
        Err(failed) => reply::failed(failed.not_yet, &failed.error, &output, &diagnostics),
    }
}
