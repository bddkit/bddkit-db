//! The one tokio runtime the plugin owns, process-wide. The pools live on it,
//! and a call from the host's blocking thread waits on it.

use std::future::Future;
use std::sync::LazyLock;

use tokio::runtime::{Builder, Runtime};

static RUNTIME: LazyLock<Result<Runtime, String>> = LazyLock::new(|| {
    Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("cannot start the async runtime: {e}"))
});

pub fn block_on<T>(future: impl Future<Output = Result<T, String>>) -> Result<T, String> {
    match &*RUNTIME {
        Ok(runtime) => runtime.block_on(future),
        Err(error) => Err(error.clone()),
    }
}
