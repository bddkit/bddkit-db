//! Every reply the plugin sends is built here, so the shape the host parses
//! has one author.

use serde_json::json;

/// The envelope shared by validate / probe / init / drop.
pub fn ok() -> String {
    r#"{"ok":true}"#.to_string()
}

/// An `ok` reply with the one-line `detail` the host shows in `doctor`.
pub fn ok_detail(detail: &str) -> String {
    json!({"ok": true, "detail": detail}).to_string()
}

pub fn err(error: impl Into<String>) -> String {
    json!({"ok": false, "error": error.into()}).to_string()
}

/// A refusal that names the instance, which the host reports as
/// `resources.<group>.<instance>: <error>`.
pub fn err_in(instance: &str, error: impl Into<String>) -> String {
    json!({"ok": false, "instance": instance, "error": error.into()}).to_string()
}

pub fn handle(handle: u64) -> String {
    json!({"ok": true, "handle": handle}).to_string()
}

/// Retrying cannot help.
pub fn fatal(error: &str) -> String {
    json!({"status": "fatal", "error": error}).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn a_refusal_names_its_instance() {
        let v: Value = serde_json::from_str(&err_in("audit", "boom")).expect("JSON");
        assert_eq!(v["ok"], false);
        assert_eq!(v["instance"], "audit");
        assert_eq!(v["error"], "boom");
    }
}
