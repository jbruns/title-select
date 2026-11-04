//! Shadow runs, one at a time: at most one runs and at most one waits, the newest taking the waiting one's place, so
//! shadow mode holds a bounded number of Selections and child processes however fast Selection Jobs end.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Why a shadow run that waited never ran: a newer one took its place.
pub const BUSY: &str =
    "skipped: busy. A newer selection's shadow run took its place while another ran";

/// Why a shadow run that waited never ran: the Service stopped first.
pub const STOPPED: &str = "skipped: the service stopped before it ran";

/// One shadow run: `run` runs it, or `skip` is told why it never will.
pub struct Task {
    pub run: Box<dyn FnOnce() + Send>,
    pub skip: Box<dyn FnOnce(&'static str) + Send>,
}

#[derive(Default)]
pub struct Shadows {
    state: Mutex<State>,
    wake: Condvar,
}

#[derive(Default)]
struct State {
    waiting: Option<Task>,
    worker: Option<JoinHandle<()>>,
    stopped: bool,
}

impl Shadows {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Runs `task` once the one running, if any, ends. A task already waiting is skipped as `BUSY`.
    pub fn submit(self: &Arc<Self>, task: Task) {
        let mut state = self.lock();
        if state.stopped {
            drop(state);
            (task.skip)(STOPPED);
            return;
        }
        let replaced = state.waiting.replace(task);
        if state.worker.is_none() {
            let shadows = self.clone();
            state.worker = Some(std::thread::spawn(move || shadows.work()));
        }
        drop(state);
        self.wake.notify_all();
        if let Some(replaced) = replaced {
            (replaced.skip)(BUSY);
        }
    }

    /// Skips the waiting task, if any, as `STOPPED`, and every task submitted after. The running one, if any, is the
    /// engine's to stop; `join` then waits for it.
    pub fn stop(&self) {
        let mut state = self.lock();
        state.stopped = true;
        let waiting = state.waiting.take();
        drop(state);
        self.wake.notify_all();
        if let Some(waiting) = waiting {
            (waiting.skip)(STOPPED);
        }
    }

    /// Waits up to `within` for the worker, and with it the running task and all it holds, to end once `stop` has
    /// stopped it. False when it hasn't ended by then.
    pub fn join(&self, within: Duration) -> bool {
        let Some(worker) = self.lock().worker.take() else {
            return true;
        };
        let until = Instant::now() + within;
        while !worker.is_finished() {
            if Instant::now() >= until {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        worker.join().is_ok()
    }

    fn work(&self) {
        loop {
            let mut state = self.lock();
            let task = loop {
                if state.stopped {
                    return;
                }
                if let Some(task) = state.waiting.take() {
                    break task;
                }
                state = self.wake.wait(state).unwrap_or_else(|p| p.into_inner());
            };
            drop(state);
            (task.run)();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    /// A task that reports to `events` when it starts, then waits for `go` to finish.
    fn task(
        name: &'static str,
        events: &mpsc::Sender<String>,
        go: Option<mpsc::Receiver<()>>,
    ) -> Task {
        let (ran, skipped) = (events.clone(), events.clone());
        Task {
            run: Box::new(move || {
                ran.send(format!("{name} ran")).unwrap();
                if let Some(go) = go {
                    go.recv().unwrap();
                }
            }),
            skip: Box::new(move |why| skipped.send(format!("{name} {why}")).unwrap()),
        }
    }

    #[test]
    fn one_runs_one_waits_and_the_newest_takes_the_waiting_one_s_place() {
        let shadows = Arc::new(Shadows::default());
        let (events, seen) = mpsc::channel();
        let next = || seen.recv_timeout(Duration::from_secs(10)).unwrap();
        let (go, wait) = mpsc::channel();
        shadows.submit(task("first", &events, Some(wait)));
        assert_eq!(next(), "first ran");
        shadows.submit(task("second", &events, None));
        shadows.submit(task("third", &events, None));
        assert_eq!(next(), format!("second {BUSY}"));
        go.send(()).unwrap();
        assert_eq!(next(), "third ran");

        let (go, wait) = mpsc::channel();
        shadows.submit(task("fourth", &events, Some(wait)));
        assert_eq!(next(), "fourth ran");
        shadows.submit(task("fifth", &events, None));
        shadows.stop();
        assert_eq!(next(), format!("fifth {STOPPED}"));
        shadows.submit(task("sixth", &events, None));
        assert_eq!(next(), format!("sixth {STOPPED}"));
        assert!(!shadows.join(Duration::from_millis(100)));
        go.send(()).unwrap();
        assert!(seen.recv_timeout(Duration::from_millis(200)).is_err());
        assert!(shadows.join(Duration::from_secs(10)));
        assert_eq!(Arc::strong_count(&shadows), 1);
    }
}
