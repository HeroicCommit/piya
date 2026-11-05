use std::future::Future;
use std::sync::{mpsc, OnceLock};

use tokio::runtime::{Handle, RuntimeFlavor};

static HANDLE: OnceLock<Handle> = OnceLock::new();

fn global_handle() -> Handle {
    HANDLE
        .get_or_init(|| {
            let (tx, rx) = mpsc::sync_channel(1);
            std::thread::Builder::new()
                .name("piya-runtime".into())
                .spawn(move || {
                    let rt = tokio::runtime::Builder::new_multi_thread()
                        .enable_all()
                        .thread_name("piya-async")
                        .build()
                        .expect("tokio runtime");
                    let _ = tx.send(rt.handle().clone());
                    rt.block_on(std::future::pending::<()>());
                })
                .expect("spawn piya-runtime thread");
            rx.recv().expect("piya runtime handle")
        })
        .clone()
}

/// Run async code from a sync UI thread (menu bar, chat panel, FFI).
///
/// Reuses one process-long Tokio runtime. Nested calls from a multi-thread
/// worker use `block_in_place` + `Handle::block_on` — never a second runtime.
pub fn block_on<F, T>(future: F) -> T
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    if let Ok(handle) = Handle::try_current() {
        if handle.runtime_flavor() == RuntimeFlavor::MultiThread {
            return tokio::task::block_in_place(|| handle.block_on(future));
        }
    }

    let handle = global_handle();
    let (tx, rx) = mpsc::sync_channel(1);
    handle.spawn(async move {
        let _ = tx.send(future.await);
    });
    rx.recv().expect("piya async worker panicked")
}
