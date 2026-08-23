//! Small, synchronous retry policy shared by HTTP-backed artifact sources.
//!
//! The policy is deliberately transport-agnostic: adapters provide one HTTP
//! attempt and a sleeper, while this module decides whether another attempt is
//! allowed. That keeps rate-limit behavior deterministic and unit-testable
//! without opening sockets or sleeping in tests.

use std::time::Duration;

use chrono::Utc;

const DEFAULT_MAX_RETRIES: usize = 3;
const DEFAULT_BASE_DELAY: Duration = Duration::from_secs(1);
const DEFAULT_MAX_DELAY: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryPolicy {
    max_retries: usize,
    base_delay: Duration,
    max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: DEFAULT_MAX_RETRIES,
            base_delay: DEFAULT_BASE_DELAY,
            max_delay: DEFAULT_MAX_DELAY,
        }
    }
}

impl RetryPolicy {
    #[must_use]
    pub const fn new(max_retries: usize, base_delay: Duration, max_delay: Duration) -> Self {
        Self {
            max_retries,
            base_delay,
            max_delay,
        }
    }

    fn should_retry(self, status: u16, retries_used: usize) -> bool {
        retries_used < self.max_retries && matches!(status, 429 | 502 | 503 | 504)
    }

    fn delay(self, retries_used: usize, retry_after: Option<&str>) -> Duration {
        retry_after
            .and_then(retry_after_delay)
            .unwrap_or_else(|| {
                let exponent = u32::try_from(retries_used).unwrap_or(u32::MAX);
                self.base_delay
                    .checked_mul(2u32.saturating_pow(exponent))
                    .unwrap_or(self.max_delay)
            })
            .min(self.max_delay)
    }
}

/// `Retry-After` is either a delay in seconds or an HTTP-date. Negative dates
/// are treated as immediately retryable and still consume the retry budget.
fn retry_after_delay(value: &str) -> Option<Duration> {
    if let Ok(seconds) = value.trim().parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let at = chrono::DateTime::parse_from_rfc2822(value.trim()).ok()?;
    Some(
        at.signed_duration_since(Utc::now())
            .to_std()
            .unwrap_or(Duration::ZERO),
    )
}

pub trait Sleeper {
    fn sleep(&self, duration: Duration);
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ThreadSleeper;

impl Sleeper for ThreadSleeper {
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

pub struct Attempt<T> {
    pub status: u16,
    pub retry_after: Option<String>,
    pub value: T,
}

#[derive(Debug, Eq, PartialEq)]
pub enum RetryError<E> {
    Request(E),
    Status { status: u16, attempts: usize },
}

/// Executes `attempt` until it succeeds, returns a non-retryable status, or
/// exhausts `policy`'s finite retry budget.
///
/// # Errors
///
/// Returns [`RetryError::Request`] for a transport failure and
/// [`RetryError::Status`] for a final non-success HTTP status.
pub fn run<T, E>(
    policy: RetryPolicy,
    sleeper: &impl Sleeper,
    mut attempt: impl FnMut() -> Result<Attempt<T>, E>,
) -> Result<T, RetryError<E>> {
    let mut retries_used = 0;
    loop {
        let response = attempt().map_err(RetryError::Request)?;
        if (200..300).contains(&response.status) {
            return Ok(response.value);
        }
        if !policy.should_retry(response.status, retries_used) {
            return Err(RetryError::Status {
                status: response.status,
                attempts: retries_used + 1,
            });
        }
        sleeper.sleep(policy.delay(retries_used, response.retry_after.as_deref()));
        retries_used += 1;
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, collections::VecDeque};

    use super::*;

    #[derive(Default)]
    struct RecordingSleeper {
        delays: RefCell<Vec<Duration>>,
    }

    impl Sleeper for RecordingSleeper {
        fn sleep(&self, duration: Duration) {
            self.delays.borrow_mut().push(duration);
        }
    }

    #[test]
    fn retry_after_is_respected_and_retries_are_bounded() {
        let sleeper = RecordingSleeper::default();
        let mut responses = VecDeque::from([
            Attempt {
                status: 429,
                retry_after: Some("7".to_owned()),
                value: "limited",
            },
            Attempt {
                status: 429,
                retry_after: None,
                value: "limited again",
            },
            Attempt {
                status: 200,
                retry_after: None,
                value: "ok",
            },
        ]);

        let value = run(
            RetryPolicy::new(2, Duration::from_secs(1), Duration::from_secs(30)),
            &sleeper,
            || Ok::<_, ()>(responses.pop_front().expect("fixture response")),
        )
        .expect("eventual success");

        assert_eq!(value, "ok");
        assert_eq!(
            *sleeper.delays.borrow(),
            [Duration::from_secs(7), Duration::from_secs(2)]
        );
    }

    #[test]
    fn a_rate_limit_after_the_retry_budget_is_an_explicit_error() {
        let sleeper = RecordingSleeper::default();
        let error = run(
            RetryPolicy::new(1, Duration::ZERO, Duration::ZERO),
            &sleeper,
            || {
                Ok::<_, ()>(Attempt {
                    status: 429,
                    retry_after: None,
                    value: (),
                })
            },
        )
        .expect_err("retry budget must be finite");

        assert_eq!(
            error,
            RetryError::Status {
                status: 429,
                attempts: 2
            }
        );
    }

    #[test]
    fn an_http_date_retry_after_is_understood_and_bounded() {
        let policy = RetryPolicy::new(1, Duration::from_secs(1), Duration::from_secs(30));

        assert_eq!(
            policy.delay(0, Some("Wed, 21 Oct 2099 07:28:00 GMT")),
            Duration::from_secs(30)
        );
    }
}
