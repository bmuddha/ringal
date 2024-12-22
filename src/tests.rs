use std::{
    collections::VecDeque, io::Write, mem::transmute, sync::mpsc::sync_channel, time::Instant,
};

use crate::{
    buffer::FixedBuf,
    header::{Guard, Header},
    RingAl, MINALLOC, USIZELEN,
};

const SIZE: usize = 4096;

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
        let header = ringal.alloc(size as usize);
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

#[test]
fn test_ext_buf_alloc() {
    let (mut ringal, _g) = setup!();
    const MSG: &[u8] = b"message";
    const ITERS: usize = (SIZE - USIZELEN * 2) / MSG.len();
    let writer = ringal.extendable(1024);
    assert!(
        writer.is_some(),
        "new allocator should have the capacity for ext buf"
    );
    let mut writer = writer.unwrap();

    for _ in 0..ITERS {
        let result = writer.write(MSG);
        assert!(
            result.is_ok() && result.unwrap() == MSG.len(),
            "exlusively owned allocator should extend"
        );
    }
}

#[test]
fn test_ext_buf_multi_alloc() {
    let (mut ringal, _g) = setup!();
    const MSG: &[u8] = b"70aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const ITERS: usize = (SIZE - USIZELEN) / (MSG.len() + USIZELEN);
    let mut buffers = Vec::with_capacity(ITERS);
    for _ in 0..ITERS {
        let writer = ringal.extendable(MSG.len());
        assert!(
            writer.is_some(),
            "allocator has just enough capacity for all iterations"
        );
        let mut writer = writer.unwrap();
        let result = writer.write(MSG);
        assert!(
            result.is_ok() && result.unwrap() == MSG.len(),
            "buffer should have exact capacity for MSG"
        );
        buffers.push(writer.finalize());
    }
    let writer = ringal.extendable(MSG.len());
    assert!(writer.is_none(), "allocator should be exhausted after loop");
    drop(writer);
    buffers.clear();
    let writer = ringal.extendable(MSG.len());
    assert!(
        writer.is_some(),
        "allocator should have full capacity after clear"
    );
}

#[test]
fn test_ext_buf_continuous_alloc() {
    let (mut ringal, _g) = setup!();
    let mut string = Vec::<u8>::new();
    let mut buffers = VecDeque::with_capacity(4);
    const ITERS: usize = 8192;
    for i in 0..ITERS {
        let random = unsafe { transmute::<Instant, (usize, usize)>(Instant::now()).0 };
        let mut size = (random * i) % 256;
        if size < MINALLOC * USIZELEN {
            size = 256 - size
        };
        string.clear();
        string.extend(std::iter::repeat_n(b'a', size));
        let writer = ringal.extendable(128);
        assert!(
            writer.is_some(),
            "allocator should never run out of capacity"
        );
        let mut writer = writer.unwrap();
        let result = writer.write(string.as_slice());
        assert!(
            result.is_ok() && result.unwrap() == string.len(),
            "allocator should have extra capacity for extension"
        );
        buffers.push_back(writer.finalize());
        if buffers.len() == 4 {
            buffers.pop_front();
            buffers.pop_front();
        }
    }
}

#[test]
fn test_fixed_buf_alloc() {
    let (mut ringal, _g) = setup!();
    const MSG: &[u8] = b"message to be written";
    let buffer = ringal.fixed(MSG.len());
    assert!(
        buffer.is_some(),
        "new allocator should have the capacity for fixed buf"
    );
    let mut buffer = buffer.unwrap();

    let result = buffer.write(MSG);
    assert!(
        result.is_ok() && result.unwrap() == MSG.len(),
        "fixed buffer write should never error"
    );
    assert_eq!(
        buffer.len(),
        MSG.len(),
        "fixed buffer len should be equal to that of written message"
    );
    assert_eq!(
        buffer.spare(),
        MINALLOC * USIZELEN - MSG.len(),
        "fixed buffer should have some spare capacity left after write"
    );
}

