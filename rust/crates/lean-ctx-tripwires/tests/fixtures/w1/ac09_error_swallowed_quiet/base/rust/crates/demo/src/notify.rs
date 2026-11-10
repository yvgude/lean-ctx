use std::sync::mpsc::Sender;

pub fn notify(tx: &Sender<u32>) {
    let _ = tx.send(1);
}

pub fn total(s: &str) -> Option<u32> {
    let n: u32 = s.parse().ok().unwrap_or(0);
    Some(n)
}
