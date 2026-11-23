//! Bounded ownership transfer with exactly one producer and one consumer.
//!
//! Construction allocates; push/pop do not allocate, lock, or destroy values.
//! A full queue returns ownership to the producer. Endpoint destruction and
//! dropping popped/rejected values are the caller's responsibility: endpoints
//! carrying audio resources must be torn down off the real-time thread.
//!
//! Endpoints cannot be cloned or shared for concurrent use:
//! ```compile_fail
//! let (producer, _) = patch_core::spsc::channel::<u32>(8);
//! let second_producer = producer.clone();
//! ```
//! ```compile_fail
//! let (producer, _) = patch_core::spsc::channel::<u32>(8);
//! std::thread::scope(|scope| {
//!     scope.spawn(|| { let _ = &producer; });
//! });
//! ```
//! ```compile_fail
//! let (_, consumer) = patch_core::spsc::channel::<u32>(8);
//! std::thread::scope(|scope| {
//!     scope.spawn(|| { let _ = &consumer; });
//! });
//! ```

use std::cell::{Cell, UnsafeCell};
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Shared<T> {
    slots: Box<[UnsafeCell<MaybeUninit<T>>]>,
    head: AtomicUsize,
    tail: AtomicUsize,
}

// SAFETY: slots are private and accessed only through the two unique endpoints.
// The producer writes a free slot then publishes head with Release; the
// consumer reads only published slots after Acquire, and releases tail only
// after moving out the value. Exclusive endpoint access prevents competing
// writers/readers. Moving a value between endpoints requires T: Send.
unsafe impl<T: Send> Sync for Shared<T> {}
unsafe impl<T: Send> Send for Shared<T> {}

impl<T> Drop for Shared<T> {
    fn drop(&mut self) {
        // Both endpoints have gone: no other slot access remains possible.
        let head = *self.head.get_mut();
        let mut tail = *self.tail.get_mut();
        while tail != head {
            let index = tail & (self.slots.len() - 1);
            // SAFETY: precisely the published, unconsumed slots are initialized.
            unsafe { self.slots[index].get_mut().assume_init_drop() };
            tail = tail.wrapping_add(1);
        }
    }
}

/// The only writer. Sendable when T is Send, deliberately not Sync or Clone.
pub struct Producer<T> {
    shared: Arc<Shared<T>>,
    exclusive: PhantomData<Cell<()>>,
}

/// The only reader. Sendable when T is Send, deliberately not Sync or Clone.
pub struct Consumer<T> {
    shared: Arc<Shared<T>>,
    exclusive: PhantomData<Cell<()>>,
}

/// Allocate a queue, rounding capacity up to a power of two (minimum two).
/// Panics if the requested capacity cannot be represented or allocated.
pub fn channel<T>(capacity: usize) -> (Producer<T>, Consumer<T>) {
    let capacity = capacity
        .max(2)
        .checked_next_power_of_two()
        .expect("queue capacity overflow");
    assert!(capacity <= isize::MAX as usize, "queue capacity too large");
    let shared = Arc::new(Shared {
        slots: (0..capacity)
            .map(|_| UnsafeCell::new(MaybeUninit::uninit()))
            .collect(),
        head: AtomicUsize::new(0),
        tail: AtomicUsize::new(0),
    });
    (
        Producer {
            shared: shared.clone(),
            exclusive: PhantomData,
        },
        Consumer {
            shared,
            exclusive: PhantomData,
        },
    )
}

impl<T> Producer<T> {
    pub fn capacity(&self) -> usize {
        self.shared.slots.len()
    }

    /// Available slots as observed by the sole producer. The consumer may
    /// make additional slots available immediately after this snapshot.
    pub fn remaining(&self) -> usize {
        let head = self.shared.head.load(Ordering::Relaxed);
        let tail = self.shared.tail.load(Ordering::Acquire);
        self.capacity() - head.wrapping_sub(tail)
    }

