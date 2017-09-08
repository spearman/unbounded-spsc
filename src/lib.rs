//! This library adapts the block-waiting `recv` mechanism from the Rust
//! standard library to an unbounded SPSC channel backed by
//! `bounded_spsc_queue`.

#![feature(optin_builtin_traits)]
#![feature(box_syntax)]

extern crate bounded_spsc_queue;

use std::sync::atomic::Ordering;

mod blocking;
mod select;

const DISCONNECTED     : isize = std::isize::MIN;
#[cfg(test)]
const MAX_STEALS       : isize = 5;
#[cfg(not(test))]
const MAX_STEALS       : isize = 1 << 20;   // ~1 million
const INITIAL_CAPACITY : usize = 128;

pub struct Receiver <T> {
  consumer    : std::cell::UnsafeCell <bounded_spsc_queue::Consumer <T>>,
  receive_new : std::sync::mpsc::Receiver <bounded_spsc_queue::Consumer <T>>,
  inner       : std::sync::Arc <Inner>,
  steals      : std::cell::UnsafeCell <isize>
}

pub struct Sender <T> {
  producer : std::cell::UnsafeCell <bounded_spsc_queue::Producer <T>>,
  send_new : std::sync::mpsc::Sender <bounded_spsc_queue::Consumer <T>>,
  inner    : std::sync::Arc <Inner>
}

struct Inner {
  counter   : std::sync::atomic::AtomicIsize,
  connected : std::sync::atomic::AtomicBool,
  to_wake   : std::sync::atomic::AtomicUsize
}

#[derive(Debug)]
pub struct Iter <'a, T : 'a> {
  rx : &'a Receiver <T>
}

#[derive(Debug)]
pub struct TryIter <'a, T : 'a> {
  rx : &'a Receiver <T>
}

#[derive(Debug)]
pub struct IntoIter <T> {
  rx : Receiver <T>
}

/// Sender disconnected, no further messages will ever be received.
#[derive(Clone,Copy,Debug,Eq,PartialEq)]
pub struct RecvError;

/// Receiver disconnected, message will never be deliverable.
#[derive(Clone,Copy,Eq,PartialEq)]
pub struct SendError <T> (pub T);

#[derive(Clone,Copy,Debug,Eq,PartialEq)]
pub enum TryRecvError {
  Empty,
  Disconnected
}

#[derive(Clone,Copy,Debug,Eq,PartialEq)]
pub enum RecvTimeoutError {
  Timeout,
  Disconnected
}

pub enum SelectionResult {
  SelSuccess,
  SelCanceled
}

impl <T> Receiver <T> {
  /// Non-blocking receive, returns `Err (TryRecvError::Empty)` if buffer was
  /// empty; will continue to receive pending messages from a disconnected
  /// channel until it is empty, at which point further calls to this function
  /// will return `Err (TryRecvError::Disconnected)`.
  pub fn try_recv (&self) -> Result <T, TryRecvError> {
    match unsafe { (*self.consumer.get()).try_pop() } {
      Some (t) => unsafe {
        if MAX_STEALS < *self.steals.get() {
          match self.inner.counter.swap (0, Ordering::SeqCst) {
            DISCONNECTED => {
              self.inner.counter.store (DISCONNECTED, Ordering::SeqCst);
            }
            n => {
              let m = std::cmp::min (n, *self.steals.get());
              *self.steals.get() -= m;
              self.bump (n - m);
            }
          }
          assert!(0 <= *self.steals.get());
        }
        *self.steals.get() += 1;
        Ok (t)
      },
      None => {
        match self.receive_new.try_recv() {
          Ok (new_consumer) => {
            unsafe { *self.consumer.get() = new_consumer; }
            self.try_recv()
          },
          Err (std::sync::mpsc::TryRecvError::Empty) => {
            match self.inner.counter.load (Ordering::SeqCst) {
              n if n != DISCONNECTED => Err (TryRecvError::Empty),
              _ => {
                match unsafe { (*self.consumer.get()).try_pop() } {
                  Some (t) => Ok (t),
                  None     => Err (TryRecvError::Disconnected)
                }
              }
            }
          },
          Err (std::sync::mpsc::TryRecvError::Disconnected) => {
            Err (TryRecvError::Disconnected)
          }
        }
      }
    }
  }

