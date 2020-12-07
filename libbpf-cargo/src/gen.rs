use std::collections::BTreeMap;
use std::ffi::{CStr, CString};
use std::fmt::Write as fmt_write;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::ptr;

use anyhow::{bail, Result};

use crate::metadata;
use crate::metadata::UnprocessedObj;

macro_rules! gen_bpf_object_iter {
    ($name:ident, $iter_ty:ty, $next_fn:expr) => {
        struct $name {
            obj: *mut libbpf_sys::bpf_object,
            last: *mut $iter_ty,
        }

        impl $name {
            fn new(obj: *mut libbpf_sys::bpf_object) -> $name {
                $name {
                    obj,
                    last: ptr::null_mut(),
                }
            }
        }

        impl Iterator for $name {
            type Item = *mut $iter_ty;

            fn next(&mut self) -> Option<Self::Item> {
                self.last = unsafe { $next_fn(self.last, self.obj) };

                if self.last.is_null() {
                    None
                } else {
                    Some(self.last)
                }
            }
        }
    };
}

gen_bpf_object_iter!(MapIter, libbpf_sys::bpf_map, libbpf_sys::bpf_map__next);
gen_bpf_object_iter!(
    ProgIter,
    libbpf_sys::bpf_program,
    libbpf_sys::bpf_program__next
);

/// Run `rustfmt` over `s` and return result
fn rustfmt(s: &str) -> Result<String> {
    let mut cmd = Command::new("rustfmt")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    write!(cmd.stdin.take().unwrap(), "{}", s)?;
    let output = cmd.wait_with_output()?;

    Ok(String::from_utf8(output.stdout)?)
}

fn get_raw_map_name(map: *const libbpf_sys::bpf_map) -> Result<String> {
    let name_ptr = unsafe { libbpf_sys::bpf_map__name(map) };
    if name_ptr.is_null() {
        bail!("Map name unknown");
    }

    Ok(unsafe { CStr::from_ptr(name_ptr) }.to_str()?.to_string())
}

/// Same as `get_raw_map_name` except the name is canonicalized
fn get_map_name(map: *const libbpf_sys::bpf_map) -> Result<String> {
    let name = get_raw_map_name(map)?;

    if unsafe { !libbpf_sys::bpf_map__is_internal(map) } {
        Ok(name.to_string())
    } else if name.ends_with(".data") {
        Ok("data".to_string())
    } else if name.ends_with(".rodata") {
        Ok("rodata".to_string())
    } else if name.ends_with(".bss") {
        Ok("bss".to_string())
    } else if name.ends_with(".kconfig") {
        Ok("kconfig".to_string())
    } else {
        bail!("Unknown map type");
    }
}

fn get_prog_name(prog: *const libbpf_sys::bpf_program) -> Result<String> {
    let name_ptr = unsafe { libbpf_sys::bpf_program__name(prog) };

    if name_ptr.is_null() {
        bail!("Prog name unknown");
    }

    Ok(unsafe { CStr::from_ptr(name_ptr) }.to_str()?.to_string())
}

fn gen_skel_map_defs(
    skel: &mut String,
    object: *mut libbpf_sys::bpf_object,
    obj_name: &str,
    open: bool,
) -> Result<()> {
    if MapIter::new(object).next().is_none() {
        return Ok(());
    }

    let (struct_name, inner_ty, return_ty) = if open {
        (
            format!("Open{}Maps", obj_name),
            "libbpf_rs::OpenObject",
            "libbpf_rs::OpenMap",
        )
    } else {
        (
            format!("{}Maps", obj_name),
            "libbpf_rs::Object",
            "libbpf_rs::Map",
        )
    };

    write!(
        skel,
        r#"
        pub struct {struct_name}<'a> {{
            inner: &'a mut {inner_ty},
        }}

        impl<'a> {struct_name}<'a> {{
        "#,
        inner_ty = inner_ty,
        struct_name = struct_name
    )?;

    for map in MapIter::new(object) {
        write!(
            skel,
            r#"
            pub fn {map_name}(&mut self) -> &mut {return_ty} {{
                self.inner.map_unwrap("{raw_map_name}")
            }}
            "#,
            map_name = get_map_name(map)?,
            raw_map_name = get_raw_map_name(map)?,
            return_ty = return_ty,
        )?;
    }

    writeln!(skel, "}}")?;

    Ok(())
}

