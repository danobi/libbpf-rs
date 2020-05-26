use core::ffi::c_void;
use std::collections::HashMap;
use std::ffi::CStr;
use std::mem;
use std::os::raw::c_char;
use std::path::Path;
use std::ptr;

use bitflags::bitflags;
use nix::errno;

use crate::util;
use crate::*;

/// Sets options for opening a [`Object`]
pub struct ObjectBuilder {
    name: String,
    relaxed_maps: bool,
}

impl ObjectBuilder {
    /// Override the generated name that would have been inferred from the constructor.
    pub fn set_name<T: AsRef<str>>(&mut self, name: T) -> &mut Self {
        self.name = name.as_ref().to_string();
        self
    }

    /// Option to parse map definitions non-strictly, allowing extra attributes/data
    pub fn set_relaxed_maps(&mut self, relaxed_maps: bool) -> &mut Self {
        self.relaxed_maps = relaxed_maps;
        self
    }

    /// Option to print debug output to stderr.
    ///
    /// I haven't figured out how to call fprintf() from rust yet so for now this will
    /// just print the format string.
    pub fn set_debug(&mut self, dbg: bool) -> &mut Self {
        extern "C" fn cb(
            _level: libbpf_sys::libbpf_print_level,
            fmtstr: *const c_char,
            _va_list: *mut libbpf_sys::__va_list_tag,
        ) -> i32 {
            match util::c_ptr_to_string(fmtstr) {
                Ok(s) => eprintln!("{}", s),
                Err(e) => eprintln!("Failed to parse string: {}", e),
            };

            0
        }

        if dbg {
            unsafe { libbpf_sys::libbpf_set_print(Some(cb)) };
        } else {
            unsafe { libbpf_sys::libbpf_set_print(None) };
        }

        self
    }

    fn opts(&mut self, name: *const c_char) -> libbpf_sys::bpf_object_open_opts {
        libbpf_sys::bpf_object_open_opts {
            sz: mem::size_of::<libbpf_sys::bpf_object_open_opts>() as libbpf_sys::size_t,
            object_name: name,
            relaxed_maps: self.relaxed_maps,
            relaxed_core_relocs: false,
            pin_root_path: ptr::null(),
            attach_prog_fd: 0,
            kconfig: ptr::null(),
        }
    }

    pub fn from_path<P: AsRef<Path>>(&mut self, path: P) -> Result<Object> {
        // Convert path to a C style pointer
        let path_str = path.as_ref().to_str().ok_or_else(|| {
            Error::InvalidInput(format!("{} is not valid unicode", path.as_ref().display()))
        })?;
        let path_c = util::str_to_cstring(path_str)?;
        let path_ptr = path_c.as_ptr();

        // Convert name to a C style pointer
        //
        // NB: we must hold onto a CString otherwise our pointer dangles
        let name = util::str_to_cstring(&self.name)?;
        let name_ptr = if !self.name.is_empty() {
            name.as_ptr()
        } else {
            ptr::null()
        };

        let opts = self.opts(name_ptr);

        let obj = unsafe { libbpf_sys::bpf_object__open_file(path_ptr, &opts) };
        if obj.is_null() {
            Err(Error::Internal("Could not create bpf_object".to_string()))
        } else {
            Ok(Object::new(obj))
        }
    }

    pub fn from_memory<T: AsRef<str>>(&mut self, name: T, mem: &[u8]) -> Result<Object> {
        // Convert name to a C style pointer
        //
        // NB: we must hold onto a CString otherwise our pointer dangles
        let name = util::str_to_cstring(name.as_ref())?;
        let name_ptr = if !name.to_bytes().is_empty() {
            name.as_ptr()
        } else {
            ptr::null()
        };

        let opts = self.opts(name_ptr);

        let obj = unsafe {
            libbpf_sys::bpf_object__open_mem(
                mem.as_ptr() as *const c_void,
                mem.len() as libbpf_sys::size_t,
                &opts,
            )
        };
        if obj.is_null() {
            Err(Error::Internal("Could not create bpf_object".to_string()))
        } else {
            Ok(Object::new(obj))
        }
    }
}

