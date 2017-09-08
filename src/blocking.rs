use ::std;

struct Inner {
  thread : std::thread::Thread,
  woken  : std::sync::atomic::AtomicBool
}
unsafe impl Send for Inner {}
unsafe impl Sync for Inner {}

#[derive(Clone)]
pub struct SignalToken {
  inner : std::sync::Arc <Inner>
}

pub struct WaitToken {
  inner : std::sync::Arc <Inner>
}
impl !Send for WaitToken {}
impl !Sync for WaitToken {}

impl SignalToken {
  pub fn signal (&self) -> bool {
    let wake = !self.inner.woken.compare_and_swap (
      false, true, std::sync::atomic::Ordering::SeqCst);
    if wake {
      self.inner.thread.unpark();
    }
    wake
  }

  #[inline]
  pub unsafe fn cast_to_usize (self) -> usize {
    std::mem::transmute (self.inner)
  }

  #[inline]
  pub unsafe fn cast_from_usize (signal_ptr : usize) -> SignalToken {
    SignalToken {
      inner: std::mem::transmute (signal_ptr)
    }
  }
}

impl WaitToken {
  pub fn wait (self) {
    while !self.inner.woken.load (std::sync::atomic::Ordering::SeqCst) {
      std::thread::park()
    }
  }

  pub fn wait_max_until (self, end : std::time::Instant) -> bool {
    while !self.inner.woken.load (std::sync::atomic::Ordering::SeqCst) {
      let now = std::time::Instant::now();
      if end <= now {
        return false;
      }
      std::thread::park_timeout (end - now)
    }
    true
  }
}

pub fn tokens() -> (WaitToken, SignalToken) {
  let inner = std::sync::Arc::new (Inner {
    thread: std::thread::current(),
    woken:  std::sync::atomic::AtomicBool::new (false)
  });
  let wait_token = WaitToken {
    inner:  inner.clone()
  };
  let signal_token = SignalToken {
    inner
  };
  (wait_token, signal_token)
}
