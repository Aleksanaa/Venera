use std::sync::mpsc::SyncSender;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, RecvTimeoutError},
    },
    time::{Duration, Instant},
};
use venera_debug_protocol::SystemInfoResult;

pub fn wait(
    receiver: &Receiver<SystemInfoResult>,
    timeout: Duration,
    stop: &AtomicBool,
) -> Option<SystemInfoResult> {
    let started = Instant::now();
    while !stop.load(Ordering::Acquire) {
        let remaining = timeout.checked_sub(started.elapsed())?;
        match receiver.recv_timeout(remaining.min(Duration::from_millis(100))) {
            Ok(result) => return Some(result),
            Err(RecvTimeoutError::Timeout) => (),
            Err(RecvTimeoutError::Disconnected) => return None,
        }
    }
    None
}

pub struct PendingReport {
    pub request_id: String,
    pub session_id: u32,
    pub sender: SyncSender<SystemInfoResult>,
    sent: bool,
}

impl PendingReport {
    pub fn new(request_id: String, session_id: u32, sender: SyncSender<SystemInfoResult>) -> Self {
        Self {
            request_id,
            session_id,
            sender,
            sent: false,
        }
    }
    pub fn command(&mut self, session_id: u32) -> Option<String> {
        if self.session_id != session_id || self.sent {
            return None;
        }
        self.sent = true;
        Some(self.request_id.clone())
    }
    pub fn deliver(&self, session_id: u32, result: SystemInfoResult) -> bool {
        if self.sent
            && session_id == self.session_id
            && result.session_id == Some(session_id)
            && result.request_id == self.request_id
        {
            return self.sender.try_send(result).is_ok();
        }
        false
    }
}

pub fn clear(slot: &mut Option<PendingReport>, request_id: &str) {
    if slot.as_ref().is_some_and(|p| p.request_id == request_id) {
        *slot = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::{sync::mpsc, time::Duration};

    fn report(id: &str, session: u32) -> SystemInfoResult {
        serde_json::from_value(json!({"request_id":id,"ok":false,"collected_at":"2026-11-11T00:00:00Z","session_id":session,"sections":[],"error":{"code":"timeout","message":"System report collection timed out"}})).unwrap()
    }
    #[test]
    fn report_commands_and_results_follow_request_and_session() {
        let (tx, rx) = mpsc::sync_channel(1);
        let mut pending = PendingReport::new("report-7".into(), 2, tx);
        assert_eq!(pending.command(1), None);
        assert!(!pending.deliver(2, report("report-7", 2)));
        assert_eq!(pending.command(2), Some("report-7".into()));
        assert_eq!(pending.command(2), None);
        assert!(!pending.deliver(1, report("report-7", 2)));
        assert!(!pending.deliver(2, report("old-report", 2)));
        assert!(!pending.deliver(2, report("report-7", 1)));
        assert!(pending.deliver(2, report("report-7", 2)));
        assert_eq!(
            serde_json::to_value(rx.recv().unwrap()).unwrap(),
            json!({"request_id":"report-7","ok":false,"collected_at":"2026-11-11T00:00:00Z","session_id":2,"sections":[],"error":{"code":"timeout","message":"System report collection timed out"}})
        );
    }
    #[test]
    fn expired_report_cleanup_preserves_the_next_request() {
        let (tx, rx) = mpsc::sync_channel(1);
        let mut slot = Some(PendingReport::new("report-8".into(), 3, tx));
        clear(&mut slot, "report-7");
        assert_eq!(slot.as_mut().unwrap().command(3), Some("report-8".into()));
        clear(&mut slot, "report-8");
        assert!(slot.is_none());
        assert_eq!(
            rx.recv_timeout(Duration::from_millis(1)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        );
    }
    #[test]
    fn successful_report_delivers_graphics_findings() {
        let (tx, rx) = mpsc::sync_channel(1);
        let mut pending = PendingReport::new("report-9".into(), 2, tx);
        assert_eq!(pending.command(2), Some("report-9".into()));
        let result = json!({"request_id":"report-9","ok":true,"collected_at":"2026-11-11T00:00:00Z","session_id":2,"sections":[{"title":"Graphics","rows":[{"key":"Adapter","value":"Unavailable","bad":true,"warning":"请检查图形驱动。\n重新启动 Windows 后重试。"}]}]});
        assert!(pending.deliver(2, serde_json::from_value(result.clone()).unwrap()));
        assert_eq!(serde_json::to_value(rx.recv().unwrap()).unwrap(), result);
    }
    #[test]
    fn report_wait_returns_findings_and_expires_unanswered_requests() {
        let (tx, rx) = mpsc::sync_channel(1);
        let stop = AtomicBool::new(false);
        assert_eq!(wait(&rx, Duration::from_millis(1), &stop), None);
        tx.send(report("report-10", 3)).unwrap();
        assert_eq!(
            serde_json::to_value(wait(&rx, Duration::from_secs(1), &stop).unwrap()).unwrap(),
            json!({"request_id":"report-10","ok":false,"collected_at":"2026-11-11T00:00:00Z","session_id":3,"sections":[],"error":{"code":"timeout","message":"System report collection timed out"}})
        );
        stop.store(true, Ordering::Release);
        assert_eq!(wait(&rx, Duration::from_secs(30), &stop), None);
    }
}