  /// Block waiting if no messages are pending in the buffer.
  pub fn recv (&self) -> Result <T, RecvError> {
    match self.try_recv() {
      Err (TryRecvError::Empty) => {}
      Err (TryRecvError::Disconnected) => return Err (RecvError),
      Ok  (t) => return Ok (t)
    }
    let (wait_token, signal_token) = blocking::tokens();
    if self.decrement (signal_token).is_ok() {
      wait_token.wait();
    }
    match self.try_recv() {
      Ok (t) => unsafe {
        *self.steals.get() -= 1;
        Ok (t)
      },
      Err (TryRecvError::Empty) => unreachable!(
        "woken thread should have found pending message"),
      Err (TryRecvError::Disconnected) => Err (RecvError)
    }
  }

  pub fn recv_timeout (&self, timeout : std::time::Duration)
    -> Result <T, RecvTimeoutError>
  {
    match self.try_recv() {
      Ok  (t)                          => Ok (t),
      Err (TryRecvError::Disconnected) => Err (RecvTimeoutError::Disconnected),
      Err (TryRecvError::Empty)
        => self.recv_max_until (std::time::Instant::now() + timeout)
    }
  }

  pub fn iter (&self) -> Iter <T> {
    Iter {
      rx: self
    }
  }

  pub fn try_iter (&self) -> TryIter <T> {
    TryIter {
      rx: self
    }
  }

  pub fn capacity (&self) -> usize {
    unsafe {
      (*self.consumer.get()).capacity()
    }
  }

  fn recv_max_until (&self, deadline : std::time::Instant)
    -> Result <T, RecvTimeoutError>
  {
    loop {
      match self.recv_deadline (deadline) {
        result @ Err (RecvTimeoutError::Timeout) => {
          if deadline <= std::time::Instant::now() {
            return result
          }
        },
        result => return result
      }
    }
  }

  /// This is the same as `recv` except with code for timeout.
  fn recv_deadline (&self, deadline : std::time::Instant)
    -> Result <T, RecvTimeoutError>
  {
    match self.try_recv() {
      Err (TryRecvError::Empty) => {}
      Err (TryRecvError::Disconnected)
        => return Err (RecvTimeoutError::Disconnected),
      Ok  (t) => return Ok (t)
    }
    let (wait_token, signal_token) = blocking::tokens();
    if self.decrement (signal_token).is_ok() {
      let timed_out = !wait_token.wait_max_until (deadline);
      if timed_out {
        // this boolean result is not used: `try_recv` is always called below
        let _has_data = self.abort_selection();
      }
    } else {
      wait_token.wait();
    }
    match self.try_recv() {
      Ok (t) => unsafe {
        *self.steals.get() -= 1;
        Ok (t)
      }
      Err (TryRecvError::Empty)        => Err (RecvTimeoutError::Timeout),
      Err (TryRecvError::Disconnected) => Err (RecvTimeoutError::Disconnected)
    }
  }

  fn decrement (&self, token : blocking::SignalToken)
    -> Result <(), blocking::SignalToken>
  {
    assert_eq!(self.inner.to_wake.load (Ordering::SeqCst), 0);
    let ptr = unsafe { token.cast_to_usize() };
    self.inner.to_wake.store (ptr, Ordering::SeqCst);
    let steals = unsafe { std::ptr::replace (self.steals.get(), 0) };
    match self.inner.counter.fetch_sub (1 + steals, Ordering::SeqCst) {
      DISCONNECTED => {
        self.inner.counter.store (DISCONNECTED, Ordering::SeqCst);
      }
      n => {
        assert!(0 <= n);
        if n - steals <= 0 {
          return Ok (())
        }
      }
    }
    self.inner.to_wake.store (0, Ordering::SeqCst);
    Err (unsafe { blocking::SignalToken::cast_from_usize (ptr) })
  }

