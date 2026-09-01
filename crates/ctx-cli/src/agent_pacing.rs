use std::{
    cell::RefCell,
    time::{Duration, Instant},
};

const MIN_REQUEST_INTERVAL: Duration = Duration::from_secs(30);
const WORK_WINDOW: Duration = Duration::from_secs(30 * 60);
const REST_WINDOW: Duration = Duration::from_secs(15 * 60);

pub(crate) trait Clock {
    fn now(&self) -> Duration;
}

pub(crate) trait Sleeper {
    fn sleep(&self, duration: Duration);
}

pub(crate) struct SystemClock {
    origin: Instant,
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }
}

pub(crate) struct ThreadSleeper;

impl Sleeper for ThreadSleeper {
    fn sleep(&self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

#[derive(Default)]
struct PacingState {
    work_started_at: Option<Duration>,
    last_request_started_at: Option<Duration>,
}

pub(crate) struct AgentPacer<C = SystemClock, S = ThreadSleeper> {
    clock: C,
    sleeper: S,
    state: RefCell<PacingState>,
}

impl AgentPacer {
    pub(crate) fn night_mode() -> Self {
        eprintln!(
            "Night mode enabled: at most one AI-agent request every 30s; after each 30m work window, requests pause for 15m."
        );
        Self {
            clock: SystemClock {
                origin: Instant::now(),
            },
            sleeper: ThreadSleeper,
            state: RefCell::new(PacingState::default()),
        }
    }
}

impl<C: Clock, S: Sleeper> AgentPacer<C, S> {
    fn before_request(&self) {
        loop {
            let now = self.clock.now();
            let mut state = self.state.borrow_mut();
            let work_started_at = *state.work_started_at.get_or_insert(now);
            if now.saturating_sub(work_started_at) >= WORK_WINDOW {
                eprintln!(
                    "Night mode: pausing AI-agent requests for 15m after the 30m work window."
                );
                drop(state);
                self.sleeper.sleep(REST_WINDOW);
                let mut state = self.state.borrow_mut();
                state.work_started_at = Some(self.clock.now());
                state.last_request_started_at = None;
                continue;
            }
            if let Some(last_started_at) = state.last_request_started_at {
                let elapsed = now.saturating_sub(last_started_at);
                if elapsed < MIN_REQUEST_INTERVAL {
                    let wait = MIN_REQUEST_INTERVAL
                        .checked_sub(elapsed)
                        .expect("elapsed was checked against the minimum interval");
                    eprintln!(
                        "Night mode: waiting {}s before the next AI-agent request (30s minimum interval).",
                        wait.as_secs()
                    );
                    drop(state);
                    self.sleeper.sleep(wait);
                    continue;
                }
            }
            state.last_request_started_at = Some(now);
            return;
        }
    }
}

impl AgentPacer {
    pub(crate) fn pace(&self) {
        self.before_request();
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::Cell, rc::Rc};

    use super::*;

    type TestPacer = AgentPacer<FakeClock, RecordingSleeper>;
    type SharedNow = Rc<Cell<Duration>>;
    type SharedWaits = Rc<RefCell<Vec<Duration>>>;

    #[derive(Clone)]
    struct FakeClock(Rc<Cell<Duration>>);

    impl Clock for FakeClock {
        fn now(&self) -> Duration {
            self.0.get()
        }
    }

    struct RecordingSleeper {
        now: Rc<Cell<Duration>>,
        waits: Rc<RefCell<Vec<Duration>>>,
    }

    impl Sleeper for RecordingSleeper {
        fn sleep(&self, duration: Duration) {
            self.waits.borrow_mut().push(duration);
            self.now.set(self.now.get() + duration);
        }
    }

    fn pacer() -> (TestPacer, SharedNow, SharedWaits) {
        let now = Rc::new(Cell::new(Duration::ZERO));
        let waits = Rc::new(RefCell::new(Vec::new()));
        (
            AgentPacer {
                clock: FakeClock(Rc::clone(&now)),
                sleeper: RecordingSleeper {
                    now: Rc::clone(&now),
                    waits: Rc::clone(&waits),
                },
                state: RefCell::new(PacingState::default()),
            },
            now,
            waits,
        )
    }

    #[test]
    fn first_request_is_immediate_and_the_next_waits_thirty_seconds() {
        let (pacer, _now, waits) = pacer();

        pacer.before_request();
        pacer.before_request();

        assert_eq!(*waits.borrow(), vec![MIN_REQUEST_INTERVAL]);
    }

    #[test]
    fn a_request_after_the_work_window_waits_for_the_full_rest_window() {
        let (pacer, now, waits) = pacer();
        pacer.before_request();
        now.set(WORK_WINDOW);

        pacer.before_request();

        assert_eq!(*waits.borrow(), vec![REST_WINDOW]);
        assert_eq!(now.get(), WORK_WINDOW + REST_WINDOW);
    }

    #[test]
    fn crossing_the_work_window_during_rate_wait_still_takes_the_break() {
        let (pacer, now, waits) = pacer();
        pacer.before_request();
        now.set(
            WORK_WINDOW
                .checked_sub(Duration::from_secs(10))
                .expect("work window exceeds ten seconds"),
        );
        pacer.state.borrow_mut().last_request_started_at = Some(now.get());

        pacer.before_request();

        assert_eq!(*waits.borrow(), vec![MIN_REQUEST_INTERVAL, REST_WINDOW]);
    }
}
