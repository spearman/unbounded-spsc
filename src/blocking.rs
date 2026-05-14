use std;
use std::sync::{atomic, Arc};

pub(crate) struct Inner {
  thread : std::thread::Thread,
  woken  : atomic::AtomicBool
}
unsafe impl Send for Inner {}
unsafe impl Sync for Inner {}

pub(crate) struct WaitToken {
  inner : Arc <Inner>
}
impl !Send for WaitToken {}
impl !Sync for WaitToken {}

impl Inner {
  pub(crate) fn signal (&self) -> bool {
    let wake = self.woken.compare_exchange (
      false, true, atomic::Ordering::SeqCst, atomic::Ordering::SeqCst
    ).is_ok();
    if wake {
      self.thread.unpark();
    }
    wake
  }
}

impl WaitToken {
  pub(crate) fn wait (self) {
    while !self.inner.woken.load (atomic::Ordering::SeqCst) {
      std::thread::park()
    }
  }

  pub(crate) fn wait_max_until (self, end : std::time::Instant) -> bool {
    while !self.inner.woken.load (atomic::Ordering::SeqCst) {
      let now = std::time::Instant::now();
      if end <= now {
        return false;
      }
      std::thread::park_timeout (end - now)
    }
    true
  }
}

pub(crate) fn tokens() -> (WaitToken, Arc <Inner>) {
  let signal_token = Arc::new (Inner {
    thread: std::thread::current(),
    woken:  atomic::AtomicBool::new (false)
  });
  let wait_token = WaitToken {
    inner:  signal_token.clone()
  };
  (wait_token, signal_token)
}