  /////////////////////////////////////////////////////////////////////////////
  //  select functions
  /////////////////////////////////////////////////////////////////////////////

  pub fn can_recv (&self) -> bool {
    0 < unsafe { (*self.consumer.get()).size() }
  }

  pub fn start_selection (&self, token : blocking::SignalToken)
    -> SelectionResult
  {
    match self.decrement (token) {
      Ok  (()) => SelectionResult::SelSuccess,
      Err (_token) => {
        // undo decrement above
        let prev = self.bump (1);
        assert!(prev == DISCONNECTED || 0 <= prev);
        SelectionResult::SelCanceled
      }
    }
  }

  /// Returns true if receiver has data pending.
  fn abort_selection (&self) -> bool {
    let steals = 1;
    let prev = self.bump (steals + 1);
    let has_data = if prev == DISCONNECTED {
      assert_eq! (self.inner.to_wake.load (Ordering::SeqCst), 0);
      true
    } else {
      let cur = prev + steals + 1;
      assert!(0 <= cur);
      if prev < 0 {
        drop (self.inner.take_to_wake());
      } else {
        while self.inner.to_wake.load (Ordering::SeqCst) != 0 {
          std::thread::yield_now();
        }
      }
      unsafe {
        assert_eq!(*self.steals.get(), 0);
        *self.steals.get() = steals;
      }
      0 <= prev
    };
    has_data
  }

  fn bump (&self, amt : isize) -> isize {
    match self.inner.counter.fetch_add (amt, Ordering::SeqCst) {
      DISCONNECTED => {
        self.inner.counter.store (DISCONNECTED, Ordering::SeqCst);
        DISCONNECTED
      }
      n => n
    }
  }

} // end impl Receiver

impl <T> std::fmt::Debug for Receiver <T> {
  fn fmt (&self, f : &mut std::fmt::Formatter) -> std::fmt::Result {
    write!(f, "Receiver {{ .. }}")
  }
}

impl <T> IntoIterator for Receiver <T> {
  type Item = T;
  type IntoIter = IntoIter <T>;
  fn into_iter (self) -> IntoIter <T> {
    IntoIter {
      rx: self
    }
  }
}

impl <'a, T> IntoIterator for &'a Receiver <T> {
  type Item = T;
  type IntoIter = Iter <'a, T>;
  fn into_iter (self) -> Iter <'a, T> {
    self.iter()
  }
}

impl <T> Drop for Receiver <T> {
  fn drop (&mut self) {
    self.inner.connected.store (false, Ordering::SeqCst);

    // NOTE: The following code to clear the queue comes from the original
    // standard library MPSC stream-flavor drop function. Whether it is
    // necessary because of the linked-list queue used in that case, or rather
    // it is needed to ensure the synchronization with the sender is not known.
    // Besides the one-time overhead it shouldn't hurt so we will do it
    // regardless.
    // TODO: find out if this is required or we can just drop the queue
    let mut steals = unsafe { *self.steals.get() };
    while {
      let count = self.inner.counter.compare_and_swap (
        steals, DISCONNECTED, Ordering::SeqCst);
      count != DISCONNECTED && count != steals
    } {
      while let Some (_t) = unsafe { (*self.consumer.get()).try_pop() } {
        steals += 1;
      }
    }
  }
}

