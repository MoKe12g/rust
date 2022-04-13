//! System Mutexes
//!
//! The Windows implementation of mutexes is a little odd and it might not be
//! immediately obvious what's going on. The primary oddness is that SRWLock is
//! used instead of CriticalSection, and this is done because:
//!
//! 1. SRWLock is several times faster than CriticalSection according to
//!    benchmarks performed on both Windows 8 and Windows 7.
//!
//! 2. CriticalSection allows recursive locking while SRWLock deadlocks. The
//!    Unix implementation deadlocks so consistency is preferred. See #19962 for
//!    more details.
//!
//! 3. While CriticalSection is fair and SRWLock is not, the current Rust policy
//!    is that there are no guarantees of fairness.

use crate::cell::UnsafeCell;
use crate::mem::ManuallyDrop;
use crate::ops::{Deref, DerefMut};
use crate::sys::c;
use compat::{MutexKind, MUTEX_KIND};

pub mod compat;
pub mod critical_section_mutex;
mod legacy_mutex;
mod srwlock_mutex;

// Windows SRW Locks are movable (while not borrowed).
pub type MovableMutex = Mutex;

pub union InnerMutex {
    srwlock: ManuallyDrop<srwlock_mutex::SrwLockMutex>,
    critical_section: ManuallyDrop<Box<critical_section_mutex::CriticalSectionMutex>>,
    legacy: ManuallyDrop<legacy_mutex::LegacyMutex>,
}

impl Drop for InnerMutex {
    fn drop(&mut self) {
        unsafe {
            match MUTEX_KIND {
                MutexKind::SrwLock => ManuallyDrop::drop(&mut self.srwlock),
                MutexKind::CriticalSection => ManuallyDrop::drop(&mut self.critical_section),
                MutexKind::Legacy => ManuallyDrop::drop(&mut self.legacy),
            }
        }
    }
}

pub struct Mutex {
    pub inner: InnerMutex,
    pub held: UnsafeCell<bool>,
}

unsafe impl Send for Mutex {}
unsafe impl Sync for Mutex {}

impl Mutex {
    pub fn raw(&self) -> c::PSRWLOCK {
        unsafe {
            debug_assert_eq!(MUTEX_KIND, MutexKind::SrwLock);
            self.inner.srwlock.raw()
        }
    }

    pub fn new() -> Mutex {
        unsafe {
            match MUTEX_KIND {
                MutexKind::SrwLock => Self {
                    inner: InnerMutex {
                        srwlock: ManuallyDrop::new(srwlock_mutex::SrwLockMutex::new()),
                    },
                    held: UnsafeCell::new(false),
                },
                MutexKind::CriticalSection => Self {
                    inner: InnerMutex {
                        critical_section: ManuallyDrop::new(
                            box critical_section_mutex::CriticalSectionMutex::new(),
                        ),
                    },
                    held: UnsafeCell::new(false),
                },
                MutexKind::Legacy => Self {
                    inner: InnerMutex {
                        legacy: ManuallyDrop::new(legacy_mutex::LegacyMutex::new()),
                    },
                    held: UnsafeCell::new(false),
                },
            }
        }
    }

    #[inline]
    pub unsafe fn init(&mut self) {
        match MUTEX_KIND {
            MutexKind::SrwLock => {
                self.inner.srwlock.deref_mut().init();
            }
            MutexKind::CriticalSection => {
                self.inner.critical_section.deref_mut().init();
            }
            MutexKind::Legacy => {
                self.inner.legacy.deref_mut().init();
            }
        }
    }

    #[inline]
    pub unsafe fn lock(&self) {
        match MUTEX_KIND {
            MutexKind::SrwLock => self.inner.srwlock.deref().lock(),
            MutexKind::CriticalSection => {
                self.inner.critical_section.deref().lock();
                if !self.flag_locked() {
                    self.unlock();
                    panic!("cannot recursively lock a mutex");
                }
            }
            MutexKind::Legacy => {
                self.inner.legacy.deref().lock();
                if !self.flag_locked() {
                    self.unlock();
                    panic!("cannot recursively lock a mutex");
                }
            }
        }
    }

