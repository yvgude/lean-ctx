//! Bounded queues for kernel HA.

use std::collections::VecDeque;

/// A first-in, first-out queue with a fixed upper bound.
#[derive(Debug, Clone)]
pub struct BoundedQueue<T> {
    items: VecDeque<T>,
    max_size: usize,
}

impl<T> BoundedQueue<T> {
    /// Creates an empty queue that retains at most `max_size` items.
    pub fn new(max_size: usize) -> Self {
        Self {
            items: VecDeque::with_capacity(max_size),
            max_size,
        }
    }

    /// Appends an item and returns the oldest item when capacity is exceeded.
    pub fn push(&mut self, item: T) -> Option<T> {
        if self.max_size == 0 {
            return Some(item);
        }

        let evicted = if self.is_full() {
            self.items.pop_front()
        } else {
            None
        };
        self.items.push_back(item);
        evicted
    }

    /// Returns the number of retained items.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Returns whether the queue contains no items.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Returns whether the queue has reached its configured capacity.
    pub fn is_full(&self) -> bool {
        self.items.len() >= self.max_size
    }

    /// Iterates over retained items from oldest to newest.
    pub fn iter(&self) -> impl Iterator<Item = &T> {
        self.items.iter()
    }

    /// Removes and returns up to `n` oldest items.
    pub fn drain_oldest(&mut self, n: usize) -> Vec<T> {
        let count = n.min(self.items.len());
        self.items.drain(..count).collect()
    }
}

#[cfg(test)]
pub mod tests {
    use super::BoundedQueue;

    #[test]
    fn bounded_queue_evicts_oldest() {
        let mut queue = BoundedQueue::new(2);

        assert_eq!(queue.push(1), None);
        assert_eq!(queue.push(2), None);
        assert_eq!(queue.push(3), Some(1));
        assert_eq!(queue.iter().copied().collect::<Vec<i32>>(), vec![2, 3]);
        assert!(queue.is_full());
    }

    #[test]
    fn bounded_queue_drain() {
        let mut queue = BoundedQueue::new(4);
        queue.push("first");
        queue.push("second");
        queue.push("third");

        assert_eq!(queue.drain_oldest(2), vec!["first", "second"]);
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.drain_oldest(5), vec!["third"]);
        assert!(queue.is_empty());
    }

    #[test]
    fn bounded_queue_with_zero_capacity_rejects_items() {
        let mut queue = BoundedQueue::new(0);

        assert_eq!(queue.push(7), Some(7));
        assert!(queue.is_empty());
        assert!(queue.is_full());
    }
}
