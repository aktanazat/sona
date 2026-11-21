//! One download state per model id.
//!
//! A cancelled download keeps its entry, as `Cancelling`, until its own task
//! has unwound, and a new start for that id waits for it. Only the claim that
//! owns an entry, named by its generation, removes it, so a late unwind can
//! never take a newer download's cancel token with it.

use super::lock_model_state;
use hf_hub::api::tokio::CancellationToken;
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Running,
    Cancelling,
}

struct Entry {
    generation: u64,
    phase: Phase,
    cancel: CancellationToken,
    ended: CancellationToken,
    /// Cancels the starts waiting behind this entry so far; a cancel swaps
    /// in a fresh one for the starts that come after it.
    queued: CancellationToken,
}

enum Begin {
    Started {
        generation: u64,
        cancel: CancellationToken,
    },
    Running,
    Cancelling {
        ended: CancellationToken,
        queued: CancellationToken,
    },
}

#[derive(Default)]
pub(super) struct Downloads {
    next_generation: u64,
    entries: HashMap<String, Entry>,
}

impl Downloads {
    fn begin(&mut self, model_id: &str) -> Begin {
        if let Some(entry) = self.entries.get(model_id) {
            return match entry.phase {
                Phase::Running => Begin::Running,
                Phase::Cancelling => Begin::Cancelling {
                    ended: entry.ended.clone(),
                    queued: entry.queued.clone(),
                },
            };
        }
        self.next_generation += 1;
        let cancel = CancellationToken::new();
        self.entries.insert(
            model_id.to_string(),
            Entry {
                generation: self.next_generation,
                phase: Phase::Running,
                cancel: cancel.clone(),
                ended: CancellationToken::new(),
                queued: CancellationToken::new(),
            },
        );
        Begin::Started {
            generation: self.next_generation,
            cancel,
        }
    }

    fn finish(&mut self, model_id: &str, generation: u64) {
        if self
            .entries
            .get(model_id)
            .is_some_and(|entry| entry.generation == generation)
        {
            if let Some(entry) = self.entries.remove(model_id) {
                entry.ended.cancel();
            }
        }
    }

    /// Cancels a running download, or the starts waiting for a cancelled
    /// one to unwind. Returns whether a download for `model_id` was in flight.
    pub(super) fn cancel(&mut self, model_id: &str) -> bool {
        let Some(entry) = self.entries.get_mut(model_id) else {
            return false;
        };
        match entry.phase {
            Phase::Running => {
                entry.cancel.cancel();
                entry.phase = Phase::Cancelling;
                true
            }
            Phase::Cancelling => {
                std::mem::replace(&mut entry.queued, CancellationToken::new()).cancel();
                true
            }
        }
    }

    /// Whether `model_id` is downloading and not cancelled: the row's
    /// "downloading" state.
    pub(super) fn is_running(&self, model_id: &str) -> bool {
        self.entries
            .get(model_id)
            .is_some_and(|entry| entry.phase == Phase::Running)
    }

    /// Whether a task, running or still unwinding, may write `model_id`'s files.
    pub(super) fn is_active(&self, model_id: &str) -> bool {
        self.entries.contains_key(model_id)
    }

    pub(super) fn active_ids(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }
}

/// Ownership of one model's download, released when dropped.
pub(super) struct DownloadClaim<'a> {
    downloads: &'a Mutex<Downloads>,
    model_id: String,
    generation: u64,
    pub(super) cancel: CancellationToken,
}

impl Drop for DownloadClaim<'_> {
    fn drop(&mut self) {
        lock_model_state(self.downloads).finish(&self.model_id, self.generation);
    }
}

/// Claims `model_id`'s download. `None` when one is already running, which
/// the caller joins, or when the start was cancelled while it waited for a
/// cancelled download to unwind.
pub(super) async fn claim<'a>(
    downloads: &'a Mutex<Downloads>,
    model_id: &str,
) -> Option<DownloadClaim<'a>> {
    loop {
        let (ended, queued) = match lock_model_state(downloads).begin(model_id) {
            Begin::Started { generation, cancel } => {
                return Some(DownloadClaim {
                    downloads,
                    model_id: model_id.to_string(),
                    generation,
                    cancel,
                })
            }
            Begin::Running => return None,
            Begin::Cancelling { ended, queued } => (ended, queued),
        };
        ended.cancelled().await;
        if queued.is_cancelled() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn download_again_after_cancel_stays_cancellable_once_the_old_one_unwinds() {
        let downloads = Mutex::new(Downloads::default());
        let first = claim(&downloads, "m").await.expect("first download starts");
        assert!(lock_model_state(&downloads).cancel("m"));

        let (second, ()) = tokio::join!(claim(&downloads, "m"), async { drop(first) });
        let second = second.expect("second download starts once the first has unwound");

        assert!(lock_model_state(&downloads).is_running("m"));
        assert!(lock_model_state(&downloads).cancel("m"));
        assert!(second.cancel.is_cancelled());
    }

    #[tokio::test]
    async fn a_cancel_while_the_next_download_waits_keeps_it_from_starting() {
        let downloads = Mutex::new(Downloads::default());
        let first = claim(&downloads, "m").await.expect("first download starts");
        lock_model_state(&downloads).cancel("m");

        let (second, ()) = tokio::join!(claim(&downloads, "m"), async {
            lock_model_state(&downloads).cancel("m");
            drop(first);
        });

        assert!(second.is_none());
        assert!(!lock_model_state(&downloads).is_active("m"));
    }
}
