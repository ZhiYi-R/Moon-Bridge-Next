use tokio::sync::watch;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    Starting,
    Running,
    Restarting,
    Stopping,
    Stopped,
    Failed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Intent {
    Continue,
    Restart,
    Stop,
}

#[derive(Clone)]
pub struct Snapshot {
    pub running: bool,
    pub addr: String,
    pub error: Option<String>,
    pub phase: Phase,
    intent: Intent,
}

pub struct Lifecycle {
    state: watch::Sender<Snapshot>,
}

impl Default for Lifecycle {
    fn default() -> Self {
        let (state, _) = watch::channel(Snapshot {
            running: false,
            addr: String::new(),
            error: None,
            phase: Phase::Starting,
            intent: Intent::Continue,
        });
        Self { state }
    }
}

impl Lifecycle {
    pub fn snapshot(&self) -> Snapshot {
        self.state.borrow().clone()
    }

    pub fn request_restart(&self) {
        self.state.send_modify(|state| {
            if state.phase == Phase::Running && state.intent == Intent::Continue {
                state.intent = Intent::Restart;
                state.phase = Phase::Restarting;
                state.running = false;
            }
        });
    }

    pub fn request_stop(&self) {
        self.state.send_modify(|state| {
            state.intent = Intent::Stop;
            state.phase = Phase::Stopping;
            state.running = false;
        });
    }

    pub fn begin_start(&self) -> bool {
        let mut start = false;
        self.state.send_modify(|state| {
            if state.intent != Intent::Stop {
                state.intent = Intent::Continue;
                state.phase = Phase::Starting;
                state.running = false;
                state.addr.clear();
                state.error = None;
                start = true;
            }
        });
        start
    }

    pub fn listening(&self, addr: std::net::SocketAddr) {
        self.state.send_modify(|state| {
            state.addr = addr.to_string();
            if state.intent == Intent::Continue {
                state.phase = Phase::Running;
                state.running = true;
            }
        });
    }

    pub fn drained(&self) {
        self.state.send_modify(|state| {
            state.running = false;
            state.addr.clear();
        });
    }

    pub fn finish(&self, failed: bool) {
        self.state.send_modify(|state| {
            state.running = false;
            state.addr.clear();
            state.phase = if failed {
                Phase::Failed
            } else {
                Phase::Stopped
            };
            state.error = failed.then(|| "服务启动或运行失败".to_string());
        });
    }

    pub fn stopping(&self) -> bool {
        self.state.borrow().intent == Intent::Stop
    }

    pub async fn wait_for_drain(&self) {
        let mut state = self.state.subscribe();
        loop {
            if state.borrow_and_update().intent != Intent::Continue {
                return;
            }
            if state.changed().await.is_err() {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn running_requires_a_bound_address_and_clears_it_after_drain() {
        let lifecycle = Lifecycle::default();
        let snapshot = lifecycle.snapshot();
        assert!(!snapshot.running);
        assert_eq!(snapshot.phase, Phase::Starting);
        assert!(snapshot.addr.is_empty());
        assert!(lifecycle.begin_start());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        assert_ne!(addr.port(), 0);
        lifecycle.listening(addr);
        let snapshot = lifecycle.snapshot();
        assert!(snapshot.running);
        assert_eq!(snapshot.phase, Phase::Running);
        assert_eq!(snapshot.addr, addr.to_string());
        lifecycle.request_restart();
        assert_eq!(lifecycle.snapshot().addr, addr.to_string());
        lifecycle.drained();
        assert!(lifecycle.snapshot().addr.is_empty());
        assert!(!lifecycle.snapshot().running);
    }

    #[tokio::test]
    async fn repeated_restart_requests_coalesce_and_next_generation_runs() {
        let lifecycle = Lifecycle::default();
        lifecycle.request_restart();
        assert_eq!(lifecycle.snapshot().phase, Phase::Starting);
        assert!(lifecycle.begin_start());
        lifecycle.listening("127.0.0.1:40001".parse().unwrap());
        lifecycle.request_restart();
        lifecycle.request_restart();
        assert_eq!(lifecycle.snapshot().phase, Phase::Restarting);
        assert!(!lifecycle.snapshot().running);
        assert!(!lifecycle.stopping());
        tokio::time::timeout(Duration::from_secs(1), lifecycle.wait_for_drain())
            .await
            .unwrap();
        lifecycle.drained();
        assert!(lifecycle.begin_start());
        lifecycle.request_restart();
        assert_eq!(lifecycle.snapshot().phase, Phase::Starting);
        lifecycle.listening("127.0.0.1:40002".parse().unwrap());
        assert!(lifecycle.snapshot().running);
        assert_eq!(lifecycle.snapshot().phase, Phase::Running);
        assert_eq!(lifecycle.snapshot().addr, "127.0.0.1:40002");
        assert!(
            tokio::time::timeout(Duration::from_millis(20), lifecycle.wait_for_drain())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn stop_wins_over_restart_and_late_listening() {
        let lifecycle = Lifecycle::default();
        assert!(lifecycle.begin_start());
        lifecycle.listening("127.0.0.1:40003".parse().unwrap());
        lifecycle.request_restart();
        lifecycle.request_stop();
        lifecycle.request_restart();
        lifecycle.listening("127.0.0.1:40004".parse().unwrap());
        assert!(lifecycle.stopping());
        assert!(!lifecycle.begin_start());
        assert_eq!(lifecycle.snapshot().phase, Phase::Stopping);
        assert!(!lifecycle.snapshot().running);
        tokio::time::timeout(Duration::from_secs(1), lifecycle.wait_for_drain())
            .await
            .unwrap();
        lifecycle.drained();
        lifecycle.finish(false);
        assert_eq!(lifecycle.snapshot().phase, Phase::Stopped);
        assert!(!lifecycle.snapshot().running);
        assert!(lifecycle.snapshot().addr.is_empty());
        assert!(lifecycle.snapshot().error.is_none());
        assert!(!lifecycle.begin_start());
    }

    #[test]
    fn failure_does_not_report_a_live_listener() {
        let lifecycle = Lifecycle::default();
        assert!(lifecycle.begin_start());
        lifecycle.listening("127.0.0.1:40005".parse().unwrap());
        lifecycle.finish(true);
        let snapshot = lifecycle.snapshot();
        assert_eq!(snapshot.phase, Phase::Failed);
        assert!(!snapshot.running);
        assert!(snapshot.addr.is_empty());
        assert!(snapshot.error.is_some());
    }
}