fn gen_skel_prog_defs(
    skel: &mut String,
    object: *mut libbpf_sys::bpf_object,
    obj_name: &str,
    open: bool,
) -> Result<()> {
    if ProgIter::new(object).next().is_none() {
        return Ok(());
    }

    let (struct_name, inner_ty, return_ty) = if open {
        (
            format!("Open{}Progs", obj_name),
            "libbpf_rs::OpenObject",
            "libbpf_rs::OpenProgram",
        )
    } else {
        (
            format!("{}Progs", obj_name),
            "libbpf_rs::Object",
            "libbpf_rs::Program",
        )
    };

    write!(
        skel,
        r#"
        pub struct {struct_name}<'a> {{
            inner: &'a mut {inner_ty},
        }}

        impl<'a> {struct_name}<'a> {{
        "#,
        inner_ty = inner_ty,
        struct_name = struct_name
    )?;

    for prog in ProgIter::new(object) {
        write!(
            skel,
            r#"
            pub fn {prog_name}(&mut self) -> &mut {return_ty} {{
                self.inner.prog_unwrap("{prog_name}")
            }}
            "#,
            prog_name = get_prog_name(prog)?,
            return_ty = return_ty,
        )?;
    }

    writeln!(skel, "}}")?;

    Ok(())
}

fn gen_skel_map_getter(
    skel: &mut String,
    object: *mut libbpf_sys::bpf_object,
    obj_name: &str,
    open: bool,
) -> Result<()> {
    if MapIter::new(object).next().is_none() {
        return Ok(());
    }

    let return_ty = if open {
        format!("Open{}Maps", obj_name)
    } else {
        format!("{}Maps", obj_name)
    };

    write!(
        skel,
        r#"
        pub fn maps(&mut self) -> {return_ty} {{
            {return_ty} {{
                inner: &mut self.obj,
            }}
        }}
        "#,
        return_ty = return_ty
    )?;

    Ok(())
}

fn gen_skel_prog_getter(
    skel: &mut String,
    object: *mut libbpf_sys::bpf_object,
    obj_name: &str,
    open: bool,
) -> Result<()> {
    if ProgIter::new(object).next().is_none() {
        return Ok(());
    }

    let return_ty = if open {
        format!("Open{}Progs", obj_name)
    } else {
        format!("{}Progs", obj_name)
    };

    write!(
        skel,
        r#"
        pub fn progs(&mut self) -> {return_ty} {{
            {return_ty} {{
                inner: &mut self.obj,
            }}
        }}
        "#,
        return_ty = return_ty
    )?;

    Ok(())
}