impl <T> Sender <T> {
  /// Non-blocking send.
  pub fn send (&self, t : T) -> Result <(), SendError <T>> {
    if self.inner.connected.load (Ordering::SeqCst) {
      match unsafe { (*self.producer.get()).try_push (t) } {
        None     => {}, // success
        Some (t) => {   // queue full
          let new_capacity = 2 * unsafe { (*self.producer.get()).capacity() };
          let (new_producer, new_consumer)
            = bounded_spsc_queue::make (new_capacity);
          // TODO: We are using a side channel here to send the new consumer
          // which was not part of the original standard library channel
          // implementation. Are we sure that this is safe to unwrap or should
          // we handle the result explicitly ?
          self.send_new.send (new_consumer).unwrap();
          unsafe { *self.producer.get() = new_producer; }
          match unsafe { (*self.producer.get()).try_push (t) } {
            None      => {}
            Some (_t) => unreachable!(
              "send on a newly created queue should always succeed")
          }
        }
      }
      match self.inner.counter.fetch_add (1, Ordering::SeqCst) {
        -1 => {
          self.inner.take_to_wake().signal();
        },
        -2 => {},
        DISCONNECTED => {
          self.inner.counter.store (DISCONNECTED, Ordering::SeqCst);
          // We want to guarantee if a message was not received that we get it
          // back; since bounded_spsc_queue::{Producer,Consumer} have the same
          // internal representation (as a singleton struct containing Arc
          // <Buffer <T>>), we can safely transmute the producer in order to
          // pop the message back if it was orphaned.
          unsafe {
            let consumer : bounded_spsc_queue::Consumer <T>
              = std::mem::transmute (self.producer.get());
            let first    = consumer.try_pop();
            let second   = consumer.try_pop();
            assert!(second.is_none());
            match first {
              Some (t) => return Err (SendError (t)),
              None     => {}
            }
          }
        },
        n => {
          assert! (0 <= n);
        }
      }
      Ok (())
    } else {
      Err (SendError (t))
    }
  }
} // end impl Sender

impl <T> std::fmt::Debug for Sender <T> {
  fn fmt (&self, f : &mut std::fmt::Formatter) -> std::fmt::Result {
    write!(f, "Sender {{ .. }}")
  }
}

impl <T> Drop for Sender <T> {
  fn drop (&mut self) {
    self.inner.connected.store (false, Ordering::SeqCst);
    match self.inner.counter.swap (DISCONNECTED, Ordering::SeqCst) {
      DISCONNECTED => {}
      -1 => {
        self.inner.take_to_wake().signal();
      }
      n  => {
        assert!(0 <= n);
      }
    }
  }
}

impl Inner {
  fn take_to_wake (&self) -> blocking::SignalToken {
    let ptr = self.to_wake.swap (0, Ordering::SeqCst);
    assert!(ptr != 0);
    unsafe {
      blocking::SignalToken::cast_from_usize (ptr)
    }
  }
}

impl <'a, T> Iterator for Iter <'a, T> {
  type Item = T;
  fn next (&mut self) -> Option <T> {
    self.rx.recv().ok()
  }
}

impl <'a, T> Iterator for TryIter <'a, T> {
  type Item = T;
  fn next (&mut self) -> Option <T> {
    self.rx.try_recv().ok()
  }
}

impl <T> Iterator for IntoIter <T> {
  type Item = T;
  fn next (&mut self) -> Option <T> {
    self.rx.recv().ok()
  }
}

impl std::fmt::Display for RecvError {
  fn fmt (&self, f : &mut std::fmt::Formatter) -> std::fmt::Result {
    "receiving on a closed cahnnel".fmt (f)
  }
}

impl std::error::Error for RecvError {
  fn description (&self) -> &str {
    "receiving on a closed cahnnel"
  }

  fn cause (&self) -> Option <&std::error::Error> {
    None
  }
}

impl <T> std::fmt::Debug for SendError <T> {
  fn fmt (&self, f : &mut std::fmt::Formatter) -> std::fmt::Result {
    "SendError(..)".fmt (f)
  }
}

impl <T> std::fmt::Display for SendError <T> {
  fn fmt (&self, f : &mut std::fmt::Formatter) -> std::fmt::Result {
    "sending on a closed cahnnel".fmt (f)
  }
}

impl <T : Send> std::error::Error for SendError <T> {
  fn description (&self) -> &str {
    "sending on a closed cahnnel"
  }

  fn cause (&self) -> Option <&std::error::Error> {
    None
  }
}

impl std::fmt::Display for TryRecvError {
  fn fmt (&self, f : &mut std::fmt::Formatter) -> std::fmt::Result {
    match *self {
      TryRecvError::Empty        => "receiving on an empty channel".fmt (f),
      TryRecvError::Disconnected => "receiving on a closed channel".fmt (f)
    }
  }
}

