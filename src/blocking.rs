use std;
use std::sync::{atomic, Arc};

pub struct Inner {
  thread : std::thread::Thread,
  woken  : atomic::AtomicBool
}
unsafe impl Send for Inner {}
unsafe impl Sync for Inner {}

#[derive(Clone)]
pub struct SignalToken {
  inner : Arc <Inner>
}

pub struct WaitToken {
  inner : Arc <Inner>
}
impl !Send for WaitToken {}
impl !Sync for WaitToken {}

impl Inner {
  pub fn signal (&self) -> bool {
    let wake = self.woken.compare_exchange (
      false, true, atomic::Ordering::SeqCst, atomic::Ordering::SeqCst
    ).is_ok();
    if wake {
      self.thread.unpark();
    }
    wake
  }
}

impl SignalToken {
  pub fn signal (&self) -> bool {
    let wake = self.inner.woken.compare_exchange (
      false, true, atomic::Ordering::SeqCst,
      atomic::Ordering::SeqCst
    ).is_ok();
    if wake {
      self.inner.thread.unpark();
    }
    wake
  }

  #[inline]
  pub fn to_atomic_ptr (self) -> atomic::AtomicPtr <Inner> {
    atomic::AtomicPtr::new (Arc::as_ptr (&self.inner).cast_mut())
  }

  #[inline]
  pub unsafe fn from_atomic_ptr (signal_ptr : atomic::AtomicPtr <Inner>)
    -> SignalToken
  {
    SignalToken {
      inner: Arc::from_raw (signal_ptr.into_inner())
    }
  }
}

impl WaitToken {
  pub fn wait (self) {
    while !self.inner.woken.load (atomic::Ordering::SeqCst) {
      std::thread::park()
    }
  }

  pub fn wait_max_until (self, end : std::time::Instant) -> bool {
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

pub fn tokens() -> (WaitToken, Arc <Inner>) {
  let signal_token = Arc::new (Inner {
    thread: std::thread::current(),
    woken:  atomic::AtomicBool::new (false)
  });
  let wait_token = WaitToken {
    inner:  signal_token.clone()
  };
  (wait_token, signal_token)
}
