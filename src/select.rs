#![allow(dead_code)]

use ::std;
use ::{Receiver, RecvError, SelectionResult};
use ::blocking;

/// A "receiver set" structure used to manage a set of receivers being selected
/// over.
pub struct Select {
  inner   : std::cell::UnsafeCell <Inner>,
  next_id : std::cell::Cell <usize>
}
impl !Send for Select {}

/// Handle to a receiver which is currently a member of a `Select` set of
/// receivers, used to keep the receiver in the set as well as to interact
/// with the underlying receiver.
pub struct Handle <'rx, T : Send + 'rx> {
  /// The ID of this handle, used to compare against the return value of
  /// `Select:::wait()`
  id       : usize,
  selector : *mut Inner,
  next     : *mut Handle <'static, ()>,
  prev     : *mut Handle <'static, ()>,
  added    : bool,
  packet   : &'rx (dyn Packet + 'rx),
  // due to our fun transmutes, be sure to place this at the end. (nothing
  // previous relies on T)
  rx       : &'rx Receiver <T>
}

struct Inner {
  head : *mut Handle <'static, ()>,
  tail : *mut Handle <'static, ()>
}

struct HandleIter {
  cur : *mut Handle <'static, ()>
}

#[derive(PartialEq, Eq)]
pub enum StartResult {
  Installed,
  Abort
}

pub trait Packet {
  fn can_recv        (&self) -> bool;
  fn start_selection (&self, token : blocking::SignalToken) -> StartResult;
  fn abort_selection (&self) -> bool;
}

impl Select {
  /// New empty selection structure.
  pub fn new () -> Select {
    Select {
      inner: std::cell::UnsafeCell::new (Inner {
        head: std::ptr::null_mut(),
        tail: std::ptr::null_mut()
      }),
      next_id: std::cell::Cell::new (1)
    }
  }

  /// New handle into this receiver set for a new receiver; does *not* add the
  /// receiver to the receiver set, for that call `add` on the handle itself.
  pub fn handle <'a, T> (&'a self, rx : &'a Receiver <T>) -> Handle <'a, T>
    where T : Send
  {
    let id = self.next_id.get();
    self.next_id.set (id + 1);
    Handle {
      id,
      selector: self.inner.get(),
      next:     std::ptr::null_mut(),
      prev:     std::ptr::null_mut(),
      added:    false,
      rx,
      packet:   rx
    }
  }

  /// Wait for an "event" on this receiver set. Returns an ID that can be
  /// queried against any active `Handle` structures `id` method. The handle
  /// with the matching `id` will have some sort of "event" available on it:
  /// either that data is available or the corresponding channel has been
  /// closed.
  pub fn wait (&self) -> usize {
    self.wait2 (true)
  }

  /// Helper method for skipping the "preflight checks" during testing
  fn wait2 (&self, do_preflight_checks : bool) -> usize {
    unsafe {
      // Stage 1: preflight checks
      if do_preflight_checks {
        for handle in self.iter() {
          if (*handle).packet.can_recv() {
            return (*handle).id();
          }
        }
      }
      // Stage 2: begin blocking process
      let (wait_token, signal_token) = blocking::tokens();
      for (i, handle) in self.iter().enumerate() {
        match (*handle).packet.start_selection (signal_token.clone()) {
          StartResult::Installed => {}
          StartResult::Abort     => {
            for handle in self.iter().take (i) {
              (*handle).packet.abort_selection();
            }
            return (*handle).id;
          }
        }
      }
      // Stage 3: no message availble, actually block
      wait_token.wait();
      // Stage 4: must be a message; find it
      let mut ready_id = std::usize::MAX;
      for handle in self.iter() {
        if (*handle).packet.abort_selection() {
          ready_id = (*handle).id;
        }
      }

      // must have found a ready receiver
      assert_ne!(ready_id, std::usize::MAX);
      return ready_id;
    }
  }

  fn iter (&self) -> HandleIter {
    HandleIter {
      cur: unsafe { &*self.inner.get() }.head
    }
  }
}

impl std::fmt::Debug for Select {
  fn fmt (&self, f : &mut std::fmt::Formatter) -> std::fmt::Result {
    write!(f, "Select {{ .. }}")
  }
}

impl Drop for Select {
  fn drop (&mut self) {
    unsafe {
      assert!((&*self.inner.get()).head.is_null());
      assert!((&*self.inner.get()).tail.is_null());
    }
  }
}

impl <'rx, T> Handle <'rx, T> where T : Send {
  #[inline]
  pub fn id (&self) -> usize {
    self.id
  }

  pub fn recv (&mut self) -> Result <T, RecvError> {
    self.rx.recv()
  }

  /// Add this handle to the receiver set that the handle was created from.
  pub unsafe fn add (&mut self) {
    if self.added {
      return
    }

    let selector = &mut *self.selector;
    let me = self as *mut Handle <'rx, T> as *mut Handle <'static, ()>;
    if selector.head.is_null() {
      selector.head = me;
      selector.tail = me;
    } else {
      (*me).prev = selector.tail;
      assert!((*me).next.is_null());
      (*selector.tail).next = me;
      selector.tail = me;
    }

    self.added = true;
  }