fn gen_skel_link_defs(
    skel: &mut String,
    object: *mut libbpf_sys::bpf_object,
    obj_name: &str,
) -> Result<()> {
    if ProgIter::new(object).next().is_none() {
        return Ok(());
    }

    write!(
        skel,
        r#"
        #[derive(Default)]
        pub struct {}Links {{
        "#,
        obj_name
    )?;

    for prog in ProgIter::new(object) {
        write!(
            skel,
            r#"pub {}: Option<libbpf_rs::Link>,
            "#,
            get_prog_name(prog)?
        )?;
    }

    writeln!(skel, "}}")?;

    Ok(())
}

fn gen_skel_link_getter(
    skel: &mut String,
    object: *mut libbpf_sys::bpf_object,
    obj_name: &str,
) -> Result<()> {
    if ProgIter::new(object).next().is_none() {
        return Ok(());
    }

    write!(
        skel,
        r#"pub links: {}Links,
        "#,
        obj_name
    )?;

    Ok(())
}

fn open_object_file(path: &Path) -> Result<*mut libbpf_sys::bpf_object> {
    if !path.exists() {
        bail!("Object file not found: {}", path.display());
    }
    let path_cstring = CString::new(path.to_string_lossy().into_owned())?;
    let mut obj_opts = libbpf_sys::bpf_object_open_opts::default();
    obj_opts.sz = std::mem::size_of::<libbpf_sys::bpf_object_open_opts>() as libbpf_sys::size_t;
    let object = unsafe { libbpf_sys::bpf_object__open_file(path_cstring.as_ptr(), &obj_opts) };
    if object.is_null() {
        bail!("Could not open object file={}", path.display());
    }

    Ok(object)
}

fn gen_skel_attach(
    skel: &mut String,
    object: *mut libbpf_sys::bpf_object,
    obj_name: &str,
) -> Result<()> {
    if ProgIter::new(object).next().is_none() {
        return Ok(());
    }

    write!(
        skel,
        r#"
        pub fn attach(&mut self) -> libbpf_rs::Result<()> {{
            self.links = {}Links {{
        "#,
        obj_name
    )?;

    for prog in ProgIter::new(object) {
        write!(
            skel,
            r#"{prog_name}: self
                .progs()
                .{prog_name}()
                .attach()
                .map_or_else(
                    |e| {{
                        if let libbpf_rs::Error::System(errno) = e {{
                            // ESRCH: cannot be auto-attached
                            if errno == 3 {{
                                return Ok(None);
                            }}
                        }}

                        Err(e)
                    }},
                    |v| Ok(Some(v))
                )?,
            "#,
            prog_name = get_prog_name(prog)?
        )?;
    }

    write!(
        skel,
        r#"
            }};

            Ok(())
        }}
        "#,
    )?;

    Ok(())
}

/// Generate contents of a single skeleton
fn gen_skel_contents(_debug: bool, obj: &UnprocessedObj) -> Result<String> {
    let mut skel = String::new();

    write!(
        skel,
        r#"// SPDX-License-Identifier: (LGPL-2.1 OR BSD-2-Clause)
           //
           // THIS FILE IS AUTOGENERATED BY CARGO-LIBBPF-GEN!
        "#
    )?;

    let mut obj_file_path = obj.out.clone();
    obj_file_path.push(format!("{}.bpf.o", obj.name));

    write!(
        skel,
        r#"
        const DATA: &[u8] = include_bytes!("{}");
        "#,
        obj_file_path.as_path().display()
    )?;

    // Capitalize object name
    let mut obj_name = String::new();
    // Unwrap is safe b/c already checked that `obj.name` contains chars
    obj_name += &obj.name.chars().next().unwrap().to_uppercase().to_string();
    if obj.name.len() > 1 {
        obj_name += &obj.name[1..];
    }

    // Open bpf_object so we can iterate over maps and progs
    let object = open_object_file(obj_file_path.as_path())?;

    write!(
        skel,
        r#"
        #[derive(Default)]
        pub struct {name}SkelBuilder {{
            pub obj_builder: libbpf_rs::ObjectBuilder,
            name: String,
        }}

        impl {name}SkelBuilder {{
            pub fn name<T: AsRef<str>>(&mut self, name: T) -> &mut Self {{
                self.name = name.as_ref().to_string();
                self.obj_builder.name(name);
                self
            }}

            pub fn relaxed_maps(&mut self, relaxed_maps: bool) -> &mut Self {{
                self.obj_builder.relaxed_maps(relaxed_maps);
                self
            }}

            pub fn debug(&mut self, dbg: bool) -> &mut Self {{
                self.obj_builder.debug(dbg);
                self
            }}

            pub fn open(&mut self) -> libbpf_rs::Result<Open{name}Skel> {{
                Ok(Open{name}Skel {{
                    obj: self.obj_builder.open_memory(&self.name, DATA)?,
                }})
            }}
        }}
        "#,
        name = obj_name
    )?;

    gen_skel_map_defs(&mut skel, object, &obj_name, true)?;
    gen_skel_prog_defs(&mut skel, object, &obj_name, true)?;

    write!(
        skel,
        r#"
        pub struct Open{name}Skel {{
            pub obj: libbpf_rs::OpenObject,
        }}

        impl Open{name}Skel {{
            pub fn load(self) -> libbpf_rs::Result<{name}Skel> {{
                Ok({name}Skel {{
                    obj: self.obj.load()?,
                    {links}
                }})
            }}
        "#,
        name = &obj_name,
        links = if ProgIter::new(object).next().is_some() {
            format!(r#"links: {}Links::default()"#, obj_name)
        } else {
            "".to_string()
        }
    )?;
    gen_skel_prog_getter(&mut skel, object, &obj_name, true)?;
    gen_skel_map_getter(&mut skel, object, &obj_name, true)?;
    writeln!(skel, "}}")?;

    gen_skel_map_defs(&mut skel, object, &obj_name, false)?;
    gen_skel_prog_defs(&mut skel, object, &obj_name, false)?;
    gen_skel_link_defs(&mut skel, object, &obj_name)?;

    write!(
        skel,
        r#"
        pub struct {name}Skel {{
            pub obj: libbpf_rs::Object,
        "#,
        name = &obj_name,
    )?;
    gen_skel_link_getter(&mut skel, object, &obj_name)?;
    write!(
        skel,
        r#"
        }}

        impl {name}Skel {{
        "#,
        name = &obj_name,
    )?;
    gen_skel_prog_getter(&mut skel, object, &obj_name, false)?;
    gen_skel_map_getter(&mut skel, object, &obj_name, false)?;
    gen_skel_attach(&mut skel, object, &obj_name)?;
    writeln!(skel, "}}")?;

    Ok(skel)
}

/// Write a single skeleton to disk
fn gen_skel(debug: bool, obj: &UnprocessedObj) -> Result<()> {
    if obj.name.is_empty() {
        bail!("Object file has no name");
    }

    let skel = rustfmt(&gen_skel_contents(debug, obj)?)?;

    let mut path = obj.path.clone();
    path.pop();
    path.push(format!("{}.skel.rs", obj.name));
    let mut file = File::create(path)?;
    file.write_all(skel.as_bytes())?;

    Ok(())
}

/// Generate mod.rs in src/bpf directory of each project.
///
/// Each `UnprocessedObj` in `objs` must belong to same project.
pub fn gen_mods(objs: &[UnprocessedObj]) -> Result<()> {
    if objs.is_empty() {
        return Ok(());
    }

    let mut path = objs[0].path.clone();
    path.pop();
    path.push("mod.rs");

    let mut contents = String::new();
    write!(
        contents,
        r#"
        // SPDX-License-Identifier: (LGPL-2.1 OR BSD-2-Clause)"
        //
        // THIS FILE IS AUTOGENERATED BY CARGO-LIBBPF-GEN!

        #[allow(dead_code)]
        #[allow(non_snake_case)]

        "#
    )?;

    for obj in objs {
        write!(
            contents,
            r#"
            #[path = "{name}.skel.rs"]
            mod {name}_skel;
            "#,
            name = obj.name
        )?;
    }

    for obj in objs {
        write!(
            contents,
            r#"
            pub use {}_skel::*;
            "#,
            obj.name
        )?;
    }

    let mut file = File::create(path)?;
    file.write_all(rustfmt(&contents)?.as_bytes())?;

    Ok(())
}

pub fn gen(debug: bool, manifest_path: Option<&PathBuf>) -> i32 {
    let to_gen = match metadata::get(debug, manifest_path) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{}", e);
            return 1;
        }
    };

    if debug && !to_gen.is_empty() {
        println!("Found bpf objs to gen skel:");
        for obj in &to_gen {
            println!("\t{:?}", obj);
        }
    } else if to_gen.is_empty() {
        eprintln!("Did not find any bpf objects to generate skeleton");
        return 1;
    }

    // Map to store package_name -> [UnprocessedObj]
    let mut package_objs: BTreeMap<String, Vec<UnprocessedObj>> = BTreeMap::new();

    for obj in to_gen {
        match gen_skel(debug, &obj) {
            Ok(_) => (),
            Err(e) => {
                eprintln!(
                    "Failed to generate skeleton for {}: {}",
                    obj.path.as_path().display(),
                    e
                );
                return 1;
            }
        }

        match package_objs.get_mut(&obj.package) {
            Some(v) => v.push(obj.clone()),
            None => {
                package_objs.insert(obj.package.clone(), vec![obj.clone()]);
            }
        };
    }

    for (package, objs) in package_objs {
        match gen_mods(&objs) {
            Ok(_) => (),
            Err(e) => {
                eprintln!("Failed to generate mod.rs for package={}: {}", package, e);
                return 1;
            }
        }
    }

    0
}