    #[inline]
    pub unsafe fn try_lock(&self) -> bool {
        match MUTEX_KIND {
            MutexKind::SrwLock => self.inner.srwlock.deref().try_lock(),
            MutexKind::CriticalSection => {
                if !self.inner.critical_section.deref().try_lock() {
                    false
                } else if self.flag_locked() {
                    true
                } else {
                    self.unlock();
                    false
                }
            }
            MutexKind::Legacy => {
                if !self.inner.legacy.deref().try_lock() {
                    false
                } else if self.flag_locked() {
                    true
                } else {
                    self.unlock();
                    false
                }
            }
        }
    }

    #[inline]
    pub unsafe fn unlock(&self) {
        match MUTEX_KIND {
            MutexKind::SrwLock => self.inner.srwlock.deref().unlock(),
            MutexKind::CriticalSection => {
                *self.held.get() = false;
                self.inner.critical_section.deref().unlock();
            }
            MutexKind::Legacy => {
                *self.held.get() = false;
                self.inner.legacy.deref().unlock()
            }
        }
    }

    #[inline]
    pub unsafe fn destroy(&self) {
        match MUTEX_KIND {
            MutexKind::SrwLock => self.inner.srwlock.deref().destroy(),
            MutexKind::CriticalSection => self.inner.critical_section.deref().destroy(),
            MutexKind::Legacy => self.inner.legacy.deref().destroy(),
        }
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

pub type StaticMutex = super::StaticRWLock;

pub struct ReentrantMutex {
    /// This contains either a critical section struct (raw unboxed), or an uninitialized handle
    /// (plus extra space) for the legacy implementation. Critical section structs may not be moved
    /// after initialization, but the unsafe API where these internal mutexes are used gives this
    /// guarantee.
    inner: UnsafeCell<critical_section_mutex::CriticalSectionMutex>,
}

unsafe impl Send for ReentrantMutex {}
unsafe impl Sync for ReentrantMutex {}

impl ReentrantMutex {
    pub const fn uninitialized() -> ReentrantMutex {
        ReentrantMutex {
            inner: UnsafeCell::new(critical_section_mutex::CriticalSectionMutex::new()),
        }
    }

    pub unsafe fn init(&self) {
        match MUTEX_KIND {
            MutexKind::SrwLock | MutexKind::CriticalSection => {
                (*self.inner.get().cast::<critical_section_mutex::CriticalSectionMutex>()).init()
            }
            MutexKind::Legacy => (*self.inner.get().cast::<legacy_mutex::LegacyMutex>()).init(),
        }
    }

    pub unsafe fn lock(&self) {
        match MUTEX_KIND {
            MutexKind::SrwLock | MutexKind::CriticalSection => {
                (*self.inner.get().cast::<critical_section_mutex::CriticalSectionMutex>()).lock()
            }

            MutexKind::Legacy => (*self.inner.get().cast::<legacy_mutex::LegacyMutex>()).lock(),
        }
    }

    #[inline]
    pub unsafe fn try_lock(&self) -> bool {
        match MUTEX_KIND {
            MutexKind::SrwLock | MutexKind::CriticalSection => {
                (*self.inner.get().cast::<critical_section_mutex::CriticalSectionMutex>())
                    .try_lock()
            }

            MutexKind::Legacy => (*self.inner.get().cast::<legacy_mutex::LegacyMutex>()).try_lock(),
        }
    }

    pub unsafe fn unlock(&self) {
        match MUTEX_KIND {
            MutexKind::SrwLock | MutexKind::CriticalSection => {
                (*self.inner.get().cast::<critical_section_mutex::CriticalSectionMutex>()).unlock()
            }

            MutexKind::Legacy => (*self.inner.get().cast::<legacy_mutex::LegacyMutex>()).unlock(),
        }
    }

    pub unsafe fn destroy(&self) {
        match MUTEX_KIND {
            MutexKind::SrwLock | MutexKind::CriticalSection => {
                (*self.inner.get().cast::<critical_section_mutex::CriticalSectionMutex>()).destroy()
            }

            MutexKind::Legacy => (*self.inner.get().cast::<legacy_mutex::LegacyMutex>()).destroy(),
        }
    }
}