impl std::error::Error for TryRecvError {
  fn description (&self) -> &str {
    match *self {
      TryRecvError::Empty        => "receiving on an empty channel",
      TryRecvError::Disconnected => "receiving on a closed channel"
    }
  }

  fn cause (&self) -> Option <&std::error::Error> {
    None
  }
}

pub fn channel <T : 'static> () -> (Sender <T>, Receiver <T>) {
  if std::mem::size_of::<T>() == 0 {
    panic!("ERROR: 0-sized types are not supported");
  }
  let (producer, consumer) = bounded_spsc_queue::make (INITIAL_CAPACITY);
  let (send_new, receive_new) = std::sync::mpsc::channel();
  let inner = std::sync::Arc::new (
    Inner {
      counter:   std::sync::atomic::AtomicIsize::new (0),
      connected: std::sync::atomic::AtomicBool::new (true),
      to_wake:   std::sync::atomic::AtomicUsize::new (0)
    }
  );
  let sender    = Sender {
    producer: std::cell::UnsafeCell::new (producer),
    send_new,
    inner: inner.clone()
  };
  let receiver  = Receiver {
    consumer: std::cell::UnsafeCell::new (consumer),
    receive_new,
    steals: std::cell::UnsafeCell::new (0),
    inner
  };
  (sender, receiver)
}

#[cfg(test)]
mod tests {
  use super::*;

  pub fn stress_factor() -> usize {
    match std::env::var ("RUST_TEST_STRESS") {
      Ok  (val) => val.parse().unwrap(),
      Err (..)  => 1,
    }
  }

  #[test]
  fn smoke() {
    let (tx, rx) = channel::<i32>();
    tx.send (1).unwrap();
    assert_eq!(rx.recv().unwrap(), 1);
  }

  #[test]
  fn drop_full() {
    let (tx, _rx) = channel::<Box <isize>>();
    tx.send(box 1).unwrap();
  }

  #[test]
  fn smoke_threads() {
    let (tx, rx) = channel::<i32>();
    let _t = std::thread::spawn (move|| {
      tx.send (1).unwrap();
    });
    assert_eq!(rx.recv().unwrap(), 1);
  }

  #[test]
  fn smoke_port_gone() {
    let (tx, rx) = channel::<i32>();
    drop (rx);
    assert!(tx.send (1).is_err());
  }

  #[test]
  fn smoke_shared_port_gone() {
    let (tx, rx) = channel::<i32>();
    drop (rx);
    assert!(tx.send (1).is_err())
  }

  #[test]
  fn port_gone_concurrent() {
    let (tx, rx) = channel::<i32>();
    let _t = std::thread::spawn (move|| {
      rx.recv().unwrap();
    });
    while tx.send (1).is_ok() {}
  }

  #[test]
  fn smoke_chan_gone() {
    let (tx, rx) = channel::<i32>();
    drop (tx);
    assert!(rx.recv().is_err());
  }

  #[test]
  fn chan_gone_concurrent() {
    let (tx, rx) = channel::<i32>();
    let _t = std::thread::spawn (move|| {
      tx.send (1).unwrap();
      tx.send (1).unwrap();
    });
    while rx.recv().is_ok() {}
  }

  #[test]
  fn stress() {
    let (tx, rx) = channel::<i32>();
    let t = std::thread::spawn (move|| {
      for _ in 0..10000 { tx.send (1).unwrap(); }
    });
    for _ in 0..10000 {
      assert_eq!(rx.recv().unwrap(), 1);
    }
    t.join().ok().unwrap();
  }

  #[test]
  fn send_from_outside_runtime() {
    let (tx1, rx1) = channel::<bool>();
    let (tx2, rx2) = channel::<i32>();
    let t1 = std::thread::spawn (move|| {
      tx1.send (true).unwrap();
      for _ in 0..40 {
        assert_eq!(rx2.recv().unwrap(), 1);
      }
    });
    rx1.recv().unwrap();
    let t2 = std::thread::spawn (move|| {
      for _ in 0..40 {
        tx2.send (1).unwrap();
      }
    });
    t1.join().ok().unwrap();
    t2.join().ok().unwrap();
  }

