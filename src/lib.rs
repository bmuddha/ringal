use buffer::ExtBuf;
use header::Header;

pub struct RingAl {
    head: *mut usize,
}

const USIZELEN: usize = size_of::<usize>();
const MINALLOC: usize = 4;

impl RingAl {
    pub fn new(mut size: usize) -> Self {
        assert!(size > USIZELEN && size < usize::MAX >> 1);
        size = size.next_power_of_two() / USIZELEN;
        let mut buffer = Vec::<usize>::with_capacity(size);
        size = buffer.capacity();
        #[allow(clippy::uninit_vec)]
        unsafe {
            buffer.set_len(size)
        };
        let inner = Box::leak(buffer.into_boxed_slice()).as_mut_ptr();
        let head = inner;
        let tail = unsafe { inner.add(size - 1) };
        unsafe { *head = (tail as usize) << 1 };
        unsafe { *tail = (head as usize) << 1 };

        Self { head }
    }

    pub fn extendable(&mut self, size: usize) -> Option<ExtBuf<'_>> {
        let header = self.alloc(size)?;
        header.set();
        Some(ExtBuf {
            header,
            initialized: 0,
            ringal: self,
            finalized: false,
        })
    }

    fn alloc(&mut self, mut size: usize) -> Option<Header> {
        size = (size / USIZELEN + (size % USIZELEN != 0) as usize).max(MINALLOC);
        let mut start = Header::new(self.head);
        let mut accumulated = 0;
        let mut next = start;
        while accumulated < size {
            next.available().then_some(())?;
            next = next.next();
            (next != self.head).then_some(())?;
            if next < start {
                accumulated = 0;
                start = next;
                continue;
            }
            accumulated = start.distance(&next);
        }
        if (accumulated - size) <= MINALLOC {
            self.head = next.inner();
        } else {
            self.head = unsafe { start.inner().add(size + 1) };
            let head = Header::new(self.head);
            head.store(next.inner());
        }
        start.store(self.head);
        Some(start)
    }
}

mod buffer;
mod header;
#[cfg(test)]
mod tests;