  /// Remove this handle from the receiver set.
  pub unsafe fn remove (&mut self) {
    if !self.added {
      return
    }

    let selector = &mut *self.selector;
    let me = self as *mut Handle <'rx, T> as *mut Handle <'static, ()>;
    if self.prev.is_null() {
      assert_eq!(selector.head, me);
      selector.head = self.next;
    } else {
      (*self.prev).next = self.next;
    }
    if self.next.is_null() {
      assert_eq!(selector.tail, me);
      selector.tail = self.prev;
    } else {
      (*self.next).prev = self.prev;
    }

    self.next = std::ptr::null_mut();
    self.prev = std::ptr::null_mut();
    self.added = false;
  }
}

impl <'rx, T> std::fmt::Debug for Handle <'rx, T> where T : Send + 'rx {
  fn fmt (&self, f : &mut std::fmt::Formatter) -> std::fmt::Result {
    write!(f, "Handle {{ .. }}")
  }
}

impl <'rx, T> Drop for Handle <'rx, T> where T : Send {
  fn drop (&mut self) {
    unsafe { self.remove() }
  }
}

impl Iterator for HandleIter {
  type Item = *mut Handle <'static, ()>;
  fn next (&mut self) -> Option <*mut Handle <'static, ()>> {
    if self.cur.is_null() {
      None
    } else {
      let ret = Some (self.cur);
      unsafe {
        self.cur = (*self.cur).next;
      }
      ret
    }
  }
}

impl <T> Packet for Receiver <T> {
  #[inline]
  fn can_recv (&self) -> bool {
    self.can_recv()
  }
  fn start_selection (&self, token : blocking::SignalToken) -> StartResult {
    match self.start_selection (token) {
      SelectionResult::SelSuccess  => StartResult::Installed,
      SelectionResult::SelCanceled => StartResult::Abort
    }
  }
  #[inline]
  fn abort_selection (&self) -> bool {
    self.abort_selection()
  }
}

#[macro_export]
macro_rules! select {
  (
    $($name:pat = $rx:ident.$meth:ident() => $code:expr),+
  ) => {{
    let sel = Select::new();
    $(
    let mut $rx = sel.handle (&$rx);
    )+
    unsafe {
      $($rx.add();)+
    }
    let ret = sel.wait();
    $(
    if ret == $rx.id() {
      let $name = $rx.$meth(); $code
    } else
    )+
    { unreachable!() }
  }}
}

#[allow(unused_imports)]
#[cfg(test)]
mod tests {
  use super::*;
  use super::super::*;

  #[test]
  fn smoke() {
    let (tx1, rx1) = channel::<i32>();
    let (tx2, rx2) = channel::<i32>();
    tx1.send (1).unwrap();
    select! {
      foo = rx1.recv() => { assert_eq!(foo.unwrap(), 1); },
      _bar = rx2.recv() => panic!()
    }
    tx2.send (2).unwrap();
    select! {
      _foo = rx1.recv() => panic!(),
      bar = rx2.recv() => assert_eq!(bar.unwrap(), 2)
    }
    drop(tx1);
    select! {
      foo = rx1.recv() => { assert!(foo.is_err()); },
      _bar = rx2.recv() => panic!()
    }
    drop(tx2);
    select! {
      bar = rx2.recv() => { assert!(bar.is_err()); }
    }
  }

  #[test]
  fn smoke2() {
    let (_tx1, rx1) = channel::<i32>();
    let (_tx2, rx2) = channel::<i32>();
    let (_tx3, rx3) = channel::<i32>();
    let (_tx4, rx4) = channel::<i32>();
    let (tx5, rx5) = channel::<i32>();
    tx5.send (4).unwrap();
    select! {
      _foo = rx1.recv() => panic!("1"),
      _foo = rx2.recv() => panic!("2"),
      _foo = rx3.recv() => panic!("3"),
      _foo = rx4.recv() => panic!("4"),
      foo = rx5.recv() => { assert_eq!(foo.unwrap(), 4); }
    }
  }

  #[test]
  fn closed() {
    let (_tx1, rx1) = channel::<i32>();
    let (tx2, rx2) = channel::<i32>();
    drop(tx2);

    select! {
      _a1 = rx1.recv() => panic!(),
      a2 = rx2.recv() => { assert!(a2.is_err()); }
    }
  }

  #[test]
  fn unblocks() {
    let (tx1, rx1) = channel::<i32>();
    let (_tx2, rx2) = channel::<i32>();
    let (tx3, rx3) = channel::<i32>();

    let _t = std::thread::spawn(move|| {
      for _ in 0..20 { std::thread::yield_now(); }
      tx1.send (1).unwrap();
      rx3.recv().unwrap();
      for _ in 0..20 { std::thread::yield_now(); }
    });

    select! {
      a = rx1.recv() => { assert_eq!(a.unwrap(), 1); },
      _b = rx2.recv() => panic!()
    }
    tx3.send (1).unwrap();
    select! {
      a = rx1.recv() => assert!(a.is_err()),
      _b = rx2.recv() => panic!()
    }
  }