  #[test]
  fn recv_from_outside_runtime() {
    let (tx, rx) = channel::<i32>();
    let t = std::thread::spawn (move|| {
      for _ in 0..40 {
        assert_eq!(rx.recv().unwrap(), 1);
      }
    });
    for _ in 0..40 {
      tx.send (1).unwrap();
    }
    t.join().ok().unwrap();
  }

  #[test]
  fn no_runtime() {
    let (tx1, rx1) = channel::<i32>();
    let (tx2, rx2) = channel::<i32>();
    let t1 = std::thread::spawn (move|| {
      assert_eq!(rx1.recv().unwrap(), 1);
      tx2.send (2).unwrap();
    });
    let t2 = std::thread::spawn (move|| {
      tx1.send (1).unwrap();
      assert_eq!(rx2.recv().unwrap(), 2);
    });
    t1.join().ok().unwrap();
    t2.join().ok().unwrap();
  }

  #[test]
  fn oneshot_single_thread_close_port_first() {
    // Simple test of closing without sending
    let (_tx, rx) = channel::<i32>();
    drop (rx);
  }

  #[test]
  fn oneshot_single_thread_close_chan_first() {
    // Simple test of closing without sending
    let (tx, _rx) = channel::<i32>();
    drop (tx);
  }

  #[test]
  fn oneshot_single_thread_send_port_close() {
    // Testing that the sender cleans up the payload if receiver is closed
    let (tx, rx) = channel::<Box <i32>>();
    drop (rx);
    assert!(tx.send (box 0).is_err());
  }

  #[test]
  fn oneshot_single_thread_recv_chan_close() {
    // Receiving on a closed chan will panic
    let res = std::thread::spawn (move|| {
      let (tx, rx) = channel::<i32>();
      drop (tx);
      rx.recv().unwrap();
    }).join();
    // What is our res?
    assert!(res.is_err());
  }

  #[test]
  fn oneshot_single_thread_send_then_recv() {
    let (tx, rx) = channel::<Box <i32>>();
    tx.send (box 10).unwrap();
    assert!(*rx.recv().unwrap() == 10);
  }

  #[test]
  fn oneshot_single_thread_try_send_open() {
    let (tx, rx) = channel::<i32>();
    assert!(tx.send (10).is_ok());
    assert!(rx.recv().unwrap() == 10);
  }

  #[test]
  fn oneshot_single_thread_try_send_closed() {
    let (tx, rx) = channel::<i32>();
    drop (rx);
    assert!(tx.send (10).is_err());
  }

  #[test]
  fn oneshot_single_thread_try_recv_open() {
    let (tx, rx) = channel::<i32>();
    tx.send (10).unwrap();
    assert!(rx.recv() == Ok (10));
  }

  #[test]
  fn oneshot_single_thread_try_recv_closed() {
    let (tx, rx) = channel::<i32>();
    drop (tx);
    assert!(rx.recv().is_err());
  }

  #[test]
  fn oneshot_single_thread_peek_data() {
    let (tx, rx) = channel::<i32>();
    assert_eq!(rx.try_recv(), Err (TryRecvError::Empty));
    tx.send (10).unwrap();
    assert_eq!(rx.try_recv(), Ok (10));
  }

  #[test]
  fn oneshot_single_thread_peek_close() {
    let (tx, rx) = channel::<i32>();
    drop (tx);
    assert_eq!(rx.try_recv(), Err (TryRecvError::Disconnected));
    assert_eq!(rx.try_recv(), Err (TryRecvError::Disconnected));
  }

  #[test]
  fn oneshot_single_thread_peek_open() {
    let (_tx, rx) = channel::<i32>();
    assert_eq!(rx.try_recv(), Err (TryRecvError::Empty));
  }