impl Default for ObjectBuilder {
    fn default() -> Self {
        ObjectBuilder {
            name: String::new(),
            relaxed_maps: false,
        }
    }
}

/// Represents a BPF object file. An object may contain zero or more
/// [`Program`]s and [`Map`]s.
pub struct Object {
    ptr: *mut libbpf_sys::bpf_object,
    maps: HashMap<String, MapBuilder>,
    progs: HashMap<String, ProgramBuilder>,
}

impl Object {
    fn new(ptr: *mut libbpf_sys::bpf_object) -> Self {
        Object {
            ptr,
            maps: HashMap::new(),
            progs: HashMap::new(),
        }
    }

    pub fn name<'a>(&'a self) -> Result<&'a str> {
        unsafe {
            let ptr = libbpf_sys::bpf_object__name(self.ptr);
            CStr::from_ptr(ptr)
                .to_str()
                .map_err(|e| Error::Internal(e.to_string()))
        }
    }

    pub fn map<T: AsRef<str>>(&mut self, name: T) -> Result<Option<&mut MapBuilder>> {
        if self.maps.contains_key(name.as_ref()) {
            Ok(self.maps.get_mut(name.as_ref()))
        } else {
            let c_name = util::str_to_cstring(name.as_ref())?;
            let ptr =
                unsafe { libbpf_sys::bpf_object__find_map_by_name(self.ptr, c_name.as_ptr()) };
            if ptr.is_null() {
                Ok(None)
            } else {
                let btf_fd = unsafe { libbpf_sys::bpf_object__btf_fd(self.ptr) };
                let owned_name = name.as_ref().to_owned();
                self.maps
                    .insert(owned_name.clone(), MapBuilder::new(ptr, owned_name, btf_fd));
                Ok(self.maps.get_mut(name.as_ref()))
            }
        }
    }

    pub fn prog<T: AsRef<str>>(&mut self, name: T) -> Result<Option<&mut ProgramBuilder>> {
        if self.progs.contains_key(name.as_ref()) {
            Ok(self.progs.get_mut(name.as_ref()))
        } else {
            let c_name = util::str_to_cstring(name.as_ref())?;
            let ptr =
                unsafe { libbpf_sys::bpf_object__find_program_by_name(self.ptr, c_name.as_ptr()) };
            if ptr.is_null() {
                Ok(None)
            } else {
                let owned_name = name.as_ref().to_owned();
                self.progs.insert(owned_name, ProgramBuilder::new(ptr));
                Ok(self.progs.get_mut(name.as_ref()))
            }
        }
    }
}

impl Drop for Object {
    fn drop(&mut self) {
        unsafe {
            libbpf_sys::bpf_object__close(self.ptr);
        }
    }
}

/// Represents a parsed but not yet loaded map.
///
/// Some methods require working with raw bytes. You may find libraries such as
/// [`plain`](https://crates.io/crates/plain) helpful.
pub struct MapBuilder {
    ptr: *mut libbpf_sys::bpf_map,
    name: String,
    attrs: libbpf_sys::bpf_create_map_attr,
    initial_val: Option<Vec<u8>>,
}

impl MapBuilder {
    fn new(ptr: *mut libbpf_sys::bpf_map, name: String, btf_fd: i32) -> Self {
        // bpf_map__def can return null but only if it's passed a null. Object::map
        // already error checks that condition for us.
        let def = unsafe { ptr::read(libbpf_sys::bpf_map__def(ptr)) };

        let mut attrs = libbpf_sys::bpf_create_map_attr::default();
        attrs.map_type = def.type_;
        attrs.key_size = def.key_size;
        attrs.value_size = def.value_size;
        attrs.max_entries = def.max_entries;
        attrs.map_flags = def.map_flags;

        if btf_fd >= 0 {
            attrs.btf_fd = btf_fd as u32;
            attrs.btf_value_type_id = unsafe { libbpf_sys::bpf_map__btf_value_type_id(ptr) };
            attrs.btf_key_type_id = unsafe { libbpf_sys::bpf_map__btf_key_type_id(ptr) };
        }

        MapBuilder {
            ptr,
            attrs,
            name,
            initial_val: None,
        }
    }