    /// Publish one item, or return it untouched when the queue is full.
    /// The queue has no disconnect notification; dropping the consumer does
    /// not change this contract. Owners coordinate shutdown separately.
    pub fn push(&mut self, value: T) -> Result<(), T> {
        let head = self.shared.head.load(Ordering::Relaxed);
        let tail = self.shared.tail.load(Ordering::Acquire);
        if head.wrapping_sub(tail) == self.capacity() {
            return Err(value);
        }
        let index = head & (self.capacity() - 1);
        // SAFETY: the unique producer owns this free slot until publication.
        unsafe { (*self.shared.slots[index].get()).write(value) };
        self.shared
            .head
            .store(head.wrapping_add(1), Ordering::Release);
        Ok(())
    }
}

impl<T> Consumer<T> {
    pub fn capacity(&self) -> usize {
        self.shared.slots.len()
    }

    /// Published items as observed by the sole consumer.
    pub fn len(&self) -> usize {
        let tail = self.shared.tail.load(Ordering::Relaxed);
        self.shared.head.load(Ordering::Acquire).wrapping_sub(tail)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Move out the next item. No value is destroyed by this operation.
    pub fn pop(&mut self) -> Option<T> {
        let tail = self.shared.tail.load(Ordering::Relaxed);
        if tail == self.shared.head.load(Ordering::Acquire) {
            return None;
        }
        let index = tail & (self.capacity() - 1);
        // SAFETY: Acquire observed publication; tail is not released until
        // after the move, so the producer cannot rewrite this slot meanwhile.
        let value = unsafe { (*self.shared.slots[index].get()).assume_init_read() };
        self.shared
            .tail
            .store(tail.wrapping_add(1), Ordering::Release);
        Some(value)
    }

    /// Inspect the front without consuming it, e.g. a future timed event.
    /// The mutable borrow keeps pop and any other consumer access exclusive.
    pub fn peek(&mut self) -> Option<&T> {
        let tail = self.shared.tail.load(Ordering::Relaxed);
        if tail == self.shared.head.load(Ordering::Acquire) {
            return None;
        }
        let index = tail & (self.capacity() - 1);
        // SAFETY: the consumer retains tail and its exclusive borrow for the
        // lifetime of this reference, so the slot remains initialized/owned.
        Some(unsafe { (*self.shared.slots[index].get()).assume_init_ref() })
    }
}

impl<T: Copy> Producer<T> {
    /// Publish a whole slice atomically, or leave the queue unchanged.
    pub fn push_slice(&mut self, values: &[T]) -> bool {
        if values.len() > self.remaining() {
            return false;
        }
        let head = self.shared.head.load(Ordering::Relaxed);
        let start = head & (self.capacity() - 1);
        let first = values.len().min(self.capacity() - start);
        // SAFETY: remaining() established enough free slots, which only this
        // producer may fill. The consumer can only increase that space.
        // UnsafeCell<MaybeUninit<T>> has T's layout; the two spans stay inside
        // the allocation. T: Copy means overwriting consumed slots drops none.
        unsafe {
            std::ptr::copy_nonoverlapping(
                values.as_ptr(),
                self.shared.slots[start].get().cast::<T>(),
                first,
            );
            if first < values.len() {
                std::ptr::copy_nonoverlapping(
                    values.as_ptr().add(first),
                    self.shared.slots[0].get().cast::<T>(),
                    values.len() - first,
                );
            }
        }
        self.shared
            .head
            .store(head.wrapping_add(values.len()), Ordering::Release);
        true
    }
}

impl<T: Copy> Consumer<T> {
    /// Copy available items into the caller's buffer; return the count.
    pub fn pop_into(&mut self, output: &mut [T]) -> usize {
        let count = output.len().min(self.len());
        let tail = self.shared.tail.load(Ordering::Relaxed);
        let start = tail & (self.capacity() - 1);
        let first = count.min(self.capacity() - start);
        // SAFETY: len() acquired publication. This consumer owns all copied
        // slots until releasing tail below; output cannot alias private slots.
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.shared.slots[start].get().cast::<T>(),
                output.as_mut_ptr(),
                first,
            );
            if first < count {
                std::ptr::copy_nonoverlapping(
                    self.shared.slots[0].get().cast::<T>(),
                    output.as_mut_ptr().add(first),
                    count - first,
                );
            }
        }
        self.shared
            .tail
            .store(tail.wrapping_add(count), Ordering::Release);
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn bounded_fifo_peek_and_wraparound() {
        let (mut p, mut c) = channel(3);
        assert_eq!(p.capacity(), 4);
        assert_eq!(c.capacity(), 4);
        // Exercise integer wrap, not just slot wrap, in a quiescent queue.
        p.shared.head.store(usize::MAX - 1, Ordering::Relaxed);
        p.shared.tail.store(usize::MAX - 1, Ordering::Relaxed);
        for i in 0..4 {
            assert_eq!(p.push(i), Ok(()));
        }
        assert_eq!(p.push(99), Err(99));
        assert_eq!(c.peek(), Some(&0));
        assert_eq!(c.peek(), Some(&0));
        for i in 0..4 {
            assert_eq!(c.pop(), Some(i));
        }
        assert_eq!(c.pop(), None);
        assert_eq!(c.peek(), None);
        assert_eq!(p.push(42), Ok(()));
        assert_eq!(c.pop(), Some(42));
    }

