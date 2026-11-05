use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

static GEN: AtomicU64 = AtomicU64::new(1);
static ACTIVE: AtomicU64 = AtomicU64::new(0);

/// Snapshot at the start of a chat turn and mark it as the active one.
pub fn begin() -> u64 {
    let epoch = GEN.load(Ordering::SeqCst);
    ACTIVE.store(epoch, Ordering::SeqCst);
    epoch
}

/// Snapshot at the start of a chat turn.
pub fn epoch() -> u64 {
    GEN.load(Ordering::SeqCst)
}

/// True when Stop was hit for the chat turn that is currently running.
pub fn active_cancelled() -> bool {
    let active = ACTIVE.load(Ordering::SeqCst);
    active != 0 && is_cancelled(active)
}

/// Invalidate the current turn (Stop button / new send).
pub fn cancel() {
    GEN.fetch_add(1, Ordering::SeqCst);
    crate::shell::interrupt_current();
}

pub fn is_cancelled(epoch: u64) -> bool {
    GEN.load(Ordering::SeqCst) != epoch
}

pub fn check(epoch: u64) -> anyhow::Result<()> {
    if is_cancelled(epoch) {
        Err(anyhow::anyhow!("cancelled"))
    } else {
        Ok(())
    }
}

pub async fn wait_cancelled(epoch: u64) {
    while !is_cancelled(epoch) {
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}