  #[test]
  fn oneshot_multi_task_recv_then_send () {
    let (tx, rx) = channel::<Box <i32>>();
    let _t = std::thread::spawn (move|| {
      assert!(*rx.recv().unwrap() == 10);
    });

    tx.send (box 10).unwrap();
  }

  #[test]
  fn oneshot_multi_task_recv_then_close() {
    let (tx, rx) = channel::<Box <i32>>();
    let _t = std::thread::spawn (move|| {
      drop (tx);
    });
    let res = std::thread::spawn (move|| {
      assert!(*rx.recv().unwrap() == 10);
    }).join();
    assert!(res.is_err());
  }

  #[test]
  fn oneshot_multi_thread_close_stress() {
    for _ in 0..stress_factor() {
      let (tx, rx) = channel::<i32>();
      let _t = std::thread::spawn (move|| {
        drop (rx);
      });
      drop (tx);
    }
  }

  #[test]
  fn oneshot_multi_thread_send_close_stress() {
    for _ in 0..stress_factor() {
      let (tx, rx) = channel::<i32>();
      let _t = std::thread::spawn (move|| {
        drop (rx);
      });
      let _ = std::thread::spawn (move|| {
        tx.send (1).unwrap();
      }).join();
    }
  }

  #[test]
  fn oneshot_multi_thread_recv_close_stress() {
    for _ in 0..stress_factor() {
      let (tx, rx) = channel::<i32>();
      std::thread::spawn (move|| {
        let res = std::thread::spawn (move|| {
          rx.recv().unwrap();
        }).join();
        assert!(res.is_err());
      });
      let _t = std::thread::spawn (move|| {
        std::thread::spawn (move|| {
          drop (tx);
        });
      });
    }
  }

  #[test]
  fn oneshot_multi_thread_send_recv_stress() {
    for _ in 0..stress_factor() {
      let (tx, rx) = channel::<Box <isize>>();
      let _t = std::thread::spawn (move|| {
        tx.send (box 10).unwrap();
      });
      assert!(*rx.recv().unwrap() == 10);
    }
  }

  #[test]
  fn stream_send_recv_stress() {
    for _ in 0..stress_factor() {
      let (tx, rx) = channel();

      send (tx, 0);
      recv (rx, 0);

      fn send (tx: Sender<Box <i32>>, i: i32) {
        if i == 10 { return }

        std::thread::spawn (move|| {
          tx.send (box i).unwrap();
          send (tx, i + 1);
        });
      }

      fn recv (rx: Receiver<Box <i32>>, i: i32) {
        if i == 10 { return }

        std::thread::spawn (move|| {
          assert!(*rx.recv().unwrap() == i);
          recv (rx, i + 1);
        });
      }
    }
  }

  #[test]
  fn oneshot_single_thread_recv_timeout() {
    let (tx, rx) = channel();
    tx.send (true).unwrap();
    assert_eq!(rx.recv_timeout (std::time::Duration::from_millis (1)), Ok (true));
    assert_eq!(rx.recv_timeout (std::time::Duration::from_millis (1)),
      Err (RecvTimeoutError::Timeout));
    tx.send (true).unwrap();
    assert_eq!(rx.recv_timeout (std::time::Duration::from_millis (1)), Ok (true));
  }

  // FIXME: will sometimes run indefinitely
  #[test]
  fn stress_recv_timeout_two_threads() {
    let (tx, rx) = channel();
    let stress = stress_factor() + 100;
    let timeout = std::time::Duration::from_millis (100);

    std::thread::spawn (move || {
      for i in 0..stress {
        if i % 2 == 0 {
          std::thread::sleep(timeout * 2);
        }
        tx.send (1usize).unwrap();
      }
    });

    let mut recv_count = 0;
    loop {
      match rx.recv_timeout (timeout) {
        Ok (n) => {
          assert_eq!(n, 1usize);
          recv_count += 1;
        }
        Err (RecvTimeoutError::Timeout) => continue,
        Err (RecvTimeoutError::Disconnected) => break,
      }
    }

    assert_eq!(recv_count, stress);
  }