    pub fn set_map_ifindex(&mut self, idx: u32) -> &mut Self {
        self.attrs.map_ifindex = idx;
        self
    }

    pub fn set_max_entries(&mut self, entries: u32) -> &mut Self {
        self.attrs.max_entries = entries;
        self
    }

    pub fn set_initial_value(&mut self, data: &[u8]) -> &mut Self {
        self.initial_val = Some(data.to_vec());
        self
    }

    pub fn set_numa_node(&mut self, node: u32) -> &mut Self {
        self.attrs.numa_node = node;
        self
    }

    pub fn set_inner_map_fd(&mut self, inner: Map) -> &mut Self {
        self.attrs.__bindgen_anon_1.inner_map_fd = inner.fd;
        mem::forget(inner);
        self
    }

    pub fn set_flags(&mut self, flags: MapBuilderFlags) -> &mut Self {
        self.attrs.map_flags = flags.bits;
        self
    }

    pub fn load(&mut self) -> Result<Map> {
        if let Some(val) = &self.initial_val {
            let ret = unsafe {
                libbpf_sys::bpf_map__set_initial_value(
                    self.ptr,
                    val.as_ptr() as *const std::ffi::c_void,
                    val.len() as u64,
                )
            };
            if ret != 0 {
                // Error code is returned negative, flip to positive to match errno
                return Err(Error::System(-ret));
            }
        }

        let fd = unsafe { libbpf_sys::bpf_create_map_xattr(&self.attrs) };
        if fd < 0 {
            Err(Error::System(errno::errno()))
        } else {
            Ok(Map::new(fd as u32))
        }
    }
}

#[rustfmt::skip]
bitflags! {
    pub struct MapBuilderFlags: u32 {
	const NO_PREALLOC     = 1;
	const NO_COMMON_LRU   = 1 << 1;
	const NUMA_NODE       = 1 << 2;
	const RDONLY          = 1 << 3;
	const WRONLY          = 1 << 4;
	const STACK_BUILD_ID  = 1 << 5;
	const ZERO_SEED       = 1 << 6;
	const RDONLY_PROG     = 1 << 7;
	const WRONLY_PROG     = 1 << 8;
	const CLONE           = 1 << 9;
	const MMAPABLE        = 1 << 10;
    }
}

/// Represents a created map.
///
/// The kernel ensure the atomicity and safety of operations on a `Map`. Therefore,
/// this handle is safe to clone and pass around between threads. This is essentially a
/// file descriptor.
///
/// Some methods require working with raw bytes. You may find libraries such as
/// [`plain`](https://crates.io/crates/plain) helpful.
#[derive(Clone)]
pub struct Map {
    fd: u32,
}

impl Map {
    fn new(fd: u32) -> Self {
        Map { fd }
    }

    pub fn name(&self) -> &str {
        unimplemented!();
    }

    /// Returns a file descriptor to the underlying map.
    pub fn fd(&self) -> i32 {
        unimplemented!();
    }

    pub fn map_type(&self) -> MapType {
        unimplemented!();
    }

    /// Key size in bytes
    pub fn key_size(&self) -> u32 {
        unimplemented!();
    }

    /// Value size in bytes
    pub fn value_size(&self) -> u32 {
        unimplemented!();
    }

    /// Returns map value as `Vec` of `u8`.
    ///
    /// `key` must have exactly [`Map::key_size()`] elements.
    pub fn lookup(&self, _key: &[u8], _flags: MapFlags) -> Result<Option<Vec<u8>>> {
        unimplemented!();
    }

