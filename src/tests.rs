use std::{collections::VecDeque, mem::transmute, time::Instant};

use crate::{
    header::{Guard, Header},
    RingAl, MINALLOC, USIZELEN,
};

const SIZE: usize = 2048;

struct IntegrityGuard {
    start: *mut usize,
    end: *mut usize,
}

macro_rules! setup {
    () => {{
        let ringal = RingAl::new(SIZE);
        let start = ringal.head;
        let end = Header::new(start).next().inner();
        let guard = IntegrityGuard { start, end };
        (ringal, guard)
    }};
}

impl Drop for IntegrityGuard {
    fn drop(&mut self) {
        let mut next = Header::new(self.start);
        let max = SIZE / USIZELEN - 1;
        for _ in 0..max {
            next = next.next();
            if next.inner() == self.start {
                break;
            }
            assert!(
                next.inner() <= self.end,
                "pointers in ring buffer should always point inside"
            )
        }
        assert_eq!(
            next.inner(),
            self.start,
            "ring buffer should always wrap around"
        )
    }
}

#[test]
fn test_init() {
    let (ringal, _g) = setup!();
    let tail = (unsafe { *ringal.head } >> 1) as *mut usize;
    assert_eq!(ringal.head, ((unsafe { *tail }) >> 1) as *mut usize);
    assert_eq!(
        unsafe { tail.offset_from(ringal.head) } as usize,
        SIZE / USIZELEN - 1
    );
}

#[test]
fn test_alloc() {
    let (mut ringal, _g) = setup!();
    let start = ringal.head;
    let header = ringal.alloc(1024);
    assert!(
        header.is_some(),
        "should be able to allocate with new allocator"
    );
    let header = header.unwrap();
    assert_eq!(header.inner(), start);
    assert_eq!(header.next().inner(), ringal.head);
}

#[test]
fn test_multi_alloc() {
    const COUNT: usize = 16;
    let (mut ringal, _g) = setup!();
    let mut allocations = Vec::<Guard>::with_capacity(COUNT);
    for i in 0..COUNT {
        let size = SIZE / COUNT - USIZELEN - (i == COUNT - 1) as usize * USIZELEN;
        let header = ringal.alloc(size);
        assert!(
            header.is_some(),
            "should have enough capacity for all allocations"
        );
        let header = header.unwrap();
        header.set();
        allocations.push(header.into());
    }
    let header = ringal.alloc(SIZE / COUNT - USIZELEN);
    assert!(header.is_none(), "should have run out of capacity");
    allocations.clear();
    let header = ringal.alloc(SIZE / COUNT - USIZELEN);
    assert!(
        header.is_some(),
        "should have all capacity after dropping allocations"
    );
}

#[test]
fn test_continious_alloc() {
    let (mut ringal, _g) = setup!();
    const ITERS: u64 = 8192;
    let mut allocations = VecDeque::<Guard>::with_capacity(4);
    for i in 0..ITERS {
        let size = (unsafe { transmute::<Instant, (u64, u64)>(Instant::now()) }.0 * i) % 256;
        let header = ringal.alloc((size as usize).max(MINALLOC * USIZELEN));
        assert!(header.is_some(), "should always have capacity");
        let header = header.unwrap();
        header.set();
        allocations.push_back(header.into());
        if allocations.len() == 4 {
            allocations.pop_front();
            allocations.pop_front();
        }
    }
}
