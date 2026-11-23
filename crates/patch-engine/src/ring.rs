//! Single-producer / single-consumer ring for the real-time thread.
//!
//! Preallocated; `push` never allocates, locks or blocks. When the reader
//! falls behind, the writer drops new items (and counts them) rather than
//! waiting: the RT thread must never stall for a meter.
//!
//! The ring is shared (`&self`), so the one-producer, one-consumer rule is
//! the caller's to keep: the producer methods are `unsafe` (one thread at a
//! time may produce) and so are the consumer methods (one at a time may
//! consume). Each call site says which thread owns that side.

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

pub struct Ring<T: Copy> {
    slots: Box<[UnsafeCell<T>]>,
    mask: usize,
    /// Next slot the writer fills (monotonic).
    head: AtomicUsize,
    /// Next slot the reader takes (monotonic).
    tail: AtomicUsize,
    pub dropped: AtomicU64,
}

// SAFETY: one producer writes slots in [tail, head+cap) only after checking
// space; one consumer reads slots in [tail, head) only after the producer's
// release store of `head`. Slots are never accessed by both at once.
unsafe impl<T: Copy + Send> Sync for Ring<T> {}
unsafe impl<T: Copy + Send> Send for Ring<T> {}

impl<T: Copy> Ring<T> {
    /// `capacity` is rounded up to a power of two.
    pub fn new(capacity: usize, fill: T) -> Self {
        let cap = capacity.next_power_of_two().max(2);
        Ring {
            slots: (0..cap).map(|_| UnsafeCell::new(fill)).collect(),
            mask: cap - 1,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            dropped: AtomicU64::new(0),
        }
    }

    pub fn capacity(&self) -> usize {
        self.mask + 1
    }

