//! bddkit plugin serving the `db` resource group: Postgres, MySQL and
//! MariaDB connections. Written against docs/plugin-authoring.md; it must
//! never need the host's source.

pub mod config;
pub mod db;
pub mod engine;
pub mod error;
mod reply;
mod runtime;
pub mod sql;

use std::collections::{BTreeMap, HashMap};
use std::ffi::{CStr, CString, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};

use config::Connection;
use db::Db;
use serde_json::Value;

/// The plugin ABI this library is built for.
const ABI: u32 = 4;

/// Outside `guard`, and safe there only because it provably cannot panic:
/// `CString::new` returns a `Result` and the fallback is a C string literal.
/// Anything added here must keep that property.
fn out(s: String) -> *mut c_char {
    CString::new(s)
        .unwrap_or_else(|_| c"{\"ok\":false,\"error\":\"NUL in reply\"}".to_owned())
        .into_raw()
}

/// Inside `guard` at every call site.
fn input(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: `p` is not null. The host builds a request from a `CString` and
    // keeps it valid for the duration of the call, which encloses this one;
    // it is copied before returning.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// A panic must never unwind across the FFI boundary. Every export is
/// guarded, including the trivial ones, so "every export is guarded" is an
/// invariant a reader checks in one pass.
fn guard(envelope_kind: &str, body: impl FnOnce() -> String) -> *mut c_char {
    match catch_unwind(AssertUnwindSafe(body)) {
        Ok(reply) => out(reply),
        Err(_) => out(match envelope_kind {
            "dispatch" => r#"{"status":"fatal","error":"the plugin panicked"}"#.to_string(),
            _ => r#"{"ok":false,"error":"the plugin panicked"}"#.to_string(),
        }),
    }
}

pub fn manifest_json() -> String {
    serde_json::json!({
        "name": "db",
        "version": env!("CARGO_PKG_VERSION"),
        "groups": ["db"],
        // Nothing changes after the pools are open, so one handle serves
        // every worker.
        "concurrency": "shared",
        // A step may name any connection in its table reference, so one
        // handle holds all of them.
        "handle_scope": "group",
        // An unreachable database stops the run before any scenario.
        "eager_init": ["db"],
        // `I use "<name>" connection` was the built-in's word for the switch.
        "deprecated": { "connection": "db" },
        "fields": { "db": config::fields_json() },
    })
    .to_string()
}

#[unsafe(no_mangle)]
pub extern "C" fn bddkit_abi_version() -> u32 {
    ABI
}

#[unsafe(no_mangle)]
pub extern "C" fn bddkit_manifest() -> *mut c_char {
    guard("envelope", manifest_json)
}

/// The steps arrive with their own issue; the group still loads, validates
/// and connects without them.
#[unsafe(no_mangle)]
pub extern "C" fn bddkit_list_steps() -> *mut c_char {
    guard("envelope", || "[]".to_string())
}

/// # Safety
/// `s` must be a pointer this library returned and has not yet freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bddkit_free_string(s: *mut c_char) {
    if !s.is_null() {
        // SAFETY: by this function's contract `s` came from `out`'s
        // `CString::into_raw` in this library and has not been freed yet.
        drop(unsafe { CString::from_raw(s) });
    }
}

fn parse_config(request: *const c_char) -> Result<Connection, String> {
    let value: Value = serde_json::from_str(&input(request)).map_err(|e| e.to_string())?;
    Connection::parse(&value["config"])
}

/// Eager, at startup, for every declared connection, touching nothing.
#[unsafe(no_mangle)]
pub extern "C" fn bddkit_validate_config(request: *const c_char) -> *mut c_char {
    guard("envelope", move || {
        match parse_config(request).and_then(|c| engine::check(&c)) {
            Ok(family) => reply::ok_detail(family.name()),
            Err(error) => reply::err(error),
        }
    })
}

/// `doctor --live`: a full connect of that one connection.
#[unsafe(no_mangle)]
pub extern "C" fn bddkit_probe_config(request: *const c_char) -> *mut c_char {
    guard("envelope", move || {
        let connection = match parse_config(request) {
            Ok(c) => c,
            Err(error) => return reply::err(error),
        };
        match runtime::block_on(db::probe(&connection)) {
            Ok(engine) => reply::ok_detail(&format!("connected, {}", engine.name())),
            Err(error) => reply::err(error),
        }
    })
}

/// A handle is an index into this table, never a pointer.
static DATABASES: LazyLock<Mutex<HashMap<u64, Arc<Db>>>> = LazyLock::new(Mutex::default);
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

/// Nothing can panic under this lock (a lookup, an insert, a remove), and the
/// table holds no half-updated state if it ever did.
fn databases() -> MutexGuard<'static, HashMap<u64, Arc<Db>>> {
    DATABASES.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Every declared connection of the run, at startup (`eager_init`).
fn init(request: &str) -> String {
    let value: Value = match serde_json::from_str(request) {
        Ok(v) => v,
        Err(e) => return reply::err(e.to_string()),
    };
    let max = value["concurrency"]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(1);
    let mut connections = BTreeMap::new();
    for (name, instance) in value["instances"].as_object().into_iter().flatten() {
        match Connection::parse(&instance["config"]) {
            Ok(connection) => {
                connections.insert(name.clone(), connection);
            }
            Err(error) => return reply::err_in(name, error),
        }
    }
    let connect = async { Ok(Db::connect(&connections, max).await) };
    match runtime::block_on(connect) {
        Ok(Ok(db)) => {
            let handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
            databases().insert(handle, Arc::new(db));
            reply::handle(handle)
        }
        Ok(Err(error)) => reply::err_in(&error.connection, error.message),
        Err(error) => reply::err(error),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn bddkit_init_instance(request: *const c_char) -> *mut c_char {
    guard("envelope", move || init(&input(request)))
}

#[unsafe(no_mangle)]
pub extern "C" fn bddkit_dispatch(
    _handle: u64,
    _step_index: u32,
    _request: *const c_char,
) -> *mut c_char {
    guard("dispatch", || {
        reply::fatal("the db plugin declares no steps yet")
    })
}

/// Closes the pools.
#[unsafe(no_mangle)]
pub extern "C" fn bddkit_drop_instance(handle: u64) -> *mut c_char {
    guard("envelope", move || {
        let removed = databases().remove(&handle);
        match removed {
            Some(db) => match runtime::block_on(async {
                db.close().await;
                Ok(())
            }) {
                Ok(()) => reply::ok(),
                Err(error) => reply::err(error),
            },
            None => reply::err("unknown handle"),
        }
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn bddkit_plugin_json_matches_the_library_and_the_manifest() {
        let declared: serde_json::Value =
            serde_json::from_str(include_str!("../bddkit-plugin.json")).expect("valid JSON");
        assert_eq!(declared["abi"], crate::bddkit_abi_version());
        let manifest: serde_json::Value =
            serde_json::from_str(&crate::manifest_json()).expect("JSON");
        assert_eq!(declared["groups"], manifest["groups"]);
    }

    #[test]
    fn the_manifest_claims_db_as_one_shared_group_scoped_eager_handle() {
        let v: serde_json::Value = serde_json::from_str(&crate::manifest_json()).expect("JSON");
        assert_eq!(v["name"], "db");
        assert_eq!(v["concurrency"], "shared");
        assert_eq!(v["handle_scope"], "group");
        assert_eq!(v["eager_init"], serde_json::json!(["db"]));
        assert_eq!(v["deprecated"], serde_json::json!({"connection": "db"}));
        assert!(v.get("implicit_instance").is_none());
        assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
    }
}