    #[test]
    fn values_are_returned_or_dropped_exactly_once() {
        struct Value(Arc<AtomicUsize>);
        impl Drop for Value {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        let drops = Arc::new(AtomicUsize::new(0));
        let (mut p, mut c) = channel(2);
        assert!(p.push(Value(drops.clone())).is_ok());
        assert!(p.push(Value(drops.clone())).is_ok());
        let rejected = p.push(Value(drops.clone())).err().unwrap();
        assert_eq!(drops.load(Ordering::Relaxed), 0);
        drop(rejected);
        let popped = c.pop().unwrap();
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        drop(popped);
        drop(p);
        assert_eq!(drops.load(Ordering::Relaxed), 2);
        drop(c);
        assert_eq!(drops.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn bulk_transfer_wraps_and_overflow_is_all_or_nothing() {
        let (mut p, mut c) = channel(8);
        assert!(p.push_slice(&[0, 1, 2, 3, 4, 5]));
        let mut first = [0; 5];
        assert_eq!(c.pop_into(&mut first), 5);
        assert_eq!(first, [0, 1, 2, 3, 4]);
        assert!(p.push_slice(&[6, 7, 8, 9, 10, 11, 12]));
        assert_eq!(p.remaining(), 0);
        assert!(!p.push_slice(&[99]));
        assert!(p.push_slice(&[]));
        assert_eq!(c.pop_into(&mut []), 0);
        let mut all = [0; 10];
        assert_eq!(c.pop_into(&mut all), 8);
        assert_eq!(&all[..8], &[5, 6, 7, 8, 9, 10, 11, 12]);
        assert!(c.is_empty());
        assert_eq!(p.remaining(), 8);
    }

    #[test]
    fn transfers_non_copy_values_between_threads_in_order() {
        let (mut p, mut c) = channel(32);
        let worker = std::thread::spawn(move || {
            for i in 0..10_000 {
                let mut value = Box::new(i);
                loop {
                    match p.push(value) {
                        Ok(()) => break,
                        Err(back) => {
                            value = back;
                            std::thread::yield_now();
                        }
                    }
                }
            }
        });
        for expected in 0..10_000 {
            let value = loop {
                if let Some(value) = c.pop() {
                    break value;
                }
                std::thread::yield_now();
            };
            assert_eq!(*value, expected);
        }
        worker.join().unwrap();
        assert!(c.pop().is_none());
    }
}
