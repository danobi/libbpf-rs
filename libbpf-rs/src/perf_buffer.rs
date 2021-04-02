use core::ffi::c_void;
use std::boxed::Box;
use std::ptr;
use std::slice;
use std::time::Duration;

use crate::*;

fn is_power_two(i: usize) -> bool {
    i > 0 && (!(i & (i - 1))) > 0
}

struct CbStruct<F, G> {
    // Both sample_cb and lost_cb are owning pointers to Box's
    sample_cb: *mut F,
    lost_cb: *mut G,
}

impl<F, G> Drop for CbStruct<F, G> {
    fn drop(&mut self) {
        if !self.sample_cb.is_null() {
            let _ = unsafe { Box::from_raw(self.sample_cb) };
        }

        if !self.lost_cb.is_null() {
            let _ = unsafe { Box::from_raw(self.lost_cb) };
        }
    }
}

/// We use this trait to erase the types from `CbStruct`. If we don't erase the type,
/// then any user storing an instance of `PerfBuffer` needs to specify the type of
/// their callbacks (which is impossible for closures as closures have an anonymous type).
///
/// Then in `PerfBuffer`, we store a `Box<dyn CbStruct>` (a wide pointer). The wide pointer
/// contains the type information `Drop` needs to recreate the callback boxes.
trait CbStructTypeErased {}
impl<F, G> CbStructTypeErased for CbStruct<F, G> {}

/// Builds [`PerfBuffer`] instances.
pub struct PerfBufferBuilder<'a, F, G>
where
    F: FnMut(i32, &[u8]) + 'static,
    G: FnMut(i32, u64) + 'static,
{
    map: &'a Map,
    pages: usize,
    sample_cb: Option<Box<F>>,
    lost_cb: Option<Box<G>>,
}

impl<'a> PerfBufferBuilder<'a, fn(i32, &[u8]), fn(i32, u64)> {
    pub fn new(map: &'a Map) -> Self {
        Self {
            map,
            pages: 64,
            sample_cb: None,
            lost_cb: None,
        }
    }
}

impl<'a, F, G> PerfBufferBuilder<'a, F, G>
where
    F: FnMut(i32, &[u8]) + 'static,
    G: FnMut(i32, u64) + 'static,
{
    /// Callback to run when a sample is received.
    ///
    /// This callback provides a raw byte slice. You may find libraries such as
    /// [`plain`](https://crates.io/crates/plain) helpful.
    ///
    /// Callback arguments are: `(cpu, data)`.
    pub fn sample_cb<NewF: FnMut(i32, &[u8])>(self, cb: NewF) -> PerfBufferBuilder<'a, NewF, G> {
        PerfBufferBuilder {
            map: self.map,
            pages: self.pages,
            sample_cb: Some(Box::new(cb)),
            lost_cb: self.lost_cb,
        }
    }

    /// Callback to run when a sample is received.
    ///
    /// Callback arguments are: `(cpu, lost_count)`.
    pub fn lost_cb<NewG: FnMut(i32, u64)>(self, cb: NewG) -> PerfBufferBuilder<'a, F, NewG> {
        PerfBufferBuilder {
            map: self.map,
            pages: self.pages,
            sample_cb: self.sample_cb,
            lost_cb: Some(Box::new(cb)),
        }
    }

    /// The number of pages to size the ring buffer.
    pub fn pages(&mut self, pages: usize) -> &mut Self {
        self.pages = pages;
        self
    }

    pub fn build(self) -> Result<PerfBuffer> {
        if self.map.map_type() != MapType::PerfEventArray {
            return Err(Error::InvalidInput(
                "Must use a PerfEventArray map".to_string(),
            ));
        }

        if !is_power_two(self.pages) {
            return Err(Error::InvalidInput(
                "Page count must be power of two".to_string(),
            ));
        }

        let c_sample_cb: libbpf_sys::perf_buffer_sample_fn = if self.sample_cb.is_some() {
            Some(Self::call_sample_cb)
        } else {
            None
        };

        let c_lost_cb: libbpf_sys::perf_buffer_lost_fn = if self.lost_cb.is_some() {
            Some(Self::call_lost_cb)
        } else {
            None
        };

        let callback_struct_ptr = Box::into_raw(Box::new(CbStruct {
            sample_cb: if let Some(cb) = self.sample_cb {
                Box::into_raw(cb) as *mut _
            } else {
                ptr::null_mut()
            },
            lost_cb: if let Some(cb) = self.lost_cb {
                Box::into_raw(cb) as *mut _
            } else {
                ptr::null_mut()
            },
        }));

        let opts = libbpf_sys::perf_buffer_opts {
            sample_cb: c_sample_cb,
            lost_cb: c_lost_cb,
            ctx: callback_struct_ptr as *mut _,
        };

        let ptr = unsafe {
            libbpf_sys::perf_buffer__new(self.map.fd(), self.pages as libbpf_sys::size_t, &opts)
        };
        let err = unsafe { libbpf_sys::libbpf_get_error(ptr as *const _) };
        if err != 0 {
            Err(Error::System(err as i32))
        } else {
            Ok(PerfBuffer {
                ptr,
                _cb_struct: unsafe { Box::from_raw(callback_struct_ptr) },
            })
        }
    }

    unsafe extern "C" fn call_sample_cb(ctx: *mut c_void, cpu: i32, data: *mut c_void, size: u32) {
        let callback_struct = ctx as *mut CbStruct<F, G>;
        let callback_ptr = (*callback_struct).sample_cb as *mut F;
        let callback = &mut *callback_ptr;

        callback(cpu, slice::from_raw_parts(data as *const u8, size as usize));
    }

    unsafe extern "C" fn call_lost_cb(ctx: *mut c_void, cpu: i32, count: u64) {
        let callback_struct = ctx as *mut CbStruct<F, G>;
        let callback_ptr = (*callback_struct).lost_cb as *mut G;
        let callback = &mut *callback_ptr;

        callback(cpu, count);
    }
}

/// Represents a special kind of [`Map`]. Typically used to transfer data between
/// [`Program`]s and userspace.
pub struct PerfBuffer {
    ptr: *mut libbpf_sys::perf_buffer,
    // Hold onto the box so it'll get dropped when PerfBuffer is dropped
    _cb_struct: Box<dyn CbStructTypeErased>,
}

impl PerfBuffer {
    pub fn poll(&self, timeout: Duration) -> Result<()> {
        let ret = unsafe { libbpf_sys::perf_buffer__poll(self.ptr, timeout.as_millis() as i32) };
        if ret < 0 {
            Err(Error::System(-ret))
        } else {
            Ok(())
        }
    }
}

impl Drop for PerfBuffer {
    fn drop(&mut self) {
        unsafe {
            libbpf_sys::perf_buffer__free(self.ptr);
        }
    }
}
