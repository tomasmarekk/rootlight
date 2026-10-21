//! Cooperative construction budgets remain outside speculative parser state.
//! Scanner loops check during long tokens; no timeout thread or synthetic EOF
//! can publish an interrupted AST as a successful source certificate.

use std::sync::{
    Arc,
    atomic::{AtomicU8, AtomicUsize, Ordering},
};

use raffia::{ParseCheckpoint, ParseControl};
use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;

use super::failure;

pub(super) struct Control {
    cancellation: Cancellation,
    stopped: AtomicU8,
    work: AtomicUsize,
    tokens: AtomicUsize,
    constructions: AtomicUsize,
    depth: AtomicUsize,
    maximum_work: usize,
    maximum_tokens: usize,
    maximum_constructions: usize,
    maximum_depth: usize,
}

fn update_counter(counter: &AtomicUsize, update: impl Fn(usize) -> Option<usize>) -> bool {
    let mut current = counter.load(Ordering::Relaxed);
    loop {
        let Some(next) = update(current) else {
            return false;
        };
        match counter.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return true,
            Err(observed) => current = observed,
        }
    }
}

impl Control {
    pub(super) fn new(
        nodes: usize,
        depth: usize,
        cancellation: &Cancellation,
    ) -> Result<Arc<Self>, AdapterError> {
        Ok(Arc::new(Self {
            cancellation: cancellation.clone(),
            stopped: AtomicU8::new(0),
            work: AtomicUsize::new(0),
            tokens: AtomicUsize::new(0),
            constructions: AtomicUsize::new(0),
            depth: AtomicUsize::new(0),
            maximum_work: nodes
                .checked_mul(128)
                .ok_or_else(|| failure("less-control-accounting"))?,
            maximum_tokens: nodes
                .checked_mul(8)
                .ok_or_else(|| failure("less-control-accounting"))?,
            maximum_constructions: nodes
                .checked_mul(16)
                .ok_or_else(|| failure("less-control-accounting"))?,
            maximum_depth: depth.min(256),
        }))
    }

    fn stop(&self, reason: u8) -> bool {
        let _ = self
            .stopped
            .compare_exchange(0, reason, Ordering::Relaxed, Ordering::Relaxed);
        false
    }

    fn charge(&self, counter: &AtomicUsize, maximum: usize, reason: u8) -> bool {
        if !update_counter(counter, |value| {
            value.checked_add(1).filter(|next| *next <= maximum)
        }) {
            return self.stop(reason);
        }
        true
    }

    fn step(&self, checkpoint: ParseCheckpoint) -> bool {
        if self.stopped.load(Ordering::Relaxed) != 0 {
            return false;
        }
        if !self.charge(&self.work, self.maximum_work, 2) {
            return false;
        }
        let work = self.work.load(Ordering::Relaxed);
        if (!matches!(checkpoint, ParseCheckpoint::Scan) || work.is_multiple_of(256))
            && self.cancellation.check().is_err()
        {
            return self.stop(1);
        }
        match checkpoint {
            ParseCheckpoint::Scan => true,
            ParseCheckpoint::Token => self.charge(&self.tokens, self.maximum_tokens, 3),
            ParseCheckpoint::Enter => {
                self.charge(&self.constructions, self.maximum_constructions, 4)
                    && self.charge(&self.depth, self.maximum_depth, 5)
            }
            ParseCheckpoint::Leave => update_counter(&self.depth, |value| value.checked_sub(1)),
        }
    }

    pub(super) fn hook(self: &Arc<Self>) -> ParseControl {
        let control = Arc::clone(self);
        Arc::new(move |checkpoint| control.step(checkpoint))
    }

    pub(super) fn check(&self) -> Result<(), AdapterError> {
        self.cancellation.check()?;
        match self.stopped.load(Ordering::Relaxed) {
            0 => Ok(()),
            1 => Err(failure("less-cancellation-state")),
            2 => Err(failure("less-parser-work-limit")),
            3 => Err(failure("less-token-work-limit")),
            4 => Err(failure("less-construction-work-limit")),
            _ => Err(failure("less-parser-depth-limit")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use raffia::{ParserBuilder, Syntax, ast::Stylesheet, error::ErrorKind};
    use rootlight_cancel::CancellationReason;

    #[test]
    fn atomic_counter_limits_remain_exact_under_contention() {
        let counter = Arc::new(AtomicUsize::new(0));
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let counter = Arc::clone(&counter);
                std::thread::spawn(move || {
                    (0..128)
                        .filter(|_| {
                            update_counter(&counter, |value| {
                                value.checked_add(1).filter(|next| *next <= 512)
                            })
                        })
                        .count()
                })
            })
            .collect();
        assert_eq!(
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .sum::<usize>(),
            512
        );
        assert_eq!(counter.load(Ordering::Relaxed), 512);
        counter.store(usize::MAX, Ordering::Relaxed);
        assert!(!update_counter(&counter, |value| value.checked_add(1)));
        assert_eq!(counter.load(Ordering::Relaxed), usize::MAX);
        counter.store(0, Ordering::Relaxed);
        assert!(!update_counter(&counter, |value| value.checked_sub(1)));
        assert_eq!(counter.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn cancellation_inside_long_native_tokens_is_a_typed_abort() {
        for source in [
            format!("@x: \"{}\"\n", "a".repeat(100_000)),
            format!("/* {} */\n", "a".repeat(100_000)),
            format!("// {}\n", "a".repeat(100_000)),
        ] {
            let cancellation = Cancellation::new();
            let control = Control::new(16_384, 128, &cancellation).unwrap();
            let base = control.hook();
            let scans = Arc::new(AtomicUsize::new(0));
            let observed = Arc::clone(&scans);
            let token = cancellation.clone();
            let hook: ParseControl = Arc::new(move |checkpoint| {
                if matches!(checkpoint, ParseCheckpoint::Scan)
                    && observed.fetch_add(1, Ordering::Relaxed) == 512
                {
                    token.cancel(CancellationReason::ClientRequest);
                }
                base(checkpoint)
            });
            let mut parser = ParserBuilder::new(&source)
                .syntax(Syntax::Less)
                .control(hook)
                .build();
            let error = parser
                .parse::<Stylesheet>()
                .expect_err("must abort inside scanner work");
            assert!(matches!(error.kind, ErrorKind::ControlledStop));
            assert!(matches!(
                control.check(),
                Err(AdapterError::Cancelled {
                    reason: CancellationReason::ClientRequest
                })
            ));
            assert!(scans.load(Ordering::Relaxed) < 1024);
        }
    }

    #[test]
    fn construction_or_token_work_stop_cannot_become_a_clean_eof() {
        let source = format!("@x: \"{}\"\n", "a".repeat(10_000));
        let control = Control::new(16, 128, &Cancellation::new()).unwrap();
        let mut parser = ParserBuilder::new(&source)
            .syntax(Syntax::Less)
            .control(control.hook())
            .build();
        let error = parser
            .parse::<Stylesheet>()
            .expect_err("bounded native work");
        assert!(matches!(error.kind, ErrorKind::ControlledStop));
        assert!(
            matches!(control.check(), Err(AdapterError::ProviderFailed { code }) if code.as_str() == "less-parser-work-limit")
        );
    }
}
