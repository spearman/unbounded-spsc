//! `send/try_recv` 10,000,000 messages in sequence:
//!
//! ~25ns per send, ~35ns per `try_recv`

extern crate unbounded_spsc;

const MESSAGE_COUNT : u64 = 10_000_000;

#[derive(Debug,PartialEq)]
struct Mystruct {
  x : f64,
  y : f64,
  z : f64
}

fn sendfun (sender : unbounded_spsc::Sender <Mystruct>) {
  let mut counter = 0;
  let start_time = std::time::SystemTime::now();
  while counter < MESSAGE_COUNT {
    sender.send (Mystruct { x: counter as f64, y: 1.5, z: 2.0 }).unwrap();
    counter += 1;
  }
  let duration = start_time.elapsed().unwrap();
  let duration_ns
    = (duration.as_secs() * 1_000_000_000) + duration.subsec_nanos() as u64;
  println!("sendfun duration ns: {duration_ns}");
  println!("sendfun ns per message: {}", duration_ns / MESSAGE_COUNT);
}

fn recvfun (receiver : unbounded_spsc::Receiver <Mystruct>) {
  let start_time = std::time::SystemTime::now();
  loop {
    match receiver.try_recv() {
      Ok  (_m) => (),
      Err (unbounded_spsc::TryRecvError::Empty) => (),
      Err (unbounded_spsc::TryRecvError::Disconnected) => break
    }
  }
  let duration = start_time.elapsed().unwrap();
  let duration_ns
    = (duration.as_secs() * 1_000_000_000) + duration.subsec_nanos() as u64;
  println!("recvfun duration ns: {duration_ns}");
  println!("recvfun ns per message: {}", duration_ns / MESSAGE_COUNT);
  println!("buffer ending capacity: {}", receiver.capacity());
}

fn main() {
  println!("main...");
  let (sender, receiver) = unbounded_spsc::channel();
  let join_sender = std::thread::spawn (move || sendfun (sender));
  join_sender.join().unwrap();
  let join_receiver = std::thread::spawn (move || recvfun (receiver));
  join_receiver.join().unwrap();
  println!("...main");
}