  #[test]
  fn both_ready() {
    let (tx1, rx1) = channel::<i32>();
    let (tx2, rx2) = channel::<i32>();
    let (tx3, rx3) = channel::<bool>();

    let _t = std::thread::spawn(move|| {
      for _ in 0..20 { std::thread::yield_now(); }
      tx1.send (1).unwrap();
      tx2.send (2).unwrap();
      rx3.recv().unwrap();
    });

    select! {
      a = rx1.recv() => { assert_eq!(a.unwrap(), 1); },
      a = rx2.recv() => { assert_eq!(a.unwrap(), 2); }
    }
    select! {
      a = rx1.recv() => { assert_eq!(a.unwrap(), 1); },
      a = rx2.recv() => { assert_eq!(a.unwrap(), 2); }
    }
    assert_eq!(rx1.try_recv(), Err (TryRecvError::Empty));
    assert_eq!(rx2.try_recv(), Err (TryRecvError::Empty));
    tx3.send (true).unwrap();
  }

  #[test]
  fn stress() {
    const AMT: i32 = 10000;
    let (tx1, rx1) = channel::<i32>();
    let (tx2, rx2) = channel::<i32>();
    let (tx3, rx3) = channel::<bool>();

    let _t = std::thread::spawn(move|| {
      for i in 0..AMT {
        if i % 2 == 0 {
          tx1.send (i).unwrap();
        } else {
          tx2.send (i).unwrap();
        }
        rx3.recv().unwrap();
      }
    });

    for i in 0..AMT {
      select! {
        i1 = rx1.recv() => { assert!(i % 2 == 0 && i == i1.unwrap()); },
        i2 = rx2.recv() => { assert!(i % 2 == 1 && i == i2.unwrap()); }
      }
      tx3.send (true).unwrap();
    }
  }

  #[test]
  fn preflight1() {
    let (tx, rx) = channel();
    tx.send (true).unwrap();
    select! {
      _n = rx.recv() => {}
    }
  }

  #[test]
  fn preflight2() {
    let (tx, rx) = channel();
    tx.send (true).unwrap();
    tx.send (true).unwrap();
    select! {
      _n = rx.recv() => {}
    }
  }

  #[test]
  fn preflight4() {
    let (tx, rx) = channel();
    tx.send (true).unwrap();
    let s = Select::new();
    let mut h = s.handle (&rx);
    unsafe { h.add(); }
    assert_eq!(s.wait2 (false), h.id);
  }

  #[test]
  fn preflight5() {
    let (tx, rx) = channel();
    tx.send (true).unwrap();
    tx.send (true).unwrap();
    let s = Select::new();
    let mut h = s.handle(&rx);
    unsafe { h.add(); }
    assert_eq!(s.wait2 (false), h.id);
  }

  #[test]
  fn preflight7() {
    let (tx, rx) = channel::<bool>();
    drop(tx);
    let s = Select::new();
    let mut h = s.handle(&rx);
    unsafe { h.add(); }
    assert_eq!(s.wait2 (false), h.id);
  }

  #[test]
  fn preflight8() {
    let (tx, rx) = channel();
    tx.send (true).unwrap();
    drop(tx);
    rx.recv().unwrap();
    let s = Select::new();
    let mut h = s.handle(&rx);
    unsafe { h.add(); }
    assert_eq!(s.wait2 (false), h.id);
  }

  #[test]
  fn oneshot_data_waiting() {
    let (tx1, rx1) = channel();
    let (tx2, rx2) = channel();
    let _t = std::thread::spawn(move|| {
      select! {
        _n = rx1.recv() => {}
      }
      tx2.send (true).unwrap();
    });

    for _ in 0..100 { std::thread::yield_now() }
    tx1.send (true).unwrap();
    rx2.recv().unwrap();
  }

  #[test]
  fn stream_data_waiting() {
    let (tx1, rx1) = channel();
    let (tx2, rx2) = channel();
    tx1.send (true).unwrap();
    tx1.send (true).unwrap();
    rx1.recv().unwrap();
    rx1.recv().unwrap();
    let _t = std::thread::spawn(move|| {
      select! {
        _n = rx1.recv() => {}
      }
      tx2.send (true).unwrap();
    });

    for _ in 0..100 { std::thread::yield_now() }
    tx1.send (true).unwrap();
    rx2.recv().unwrap();
  }

  #[test]
  fn fmt_debug_select() {
    let sel = Select::new();
    assert_eq!(format!("{:?}", sel), "Select { .. }");
  }

  #[test]
  fn fmt_debug_handle() {
    let (_, rx) = channel::<i32>();
    let sel = Select::new();
    let handle = sel.handle(&rx);
    assert_eq!(format!("{:?}", handle), "Handle { .. }");
  }
}