    /// Deletes an element from the map.
    ///
    /// `key` must have exactly [`Map::key_size()`] elements.
    pub fn delete(&mut self, _key: &[u8]) -> Result<()> {
        unimplemented!();
    }

    /// Same as [`Map::lookup()`] except this also deletes the key from the map.
    ///
    /// `key` must have exactly [`Map::key_size()`] elements.
    pub fn lookup_and_delete(&mut self, _key: &[u8], _flags: MapFlags) -> Result<Option<Vec<u8>>> {
        unimplemented!();
    }

    /// Update an element.
    ///
    /// `key` must have exactly [`Map::key_size()`] elements. `value` must have exatly
    /// [`Map::value_size()`] elements.
    pub fn update(&mut self, _key: &[u8], _value: &[u8], _flags: MapFlags) -> Result<()> {
        unimplemented!();
    }
}

#[rustfmt::skip]
bitflags! {
    /// Flags to configure [`Map`] operations.
    pub struct MapFlags: u64 {
	const ANY      = 0;
	const NO_EXIST = 1;
	const EXIST    = 1 << 1;
	const LOCK     = 1 << 2;
    }
}

/// Type of a [`Map`]. Maps to `enum bpf_map_type` in kernel uapi.
#[non_exhaustive]
pub enum MapType {}

/// Represents a parsed but not yet loaded BPF program.
pub struct ProgramBuilder {
    _ptr: *mut libbpf_sys::bpf_program,
}

impl ProgramBuilder {
    fn new(_ptr: *mut libbpf_sys::bpf_program) -> Self {
        ProgramBuilder { _ptr }
    }

    pub fn set_prog_type(&mut self, _prog_type: ProgramType) -> &mut Self {
        unimplemented!();
    }

    pub fn set_attach_type(&mut self, _attach_type: ProgramAttachType) -> &mut Self {
        unimplemented!();
    }

    pub fn set_ifindex(&mut self, _idx: i32) -> &mut Self {
        unimplemented!();
    }

    // TODO: more flags here:
    // https://github.com/torvalds/linux/blob/master/include/uapi/linux/bpf.h#L267

    pub fn load(&mut self) -> Result<Program> {
        unimplemented!();
    }
}

/// Type of a [`Program`]. Maps to `enum bpf_prog_type` in kernel uapi.
#[non_exhaustive]
pub enum ProgramType {}

/// Attach type of a [`Program`]. Maps to `enum bpf_attach_type` in kernel uapi.
#[non_exhaustive]
pub enum ProgramAttachType {}

/// Represents a loaded [`Program`].
///
/// The kernel ensure the atomicity and safety of operations on a `Program`. Therefore,
/// this handle is safe to clone and pass around between threads. This is essentially a
/// file descriptor.
///
/// If you attempt to attach a `Program` with the wrong attach method, the `attach_*`
/// method will fail with the appropriate error.
#[derive(Clone)]
pub struct Program {}

impl Program {
    pub fn name(&self) -> &str {
        unimplemented!();
    }

    /// Name of the section this `Program` belongs to.
    pub fn section(&self) -> &str {
        unimplemented!();
    }

    pub fn prog_type(&self) -> ProgramType {
        unimplemented!();
    }

    /// Returns a file descriptor to the underlying program.
    pub fn fd(&self) -> i32 {
        unimplemented!();
    }

    pub fn attach_type(&self) -> ProgramAttachType {
        unimplemented!();
    }

    pub fn attach_cgroup(&mut self, _cgroup_fd: i32, _flags: CgroupAttachFlags) -> Result<Link> {
        unimplemented!();
    }

    pub fn attach_perf_event(&mut self, _pfd: i32) -> Result<Link> {
        unimplemented!();
    }
}

#[rustfmt::skip]
bitflags! {
    pub struct CgroupAttachFlags: u64 {
	const ALLOW_OVERRIDE   = 1;
	const ALLOW_MULTI      = 1 << 1;
	const REPLACE          = 1 << 2;
    }
}
