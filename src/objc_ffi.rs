use std::ffi::c_void;

pub(crate) type Id = *mut c_void;
pub(crate) type Sel = *mut c_void;

unsafe extern "C" {
    pub(crate) fn objc_getClass(name: *const i8) -> Id;
    pub(crate) fn sel_registerName(name: *const i8) -> Sel;
    fn objc_msgSend();
}

pub(crate) unsafe fn send0(obj: Id, sel: Sel) -> Id {
    let f: unsafe extern "C" fn(Id, Sel) -> Id =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { f(obj, sel) }
}

pub(crate) unsafe fn send1(obj: Id, sel: Sel, a: Id) -> Id {
    let f: unsafe extern "C" fn(Id, Sel, Id) -> Id =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { f(obj, sel, a) }
}

pub(crate) unsafe fn send2_void(obj: Id, sel: Sel, a: Id, b: Id) {
    let f: unsafe extern "C" fn(Id, Sel, Id, Id) =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    unsafe { f(obj, sel, a, b) }
}

pub(crate) unsafe fn nsstring(s: &str) -> Id {
    let cls = unsafe { objc_getClass(c"NSString".as_ptr()) };
    let cstr = std::ffi::CString::new(s)
        .unwrap_or_else(|_| std::ffi::CString::new(s.replace('\0', "")).unwrap());
    unsafe {
        send1(
            cls,
            sel_registerName(c"stringWithUTF8String:".as_ptr()),
            cstr.as_ptr() as Id,
        )
    }
}
