use crate::sys::c;

static mut IS_NT: bool = true;

// See compat.rs for the explanation of how this works.
#[used]
#[link_section = ".CRT$XCU"]
static INIT_TABLE_ENTRY: unsafe extern "C" fn() = init;

unsafe extern "C" fn init() {
    // according to old MSDN info, the high-order bit is set only on 95/98/ME.
    IS_NT = c::GetVersion() < 0x8000_0000;
}

/// Returns true if we are running on a Windows NT-based system. Only use this for APIs where the
/// same API differs in behavior or capability on 9x/ME compared to NT.
#[inline(always)]
pub(crate) fn is_windows_nt() -> bool {
    unsafe { IS_NT }
}