#[test]
fn test_fixed_buf_multi_alloc() {
    let (mut ringal, _g) = setup!();
    const MSG: &[u8] = b"70aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const ITERS: usize = (SIZE - USIZELEN) / (MSG.len() + USIZELEN);
    let mut buffers = Vec::with_capacity(ITERS);
    for _ in 0..ITERS {
        let buffer = ringal.fixed(MSG.len());
        assert!(
            buffer.is_some(),
            "allocator has just enough capacity for all iterations"
        );
        let mut buffer = buffer.unwrap();
        let result = buffer.write(MSG);
        assert!(
            result.is_ok() && result.unwrap() == MSG.len(),
            "fixed buffer should always write without error"
        );
        buffers.push(buffer);
    }
    let buffer = ringal.fixed(MSG.len());
    assert!(buffer.is_none(), "allocator should be exhausted after loop");
    buffers.clear();
    let buffer = ringal.fixed(MSG.len());
    assert!(
        buffer.is_some(),
        "allocator should have full capacity after clear"
    );
}

#[test]
fn test_fixed_buf_continuous_alloc() {
    let (mut ringal, _g) = setup!();
    let mut string = Vec::<u8>::new();
    let mut buffers = VecDeque::with_capacity(4);
    const ITERS: usize = 8192;
    for i in 0..ITERS {
        let random = unsafe { transmute::<Instant, (usize, usize)>(Instant::now()).0 };
        let mut size = (random * i) % 256;
        if size < MINALLOC * USIZELEN {
            size = 256 - size
        };
        string.clear();
        string.extend(std::iter::repeat_n(b'a', size));
        let buffer = ringal.fixed(string.len());
        assert!(
            buffer.is_some(),
            "allocator should never run out of capacity"
        );
        let mut buffer = buffer.unwrap();
        let result = buffer.write(string.as_slice());
        assert!(
            result.is_ok() && result.unwrap() == string.len(),
            "fixed buffer should never error on write"
        );
        buffers.push_back(buffer);
        if buffers.len() == 4 {
            buffers.pop_front();
            buffers.pop_front();
        }
    }
}

#[test]
fn test_fixed_buf_continuous_alloc_multi_thread() {
    let (mut ringal, _g) = setup!();
    let mut string = Vec::<u8>::new();
    let mut threads = Vec::with_capacity(4);
    const ITERS: usize = 8192;
    for i in 0..4 {
        let (tx, rx) = sync_channel::<FixedBuf>(2);
        std::thread::spawn(move || {
            let mut number = 0;
            while let Ok(s) = rx.recv() {
                number += s.len() * i;
            }
            number
        });
        threads.push(tx);
    }
    for i in 0..ITERS {
        let random = unsafe { transmute::<Instant, (usize, usize)>(Instant::now()).0 };
        let mut size = (random * i) % 256;
        if size < MINALLOC * USIZELEN {
            size = 256 - size
        };
        string.clear();
        string.extend(std::iter::repeat_n(b'a', size));
        let buffer = ringal.fixed(string.len());
        assert!(
            buffer.is_some(),
            "allocator should never run out of capacity"
        );
        let mut buffer = buffer.unwrap();
        let result = buffer.write(string.as_slice());
        assert!(
            result.is_ok() && result.unwrap() == string.len(),
            "fixed buffer should never error on write"
        );
        let buffer = buffer.freeze();
        for tx in &mut threads {
            tx.send(buffer.clone()).unwrap();
        }
    }
}

#[cfg(feature = "tls")]
#[test]
fn test_thread_local_allocator() {
    use crate::ringal;
    ringal!(@init, 8899);
    let fixed = ringal!(@fixed, 944);
    assert!(fixed.is_some());
    const MSG: &[u8] = b"long message with some extra text";
    const MSGLEN: usize = MSG.len();
    let extended = ringal!(@ext, 256, |mut writer| {
        let result = writer.write(MSG);
        assert!(matches!(result, Ok(MSGLEN)));
        writer.finalize()
    });
    assert!(extended.is_some());
    assert_eq!(extended.unwrap().len(), MSGLEN);
    // functions cannot capture variable in outer scope, but they still have access to TLS
    fn some_fn() {
        let fixed = ringal!(@fixed, 944);
        assert!(fixed.is_some());
        const MSG: &[u8] = b"long message with some extra text";
        const MSGLEN: usize = MSG.len();
        let extended = ringal!(@ext, 256, |mut writer| {
            let result = writer.write(MSG);
            assert!(matches!(result, Ok(MSGLEN)));
            writer.finalize()
        });
        assert!(extended.is_some());
        assert_eq!(extended.unwrap().len(), MSGLEN);
    }
    some_fn();
}
