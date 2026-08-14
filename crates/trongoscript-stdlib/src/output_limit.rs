//! The ceiling on what a single builtin call may allocate.
//!
//! Some builtins build an output whose size is chosen by the caller —
//! `repeat`, `pad-left`, `pad-right` — so a few bytes of source can ask
//! the allocator for terabytes. The evaluator's allocation budget is
//! charged *after* a builtin returns, which is too late: the process is
//! already dead. The size therefore has to be rejected inside the
//! builtin, before anything is allocated.
//!
//! That check needs a number, and the builtin signature
//! (`fn(&[Value]) -> BuiltinResult`) carries no evaluator context to get
//! one from. It used to be a hardcoded 16 MiB, which meant `--max-alloc
//! 1000` did not stop a 16 MiB allocation inside a single call: the
//! backstop against crashing was also, in effect, the allocation policy.
//!
//! The evaluator now publishes its budget here with [`with_limit`] for
//! the duration of a builtin call, and the builtins read it with
//! [`limit`]. The value is thread-local because a `Context` evaluates on
//! one thread; two embedders running concurrently each see their own.
//! Outside any [`with_limit`] scope — a host calling a stdlib function
//! directly, or a unit test — [`DEFAULT_MAX_OUT`] applies, so the
//! backstop is never simply absent.

use std::cell::Cell;

/// Ceiling applied when no evaluator has published one.
///
/// Large enough not to interfere with legitimate string building, small
/// enough that a single call cannot exhaust a normal process.
pub const DEFAULT_MAX_OUT: usize = 16 * 1024 * 1024;

thread_local! {
    static MAX_OUT: Cell<usize> = const { Cell::new(DEFAULT_MAX_OUT) };
}

/// The largest output a single builtin call may construct, in bytes.
pub fn limit() -> usize {
    MAX_OUT.with(|c| c.get())
}

/// Run `f` with the single-call output ceiling set to `bytes`.
///
/// The previous value is restored afterwards, including when `f` panics
/// or returns early, so a nested call cannot leak its ceiling to the
/// caller.
pub fn with_limit<T>(bytes: usize, f: impl FnOnce() -> T) -> T {
    struct Restore(usize);
    impl Drop for Restore {
        fn drop(&mut self) {
            MAX_OUT.with(|c| c.set(self.0));
        }
    }

    let _restore = Restore(MAX_OUT.with(|c| c.replace(bytes)));
    f()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_applies_outside_any_scope() {
        assert_eq!(limit(), DEFAULT_MAX_OUT);
    }

    #[test]
    fn scope_sets_and_restores() {
        assert_eq!(limit(), DEFAULT_MAX_OUT);
        with_limit(1024, || assert_eq!(limit(), 1024));
        assert_eq!(limit(), DEFAULT_MAX_OUT);
    }

    #[test]
    fn scopes_nest() {
        with_limit(4096, || {
            assert_eq!(limit(), 4096);
            with_limit(16, || assert_eq!(limit(), 16));
            assert_eq!(limit(), 4096);
        });
        assert_eq!(limit(), DEFAULT_MAX_OUT);
    }

    #[test]
    fn scope_restores_after_a_panic() {
        let r = std::panic::catch_unwind(|| {
            with_limit(64, || panic!("boom"));
        });
        assert!(r.is_err());
        assert_eq!(limit(), DEFAULT_MAX_OUT);
    }
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
