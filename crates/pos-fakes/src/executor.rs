// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Twenty lines instead of an async runtime.
//!
//! `pos-contract-tests` deliberately depends on no executor: each adapter invoking a suite supplies
//! its own `block_on`, so `store-sqlite` uses tokio's and this crate uses [`run_ready`]. That is why
//! neither crate drags a runtime into the graph of anything that only wants the fakes.
//!
//! # A fake that yields is a bug, not a wait
//!
//! [`run_ready`] polls a future exactly once. Every fake here completes on the first poll, because
//! there is nothing to wait for — no socket, no file, no lock held across an await. So `Pending` is
//! not a slow fake, it is a fake that has grown a suspension point, and returning `None` rather
//! than spinning surfaces that immediately.
//!
//! This is also the mechanism behind "the domain suite runs in milliseconds": not that the fakes are
//! fast, but that they never suspend, so there is no scheduler in the loop at all.

use core::future::Future;
use core::pin::pin;
use core::task::{Context, Poll, Waker};

/// Drives `future` for exactly one poll.
///
/// Returns `None` if it suspended, which for a fake means it should not have.
pub fn poll_once<F: Future>(future: F) -> Option<F::Output> {
    let mut future = pin!(future);
    // `Waker::noop` is sound here precisely because nothing can suspend: a waker that never wakes
    // is only a problem for a future that expects to be woken.
    let mut context = Context::from_waker(Waker::noop());
    match future.as_mut().poll(&mut context) {
        Poll::Ready(output) => Some(output),
        Poll::Pending => None,
    }
}

/// Drives `future` to completion, or reports that it suspended.
///
/// The `block_on` the suite macros take. Named `run_ready` rather than `block_on` because it does
/// not block: if the future is not ready, there is nothing to wait for and waiting would hang a
/// test run instead of failing it.
///
/// # Panics
///
/// If the future suspends. That is a fake with a suspension point in it, and the message says so —
/// silently returning a default would make the case fail somewhere unrelated.
#[expect(
    clippy::panic,
    reason = "a suspended fake cannot produce a value, and there is nothing to wait for. Failing \
              here names the cause; returning a placeholder would fail the case three assertions \
              later with no hint as to why."
)]
pub fn run_ready<F: Future>(future: F) -> F::Output {
    match poll_once(future) {
        Some(output) => output,
        None => panic!(
            "a fake suspended. Nothing in pos-fakes awaits anything, so this means a suspension \
             point has been added — probably a real lock, channel, or timer. Either remove it or \
             give this crate a real executor."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{poll_once, run_ready};
    use core::future::{pending, ready};

    #[test]
    fn a_ready_future_completes_in_one_poll() {
        assert_eq!(run_ready(ready(7_u32)), 7);
        assert_eq!(poll_once(ready(7_u32)), Some(7));
    }

    #[test]
    fn a_pending_future_is_reported_rather_than_spun_on() {
        // The alternative — looping until ready — turns a fake with a suspension point into a test
        // run that hangs, which in CI means a ten-minute timeout and no diagnosis.
        assert_eq!(poll_once(pending::<u32>()), None);
    }

    #[test]
    fn an_async_block_with_several_awaits_still_completes_in_one_poll() {
        // Which is the property the fakes rely on: awaiting a ready future does not suspend, so a
        // chain of them is still one poll.
        let composed = async {
            let first = ready(1_u32).await;
            let second = ready(2_u32).await;
            first + second
        };
        assert_eq!(run_ready(composed), 3);
    }

    #[test]
    #[should_panic(expected = "a fake suspended")]
    fn run_ready_names_the_cause_when_a_fake_suspends() {
        let _unreachable: u32 = run_ready(pending::<u32>());
    }
}
