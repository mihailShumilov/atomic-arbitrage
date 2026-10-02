//! Exit codes of the `enricher` binary: the one place they are defined.
//!
//! The contract with systemd (`deploy/enricher-gaps.service`,
//! `SuccessExitStatus=75`) and the operator docs
//! (`references/data-model.md`, "Коды выхода enricher"; `deploy/README.md`).
//! The values never change; new outcomes get new names, not reused numbers.

/// The run finished: every planned file is committed.
pub const OK: u8 = 0;
/// Any other error: configuration, RPC failure after all retries, wrong
/// chain id, IO. systemd notifies via `OnFailure=`.
pub const FAILED: u8 = 1;
/// `--max-calls` is used up (`EX_TEMPFAIL`, task 012): finished files are in
/// `filled.tsv`, the unfinished `*.partial` is removed, the next run goes on.
pub const BUDGET_EXHAUSTED: u8 = 75;
/// Stopped by SIGINT/SIGTERM; the unfinished file is discarded.
pub const INTERRUPTED: u8 = 130;

/// How a run ended, as seen by `main`.
#[derive(Debug)]
pub enum Outcome {
    /// `run` returned `Ok`.
    Done,
    /// A signal arrived first (its name).
    Interrupted(&'static str),
    /// `run` returned an error.
    Failed(anyhow::Error),
}

/// Exit code for `outcome`. Pure: writing `--stats-json` or any other
/// reporting cannot change it (task 020 item 5).
pub fn code(outcome: &Outcome) -> u8 {
    match outcome {
        Outcome::Done => OK,
        Outcome::Interrupted(_) => INTERRUPTED,
        Outcome::Failed(e) if crate::rpc::is_budget_exhausted(e) => BUDGET_EXHAUSTED,
        Outcome::Failed(_) => FAILED,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::{BudgetExhausted, CallError};

    #[test]
    fn codes_by_outcome() {
        assert_eq!(code(&Outcome::Done), 0);
        assert_eq!(code(&Outcome::Interrupted("SIGTERM")), 130);
        assert_eq!(code(&Outcome::Failed(anyhow::anyhow!("boom"))), 1);
        let b = CallError::Budget(BudgetExhausted { what: "x".into(), sent: 1, next: 2, max: 2 });
        let e = anyhow::Error::new(b).context("blocks 1..=2");
        assert_eq!(code(&Outcome::Failed(e)), 75, "found anywhere in the chain");
    }
}
