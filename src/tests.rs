#![allow(unused)]

use std::{
    collections::{HashSet, VecDeque},
    io::Write,
    mem::transmute,
    sync::mpsc::sync_channel,
    time::Instant,
};

use crate::{
    buffer::FixedBuf,
    header::{Guard, Header},
    RingAl, MINALLOC, USIZEALIGN, USIZELEN,
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
        let end = Header(start).next().0;
        let guard = IntegrityGuard { start, end };
        (ringal, guard)
    }};
}

impl Drop for IntegrityGuard {
    fn drop(&mut self) {
        let mut next = Header(self.start);
        let max = SIZE / USIZELEN - 1;
        for _ in 0..max {
            next = next.next();
            if next.0 == self.start {
                break;
            }
            assert!(
                next.0 <= self.end,
                "pointers in ring buffer should always point inside"
            )
        }
        assert_eq!(next.0, self.start, "ring buffer should always wrap around")
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
    let header = ringal.alloc(1024, USIZEALIGN);
    assert!(
        header.is_some(),
        "should be able to allocate with new allocator"
    );
    let header = header.unwrap();
    assert_eq!(header.0, start);
    assert_eq!(header.next().0, ringal.head);
}

#[test]
fn test_multi_alloc() {
    const COUNT: usize = 16;
    let (mut ringal, _g) = setup!();
    let mut allocations = Vec::<Guard>::with_capacity(COUNT);
    for i in 0..COUNT {
        let size = SIZE / COUNT - USIZELEN * 2 - (i == COUNT - 1) as usize * USIZELEN;
        let header = ringal.alloc(size, USIZEALIGN);
        assert!(
            header.is_some(),
            "should have enough capacity for all allocations"
        );
        let header = header.unwrap();
        header.set();
        allocations.push(header.into());
    }
    let header = ringal.alloc(SIZE / COUNT - USIZELEN, USIZEALIGN);
    assert!(header.is_none(), "should have run out of capacity");
    allocations.clear();
    let header = ringal.alloc(SIZE / COUNT - USIZELEN, USIZEALIGN);
    assert!(
        header.is_some(),
        "should have all capacity after dropping allocations"
    );
}

#[test]
fn test_continuous_alloc() {
    let (mut ringal, _g) = setup!();
    const ITERS: u64 = 8192;
    let mut allocations = VecDeque::<Guard>::with_capacity(4);
    for i in 0..ITERS {
        let size = (unsafe { transmute::<Instant, (u64, u32)>(Instant::now()) }.0 * i) % 256;
        let header = ringal.alloc(size as usize, USIZEALIGN);
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
    const MSG: &[u8] = b"64aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const ITERS: usize = (SIZE - USIZELEN) / (MSG.len() + USIZELEN * 2);
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
        let random = unsafe { transmute::<Instant, (usize, u32)>(Instant::now()).0 };
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
    assert!(
        buffer.spare() > 1,
        "fixed buffer should have some spare capacity left after write"
    );
}

#[test]
fn test_fixed_buf_multi_alloc() {
    let (mut ringal, _g) = setup!();
    const MSG: &[u8] = b"64aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const ITERS: usize = (SIZE - USIZELEN) / (MSG.len() + USIZELEN * 2);
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
        let random = unsafe { transmute::<Instant, (usize, u32)>(Instant::now()).0 };
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
        let random = unsafe { transmute::<Instant, (usize, u32)>(Instant::now()).0 };
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

macro_rules! test_generic_buf {
    ($int: ty) => {
        let (mut ringal, _g) = setup!();
        const ITERS: $int = 7;
        struct SomeType {
            i: $int,
            string: FixedBuf,
        }
        let mut string = ringal.fixed(64).unwrap();
        let _ = string.write(b"hello world").unwrap();
        let string = string.freeze();
        let buffer = ringal.generic::<SomeType>(ITERS as usize);
        assert!(
            buffer.is_some(),
            "should be able to allocate generic buf with new allocator"
        );
        let mut buffer = buffer.unwrap();
        for i in 0..ITERS {
            let instance = SomeType {
                i,
                string: string.clone(),
            };
            assert!(buffer.push(instance).is_none());
        }
        assert_eq!(buffer.len(), ITERS as usize);
        //for i in (0..ITERS).rev() {
        //    let elem = buffer.pop();
        //    println!("poped: elem: {i}");
        //    assert!(elem.is_some(), "buffer should pop all pushed elements");
        //    let elem = elem.unwrap();
        //    assert_eq!(elem.i, i);
        //    assert_eq!(elem.string.as_ref(), string.as_ref());
        //    assert!(buffer.insert(elem, 0).is_none());
        //}
        //assert_eq!(buffer.len(), ITERS as usize);
        //for _ in 0..ITERS {
        //    let elem = buffer.swap_remove(0);
        //    assert!(
        //        elem.is_some(),
        //        "buffer should swap remove all pushed elements"
        //    );
        //    let elem = elem.unwrap();
        //    buffer.push(elem);
        //}
        //assert_eq!(buffer.len(), ITERS as usize);
        let mut indices: HashSet<$int> = [0, 1, 2, 3, 4, 5, 6].into_iter().collect();
        for _ in 0..ITERS {
            let elem = buffer.remove(0);
            assert!(
                elem.is_some(),
                "buffer should swap remove all pushed elements"
            );
            assert!(indices.remove(&elem.unwrap().i));
        }
        assert_eq!(buffer.len(), 0);
        //drop(buffer);
        //drop(ringal);
    };
}

//#[cfg(feature = "generic")]
#[test]
fn test_generic_buf_word() {
    test_generic_buf!(usize);
}

#[test]
fn test_generic_buf_double_word_align() {
    test_generic_buf!(u128);
}
#[test]
fn test_generic_buf_half_word_align() {
    test_generic_buf!(u32);
}

#[test]
fn test_generic_buf_quarter_word_align() {
    test_generic_buf!(u16);
}

#[test]
fn test_generic_buf_eighth_word_align() {
    test_generic_buf!(u8);
}