    /// Producer side. Returns false (and counts a drop) when full.
    ///
    /// # Safety
    /// No other thread may be producing at the same time.
    pub unsafe fn push(&self, v: T) -> bool {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        if head - tail > self.mask {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        // SAFETY: the slot is outside [tail, head), so the reader isn't on it.
        unsafe { *self.slots[head & self.mask].get() = v };
        self.head.store(head + 1, Ordering::Release);
        true
    }

    /// Producer side: push a whole slice, or nothing when it doesn't fit.
    ///
    /// # Safety
    /// No other thread may be producing at the same time.
    pub unsafe fn push_slice(&self, vs: &[T]) -> bool {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        if head - tail + vs.len() > self.capacity() {
            self.dropped.fetch_add(vs.len() as u64, Ordering::Relaxed);
            return false;
        }
        // Tiny writes are faster inline than through a general bulk copy.
        if vs.len() <= 32 {
            for (i, v) in vs.iter().enumerate() {
                // SAFETY: all these slots are free, as checked above.
                unsafe { *self.slots[(head + i) & self.mask].get() = *v };
            }
            self.head.store(head + vs.len(), Ordering::Release);
            return true;
        }
        let start = head & self.mask;
        let first = vs.len().min(self.capacity() - start);
        // SAFETY: all destination slots are free, source cannot alias the
        // private ring storage, and UnsafeCell<T> has T's layout. Split at
        // the ring boundary so both copies remain within the allocation.
        unsafe {
            std::ptr::copy_nonoverlapping(vs.as_ptr(), self.slots[start].get(), first);
            if first < vs.len() {
                std::ptr::copy_nonoverlapping(
                    vs.as_ptr().add(first),
                    self.slots[0].get(),
                    vs.len() - first,
                );
            }
        }
        self.head.store(head + vs.len(), Ordering::Release);
        true
    }

    /// Items waiting (consumer side; a lower bound for the producer).
    pub fn len(&self) -> usize {
        self.head.load(Ordering::Acquire) - self.tail.load(Ordering::Acquire)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Room left (producer side).
    pub fn free(&self) -> usize {
        self.capacity() - self.len()
    }

    /// Consumer side: take one item.
    ///
    /// # Safety
    /// No other thread may be consuming at the same time.
    pub unsafe fn pop(&self) -> Option<T> {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        if tail == head {
            return None;
        }
        // SAFETY: published slot, not rewritten until tail moves past it.
        let v = unsafe { *self.slots[tail & self.mask].get() };
        self.tail.store(tail + 1, Ordering::Release);
        Some(v)
    }

    /// Consumer side: fill `out` from the front; returns items taken.
    ///
    /// # Safety
    /// No other thread may be consuming at the same time.
    pub unsafe fn pop_into(&self, out: &mut [T]) -> usize {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        let n = (head - tail).min(out.len());
        for (i, o) in out.iter_mut().enumerate().take(n) {
            // SAFETY: as in `pop`.
            *o = unsafe { *self.slots[(tail + i) & self.mask].get() };
        }
        self.tail.store(tail + n, Ordering::Release);
        n
    }

    /// Consumer side: drop up to `n` items.
    ///
    /// # Safety
    /// No other thread may be consuming at the same time.
    pub unsafe fn skip(&self, n: usize) -> usize {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        let k = (head - tail).min(n);
        self.tail.store(tail + k, Ordering::Release);
        k
    }

    /// Consumer side: take everything available, in order.
    ///
    /// # Safety
    /// No other thread may be consuming at the same time.
    pub unsafe fn drain(&self, mut f: impl FnMut(T)) -> usize {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        for i in tail..head {
            // SAFETY: slots in [tail, head) are published and not rewritten
            // until `tail` moves past them.
            f(unsafe { *self.slots[i & self.mask].get() });
        }
        self.tail.store(head, Ordering::Release);
        head - tail
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bulk_slices_wrap_preserve_order() {
        let r = Ring::new(128, 0u32);
        assert!(unsafe { r.push_slice(&[0; 100]) });
        assert_eq!(unsafe { r.skip(100) }, 100);
        let input: Vec<u32> = (0..100).collect();
        assert!(unsafe { r.push_slice(&input) });
        let mut got = vec![0; 100];
        assert_eq!(unsafe { r.pop_into(&mut got) }, 100);
        assert_eq!(got, input);
    }

    #[test]
    fn slices_wrap_and_failed_push_keeps_existing_data() {
        let r = Ring::new(8, 0u32);
        assert!(unsafe { r.push_slice(&[]) });
        assert!(unsafe { r.push_slice(&[1, 2, 3, 4, 5, 6]) });
        assert_eq!(unsafe { r.skip(5) }, 5);
        assert!(unsafe { r.push_slice(&[7, 8, 9, 10, 11, 12, 13]) });
        assert!(!unsafe { r.push_slice(&[99]) });
        let mut got = [0; 8];
        assert_eq!(unsafe { r.pop_into(&mut got) }, 8);
        assert_eq!(got, [6, 7, 8, 9, 10, 11, 12, 13]);
        assert_eq!(r.dropped.load(Ordering::Relaxed), 1);
        assert!(unsafe { r.push_slice(&[]) });
    }

    #[test]
    fn order_capacity_and_drops() {
        let r = Ring::new(5, 0u32); // rounds to 8
        assert_eq!(r.capacity(), 8);
        for i in 0..8 {
            assert!(unsafe { r.push(i) });
        }
        assert!(!unsafe { r.push(99) }, "full");
        assert_eq!(r.dropped.load(Ordering::Relaxed), 1);
        let mut got = vec![];
        assert_eq!(unsafe { r.drain(|v| got.push(v)) }, 8);
        assert_eq!(got, (0..8).collect::<Vec<_>>());
        assert!(unsafe { r.push_slice(&[1, 2, 3]) });
        assert!(!unsafe { r.push_slice(&[0; 6]) }, "3 + 6 > 8");
        let mut got = vec![];
        unsafe { r.drain(|v| got.push(v)) };
        assert_eq!(got, [1, 2, 3]);
    }

    #[test]
    fn threads() {
        let r = std::sync::Arc::new(Ring::new(1024, 0u64));
        let w = r.clone();
        let t = std::thread::spawn(move || {
            let mut i = 0u64;
            while i < 100_000 {
                if unsafe { w.push(i) } {
                    i += 1;
                }
            }
        });
        let mut next = 0u64;
        while next < 100_000 {
            unsafe { r.drain(|v| {
                assert_eq!(v, next);
                next += 1;
            }) };
        }
        t.join().unwrap();
    }
}
