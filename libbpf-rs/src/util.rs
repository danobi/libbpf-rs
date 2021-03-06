use std::ffi::{CStr, CString};
use std::os::raw::c_char;
use std::path::Path;

use crate::*;

pub fn str_to_cstring(s: &str) -> Result<CString> {
    CString::new(s).map_err(|e| Error::InvalidInput(e.to_string()))
}

pub fn path_to_cstring<P: AsRef<Path>>(path: P) -> Result<CString> {
    let path_str = path.as_ref().to_str().ok_or_else(|| {
        Error::InvalidInput(format!("{} is not valid unicode", path.as_ref().display()))
    })?;

    str_to_cstring(path_str)
}

pub fn c_ptr_to_string(p: *const c_char) -> Result<String> {
    if p.is_null() {
        return Err(Error::Internal("Null string".to_owned()));
    }

    let c_str = unsafe { CStr::from_ptr(p) };
    Ok(c_str
        .to_str()
        .map_err(|e| Error::Internal(e.to_string()))?
        .to_owned())
}
