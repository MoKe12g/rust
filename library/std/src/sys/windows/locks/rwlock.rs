use crate::cell::UnsafeCell;
use crate::mem;
use crate::sync::atomic::{AtomicUsize, Ordering};
use crate::sys::c;
use crate::sys::locks::{
    mutex::{
        compat::{atomic_boxed_init, MutexKind, MUTEX_KIND},
        critical_section_mutex::CriticalSectionMutex,
    },
    Mutex,
};

/// The fallback implementation is just a mutex, which might be slower, but valid and compatible.
pub struct MovableRWLock {
    // Both the `SRWLOCK` and a boxed mutex are usize-sized
    lock: AtomicUsize,
}

unsafe impl Send for MovableRWLock {}
unsafe impl Sync for MovableRWLock {}

impl MovableRWLock {
    pub const fn new() -> MovableRWLock {
        MovableRWLock { lock: AtomicUsize::new(0) }
    }
    #[inline]
    pub unsafe fn read(&self) {
        match MUTEX_KIND {
            MutexKind::SrwLock => c::AcquireSRWLockShared(&self.lock as *const _ as *mut _),
            MutexKind::CriticalSection | MutexKind::Legacy => (*self.remutex()).lock(),
        }
    }
    #[inline]
    pub unsafe fn try_read(&self) -> bool {
        match MUTEX_KIND {
            MutexKind::SrwLock => c::TryAcquireSRWLockShared(&self.lock as *const _ as *mut _) != 0,
            MutexKind::CriticalSection | MutexKind::Legacy => (*self.remutex()).try_lock(),
        }
    }
    #[inline]
    pub unsafe fn write(&self) {
        match MUTEX_KIND {
            MutexKind::SrwLock => c::AcquireSRWLockExclusive(&self.lock as *const _ as *mut _),
            MutexKind::CriticalSection | MutexKind::Legacy => (*self.remutex()).lock(),
        }
    }
    #[inline]
    pub unsafe fn try_write(&self) -> bool {
        match MUTEX_KIND {
            MutexKind::SrwLock => {
                c::TryAcquireSRWLockExclusive(&self.lock as *const _ as *mut _) != 0
            }
            MutexKind::CriticalSection | MutexKind::Legacy => (*self.remutex()).try_lock(),
        }
    }
    #[inline]
    pub unsafe fn read_unlock(&self) {
        match MUTEX_KIND {
            MutexKind::SrwLock => c::ReleaseSRWLockShared(&self.lock as *const _ as *mut _),
            MutexKind::CriticalSection | MutexKind::Legacy => (*self.remutex()).unlock(),
        }
    }
    #[inline]
    pub unsafe fn write_unlock(&self) {
        match MUTEX_KIND {
            MutexKind::SrwLock => c::ReleaseSRWLockExclusive(&self.lock as *const _ as *mut _),
            MutexKind::CriticalSection | MutexKind::Legacy => (*self.remutex()).unlock(),
        }
    }

    #[inline]
    pub unsafe fn destroy(&self) {
        match MUTEX_KIND {
            MutexKind::SrwLock => {}
            MutexKind::CriticalSection | MutexKind::Legacy => {
                match self.lock.load(Ordering::SeqCst) {
                    0 => {}
                    n => {
                        Box::from_raw(n as *mut Mutex).destroy();
                    }
                }
            }
        }
    }

    unsafe fn remutex(&self) -> *mut Mutex {
        unsafe fn init() -> Box<Mutex> {
            let mut re = box Mutex::new();
            re.init();
            re
        }

        unsafe fn destroy(mutex: &Mutex) {
            mutex.destroy()
        }

        atomic_boxed_init(&self.lock, init, destroy)
    }
}

/// For static mutexes and RWLocks we can use critical sections all the way down to NT 3.1 since
/// `try_lock`/`TryEnterCriticalSection` is not needed.
// based on the old pre-XP-support-removal mutex impl
// https://github.com/rust-lang/rust/blob/c35007dbbe4846c641b5edad9fddf3f72a5a035a/library/std/src/sys/windows/mutex.rs
pub struct RWLock {
    lock: AtomicUsize,
    held: UnsafeCell<bool>,
}

pub type StaticRWLock = RWLock;

unsafe impl Send for RWLock {}
unsafe impl Sync for RWLock {}

impl RWLock {
    pub const fn new() -> Self {
        Self {
            // This works because SRWLOCK_INIT is 0 (wrapped in a struct), so we are also properly
            // initializing an SRWLOCK here.
            lock: AtomicUsize::new(0),
            held: UnsafeCell::new(false),
        }
    }

    #[inline]
    pub unsafe fn read(&self) {
        self.lock();
    }

    #[inline]
    pub unsafe fn write(&self) {
        self.lock();
    }

    #[inline]
    pub unsafe fn lock(&self) {
        match MUTEX_KIND {
            MutexKind::SrwLock => {
                debug_assert!(mem::size_of::<c::SRWLOCK>() <= mem::size_of_val(&self.lock));
                c::AcquireSRWLockExclusive(&self.lock as *const _ as *mut _)
            }
            MutexKind::CriticalSection | MutexKind::Legacy => {
                let re = self.remutex();
                (*re).lock();
                if !self.flag_locked() {
                    (*re).unlock();
                    panic!("cannot recursively lock a mutex");
                }
            }
        }
    }

    #[inline]
    pub unsafe fn read_unlock(&self) {
        self.unlock();
    }

    #[inline]
    pub unsafe fn write_unlock(&self) {
        self.unlock();
    }

    #[inline]
    pub unsafe fn unlock(&self) {
        match MUTEX_KIND {
            MutexKind::SrwLock => c::ReleaseSRWLockExclusive(&self.lock as *const _ as *mut _),
            MutexKind::CriticalSection | MutexKind::Legacy => {
                *self.held.get() = false;
                (*self.remutex()).unlock();
            }
        }
    }

    unsafe fn remutex(&self) -> *mut CriticalSectionMutex {
        unsafe fn init() -> Box<CriticalSectionMutex> {
            let re = box CriticalSectionMutex::new();
            re.init();
            re
        }

        unsafe fn destroy(mutex: &CriticalSectionMutex) {
            mutex.destroy()
        }

        atomic_boxed_init(&self.lock, init, destroy)
    }

    unsafe fn flag_locked(&self) -> bool {
        if *self.held.get() {
            false
        } else {
            *self.held.get() = true;
            true
        }
    }
}
