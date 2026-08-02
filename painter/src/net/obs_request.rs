//! OBS projector要求の純状態機械。
//!
//! Workerの完了順やWin32 timerの揺らぎに依存せず、現在の世代と共通deadlineに一致する
//! eventだけを適用する。時刻は呼び出し側から渡し、テストではsleepを使わない。

#![cfg_attr(not(windows), allow(dead_code))]

use std::time::{Duration, Instant};

pub const PROJECTOR_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

pub type RequestGeneration = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestToken {
    pub generation: RequestGeneration,
    pub deadline: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerDisposition {
    /// 終了済み、または別世代の結果。
    Ignored,
    /// 現世代のworkerは成功した。projectorの実表示確認を待つ。
    AwaitingProjector,
    /// 現世代のworkerがdeadline内に失敗した。
    Failed(RequestGeneration),
    /// 結果処理時点で共通deadlineを過ぎていた。
    TimedOut(RequestGeneration),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollDisposition {
    Idle,
    Waiting,
    Ready(RequestGeneration),
    TimedOut(RequestGeneration),
}

#[derive(Debug, Default)]
pub struct ProjectorRequestTracker {
    next_generation: RequestGeneration,
    pending: Option<PendingRequest>,
}

#[derive(Debug)]
struct PendingRequest {
    token: RequestToken,
    worker_finished: bool,
}

impl ProjectorRequestTracker {
    pub fn begin(&mut self, now: Instant) -> Option<RequestToken> {
        if self.pending.is_some() {
            return None;
        }
        // generation 0も有効だが、ログを読みやすくするため1始まりにする。
        let generation = self.next_generation.wrapping_add(1).max(1);
        self.next_generation = generation;
        let token = RequestToken {
            generation,
            deadline: now + PROJECTOR_REQUEST_TIMEOUT,
        };
        self.pending = Some(PendingRequest {
            token,
            worker_finished: false,
        });
        Some(token)
    }

    pub fn worker_finished(
        &mut self,
        now: Instant,
        generation: RequestGeneration,
        succeeded: bool,
    ) -> WorkerDisposition {
        let Some(pending) = self.pending.as_mut() else {
            return WorkerDisposition::Ignored;
        };
        if pending.token.generation != generation || pending.worker_finished {
            return WorkerDisposition::Ignored;
        }
        if now >= pending.token.deadline {
            self.pending = None;
            return WorkerDisposition::TimedOut(generation);
        }
        if succeeded {
            pending.worker_finished = true;
            WorkerDisposition::AwaitingProjector
        } else {
            self.pending = None;
            WorkerDisposition::Failed(generation)
        }
    }

    pub fn poll(&mut self, now: Instant, projector_visible: bool) -> PollDisposition {
        let Some(pending) = self.pending.as_ref() else {
            return PollDisposition::Idle;
        };
        let token = pending.token;
        if now >= token.deadline {
            self.pending = None;
            return PollDisposition::TimedOut(token.generation);
        }
        // 見えているprojectorが手動起動や旧世代のものでも、現世代workerの成功前には
        // 「自分が開いた」と扱わない。成功結果と表示確認の両方が同じ世代内で揃って初めて完了する。
        if pending.worker_finished && projector_visible {
            self.pending = None;
            return PollDisposition::Ready(token.generation);
        }
        PollDisposition::Waiting
    }

    pub fn cancel(&mut self) -> Option<RequestGeneration> {
        self.pending.take().map(|pending| pending.token.generation)
    }

    #[cfg(test)]
    fn pending_generation(&self) -> Option<RequestGeneration> {
        self.pending
            .as_ref()
            .map(|pending| pending.token.generation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timeout_retry_ignores_old_failure_and_old_success() {
        let started = Instant::now();
        let mut tracker = ProjectorRequestTracker::default();
        let first = tracker.begin(started).unwrap();
        assert_eq!(
            tracker.poll(first.deadline, false),
            PollDisposition::TimedOut(first.generation)
        );

        let second = tracker.begin(first.deadline).unwrap();
        assert_ne!(first.generation, second.generation);
        assert_eq!(
            tracker.worker_finished(
                second.deadline - Duration::from_secs(1),
                first.generation,
                false,
            ),
            WorkerDisposition::Ignored
        );
        assert_eq!(tracker.pending_generation(), Some(second.generation));
        assert_eq!(
            tracker.worker_finished(
                second.deadline - Duration::from_millis(500),
                first.generation,
                true,
            ),
            WorkerDisposition::Ignored
        );
        assert_eq!(tracker.pending_generation(), Some(second.generation));

        assert_eq!(
            tracker.worker_finished(
                second.deadline - Duration::from_millis(250),
                second.generation,
                true,
            ),
            WorkerDisposition::AwaitingProjector
        );
        assert_eq!(
            tracker.poll(second.deadline - Duration::from_millis(100), true),
            PollDisposition::Ready(second.generation)
        );
    }

    #[test]
    fn worker_result_at_or_after_deadline_times_out_without_applying_result() {
        let started = Instant::now();
        let mut tracker = ProjectorRequestTracker::default();
        let request = tracker.begin(started).unwrap();

        assert_eq!(
            tracker.worker_finished(request.deadline, request.generation, true),
            WorkerDisposition::TimedOut(request.generation)
        );
        assert_eq!(tracker.pending_generation(), None);
        assert_eq!(
            tracker.worker_finished(
                request.deadline + Duration::from_secs(1),
                request.generation,
                false,
            ),
            WorkerDisposition::Ignored
        );
    }

    #[test]
    fn current_failure_cancels_only_its_own_generation() {
        let started = Instant::now();
        let mut tracker = ProjectorRequestTracker::default();
        let request = tracker.begin(started).unwrap();
        assert_eq!(
            tracker.worker_finished(started, request.generation, false),
            WorkerDisposition::Failed(request.generation)
        );
        assert_eq!(tracker.poll(started, true), PollDisposition::Idle);
    }

    #[test]
    fn visible_projector_waits_for_the_current_worker_success() {
        let started = Instant::now();
        let mut tracker = ProjectorRequestTracker::default();
        let request = tracker.begin(started).unwrap();

        assert_eq!(tracker.poll(started, true), PollDisposition::Waiting);
        assert_eq!(
            tracker.worker_finished(started, request.generation, true),
            WorkerDisposition::AwaitingProjector
        );
        assert_eq!(
            tracker.poll(started, true),
            PollDisposition::Ready(request.generation)
        );
    }

    #[test]
    fn exit_cancel_makes_late_worker_result_inert() {
        let started = Instant::now();
        let mut tracker = ProjectorRequestTracker::default();
        let request = tracker.begin(started).unwrap();
        assert_eq!(tracker.cancel(), Some(request.generation));
        assert_eq!(
            tracker.worker_finished(started, request.generation, true),
            WorkerDisposition::Ignored
        );
        assert_eq!(tracker.poll(started, true), PollDisposition::Idle);
    }

    #[test]
    fn second_begin_is_rejected_while_current_request_is_pending() {
        let started = Instant::now();
        let mut tracker = ProjectorRequestTracker::default();
        let request = tracker.begin(started).unwrap();
        assert_eq!(tracker.begin(started), None);
        assert_eq!(tracker.pending_generation(), Some(request.generation));
    }
}