  #[test]
  fn recv_a_lot() {
    // Regression test that we don't run out of stack in scheduler context
    let (tx, rx) = channel();
    for _ in 0..10000 { tx.send (true).unwrap(); }
    for _ in 0..10000 { rx.recv().unwrap(); }
  }

  #[test]
  fn test_nested_recv_iter() {
    let (tx, rx) = channel::<i32>();
    let (total_tx, total_rx) = channel::<i32>();

    let _t = std::thread::spawn (move|| {
      let mut acc = 0;
      for x in rx.iter() {
        acc += x;
      }
      total_tx.send (acc).unwrap();
    });

    tx.send (3).unwrap();
    tx.send (1).unwrap();
    tx.send (2).unwrap();
    drop (tx);
    assert_eq!(total_rx.recv().unwrap(), 6);
  }

  #[test]
  fn test_recv_iter_break() {
    let (tx, rx) = channel::<i32>();
    let (count_tx, count_rx) = channel();

    let _t = std::thread::spawn (move|| {
      let mut count = 0;
      for x in rx.iter() {
        if count >= 3 {
          break;
        } else {
          count += x;
        }
      }
      count_tx.send (count).unwrap();
    });

    tx.send (2).unwrap();
    tx.send (2).unwrap();
    tx.send (2).unwrap();
    let _ = tx.send (2);
    drop (tx);
    assert_eq!(count_rx.recv().unwrap(), 4);
  }

  #[test]
  fn test_recv_try_iter() {
    let (request_tx, request_rx) = channel();
    let (response_tx, response_rx) = channel();

    // Request `x`s until we have `6`.
    let t = std::thread::spawn (move|| {
      let mut count = 0;
      loop {
        for x in response_rx.try_iter() {
          count += x;
          if count == 6 {
            return count;
          }
        }
        request_tx.send (true).unwrap();
      }
    });

    for _ in request_rx.iter() {
      if response_tx.send (2).is_err() {
        break;
      }
    }

    assert_eq!(t.join().unwrap(), 6);
  }

  #[test]
  fn test_recv_into_iter_owned() {
    let mut iter = {
      let (tx, rx) = channel::<i32>();
      tx.send (1).unwrap();
      tx.send (2).unwrap();

      rx.into_iter()
    };
    assert_eq!(iter.next().unwrap(), 1);
    assert_eq!(iter.next().unwrap(), 2);
    assert_eq!(iter.next().is_none(), true);
  }

  #[test]
  fn test_recv_into_iter_borrowed() {
    let (tx, rx) = channel::<i32>();
    tx.send (1).unwrap();
    tx.send (2).unwrap();
    drop (tx);
    let mut iter = (&rx).into_iter();
    assert_eq!(iter.next().unwrap(), 1);
    assert_eq!(iter.next().unwrap(), 2);
    assert_eq!(iter.next().is_none(), true);
  }

  #[test]
  fn try_recv_states() {
    let (tx1, rx1) = channel::<i32>();
    let (tx2, rx2) = channel::<bool>();
    let (tx3, rx3) = channel::<bool>();
    let _t = std::thread::spawn (move|| {
      rx2.recv().unwrap();
      tx1.send (1).unwrap();
      tx3.send (true).unwrap();
      rx2.recv().unwrap();
      drop (tx1);
      tx3.send (true).unwrap();
    });

    assert_eq!(rx1.try_recv(), Err (TryRecvError::Empty));
    tx2.send (true).unwrap();
    rx3.recv().unwrap();
    assert_eq!(rx1.try_recv(), Ok (1));
    assert_eq!(rx1.try_recv(), Err (TryRecvError::Empty));
    tx2.send (true).unwrap();
    rx3.recv().unwrap();
    assert_eq!(rx1.try_recv(), Err (TryRecvError::Disconnected));
  }

  #[test]
  fn issue_32114() {
    let (tx, _) = channel();
    let _ = tx.send (123);
    assert_eq!(tx.send (123), Err (SendError (123)));
  }
}
