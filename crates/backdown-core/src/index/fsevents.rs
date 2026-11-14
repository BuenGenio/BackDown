//! macOS's FSEvents history: every volume macOS writes to keeps a log of what changed on it (in
//! `.fseventsd` at its top), which survives restarts and unplugging, under a UUID that changes
//! whenever the history is reset or lost.
//!
//! A scan notes the volume's history UUID and the last event id when it begins. The next rescan
//! checks the UUID is the same, then replays the volume's history since that id, file by file
//! (`kFSEventStreamCreateFlagFileEvents`), until the history is done. Each event names a path;
//! an event that says "something in here changed, look at all of it" (`MustScanSubDirs`) names a
//! folder to walk whole, and one that says events were dropped, or the ids wrapped, or the root
//! moved, means a walk of everything.
//!
//! Only APFS and HFS+ volumes are used: their inode numbers aren't reused (on FAT and exFAT they
//! are made up from where a file sits, and those are drives other systems write to without
//! keeping any history). The history doesn't know about changes made while the drive was
//! plugged into something other than a Mac; the regular full walk (`Settings`) catches those.
//!
//! The FSEvents functions live in the CoreServices framework, which the macOS build can't link
//! against (it's linked by zig, without Apple's SDK), so `sys` loads them when first needed with
//! `dlopen`; the binary itself links only libSystem.

use std::path::{Path, PathBuf};

use super::ChangeSet;

pub const MUST_SCAN_SUBDIRS: u32 = 0x0000_0001;
pub const USER_DROPPED: u32 = 0x0000_0002;
pub const KERNEL_DROPPED: u32 = 0x0000_0004;
pub const EVENT_IDS_WRAPPED: u32 = 0x0000_0008;
pub const HISTORY_DONE: u32 = 0x0000_0010;
pub const ROOT_CHANGED: u32 = 0x0000_0020;
pub const MOUNT: u32 = 0x0000_0040;
pub const UNMOUNT: u32 = 0x0000_0080;
pub const ITEM_CREATED: u32 = 0x0000_0100;
pub const ITEM_REMOVED: u32 = 0x0000_0200;
pub const ITEM_INODE_META_MOD: u32 = 0x0000_0400;
pub const ITEM_RENAMED: u32 = 0x0000_0800;
pub const ITEM_MODIFIED: u32 = 0x0000_1000;
pub const ITEM_IS_FILE: u32 = 0x0001_0000;
pub const ITEM_IS_DIR: u32 = 0x0002_0000;
pub const ITEM_IS_SYMLINK: u32 = 0x0004_0000;

/// One event of the history: the path (the volume's mount point joined with the path the history
/// gives, relative to the volume), what happened, its id, and the inode it was about (FSEvents
/// gives it with extended data).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub path: PathBuf,
    pub flags: u32,
    pub id: u64,
    pub inode: Option<u64>,
}

/// The change set events come to, for `root`. The root is always looked at (cheap), so a root
/// folder that isn't the one on record any more is noticed.
pub fn changes<'e>(root: &Path, events: impl IntoIterator<Item = &'e Event>) -> ChangeSet {
    let mut out = ChangeSet::new();
    out.path(root.to_path_buf());
    for e in events {
        let f = e.flags;
        if f & (USER_DROPPED | KERNEL_DROPPED) != 0 {
            out.everything("the change history dropped events");
        } else if f & EVENT_IDS_WRAPPED != 0 {
            out.everything("the change history's event ids wrapped around");
        } else if f & ROOT_CHANGED != 0 {
            out.everything("the folder being watched was moved or deleted");
        } else if f & HISTORY_DONE != 0 {
            continue;
        } else if f & MUST_SCAN_SUBDIRS != 0 {
            out.subtree(e.path.clone());
        } else {
            // a file or folder event, or (without file events) a folder whose entries changed:
            // either way that path is looked at again, and a folder is listed again
            out.path(e.path.clone());
            // a file changed through one hard link changed under its other names too, and the
            // name it was changed through may be gone since
            if let Some(ino) = e.inode
                && f & ITEM_IS_DIR == 0
            {
                out.inode(ino);
            }
        }
    }
    out
}

/// The path of an event given relative to its volume, under the volume's mount point.
pub fn absolute(mount: &Path, rel: &str) -> PathBuf {
    // name by name, so the path is spelt as the index spells it (`//` collapsed)
    rel.split('/')
        .filter(|name| !name.is_empty())
        .fold(mount.to_path_buf(), |p, name| p.join(name))
}

// ---------- macOS

#[cfg(target_os = "macos")]
pub(crate) mod sys {
    //! FSEvents through `dlopen`: the volume's history UUID and last event id, and a replay of its
    //! history on a thread of its own with a run loop, until the history is done.

    use std::ffi::{CStr, CString, c_char, c_void};
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    use super::super::ChangeSet;
    use super::super::journal::Cursor;
    use super::{Event, HISTORY_DONE};

    type CFTypeRef = *const c_void;
    type CFAllocatorRef = *const c_void;
    type CFStringRef = *const c_void;
    type CFArrayRef = *const c_void;
    type CFRunLoopRef = *const c_void;
    type FSEventStreamRef = *mut c_void;
    type CFAbsoluteTime = f64;
    type Callback =
        extern "C" fn(FSEventStreamRef, *mut c_void, usize, *mut c_void, *const u32, *const u64);

    #[repr(C)]
    struct Context {
        version: isize,
        info: *mut c_void,
        retain: *const c_void,
        release: *const c_void,
        copy_description: *const c_void,
    }

    const RTLD_LAZY: i32 = 0x1;
    const UTF8: u32 = 0x0800_0100; // kCFStringEncodingUTF8
    const SINT64: isize = 4; // kCFNumberSInt64Type
    const USE_CF_TYPES: u32 = 0x01; // kFSEventStreamCreateFlagUseCFTypes
    const NO_DEFER: u32 = 0x02; // kFSEventStreamCreateFlagNoDefer
    const FILE_EVENTS: u32 = 0x10; // kFSEventStreamCreateFlagFileEvents
    const EXTENDED_DATA: u32 = 0x40; // kFSEventStreamCreateFlagUseExtendedData: path and inode

    unsafe extern "C" {
        fn dlopen(path: *const c_char, mode: i32) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    }

    /// The functions, looked up once.
    struct Api {
        create: unsafe extern "C" fn(
            CFAllocatorRef,
            Callback,
            *const Context,
            i32,
            CFArrayRef,
            u64,
            f64,
            u32,
        ) -> FSEventStreamRef,
        schedule: unsafe extern "C" fn(FSEventStreamRef, CFRunLoopRef, CFStringRef),
        start: unsafe extern "C" fn(FSEventStreamRef) -> u8,
        stop: unsafe extern "C" fn(FSEventStreamRef),
        invalidate: unsafe extern "C" fn(FSEventStreamRef),
        release_stream: unsafe extern "C" fn(FSEventStreamRef),
        copy_uuid: unsafe extern "C" fn(i32) -> CFTypeRef,
        last_before: unsafe extern "C" fn(i32, CFAbsoluteTime) -> u64,
        string: unsafe extern "C" fn(CFAllocatorRef, *const c_char, u32) -> CFStringRef,
        array: unsafe extern "C" fn(
            CFAllocatorRef,
            *const CFTypeRef,
            isize,
            *const c_void,
        ) -> CFArrayRef,
        release: unsafe extern "C" fn(CFTypeRef),
        uuid_string: unsafe extern "C" fn(CFAllocatorRef, CFTypeRef) -> CFStringRef,
        cstring: unsafe extern "C" fn(CFStringRef, *mut c_char, isize, u32) -> u8,
        current_loop: unsafe extern "C" fn() -> CFRunLoopRef,
        run_in_mode: unsafe extern "C" fn(CFStringRef, f64, u8) -> i32,
        now: unsafe extern "C" fn() -> CFAbsoluteTime,
        array_count: unsafe extern "C" fn(CFArrayRef) -> isize,
        array_at: unsafe extern "C" fn(CFArrayRef, isize) -> CFTypeRef,
        dict_get: unsafe extern "C" fn(CFTypeRef, CFTypeRef) -> CFTypeRef,
        number: unsafe extern "C" fn(CFTypeRef, isize, *mut c_void) -> u8,
        default_mode: *const CFStringRef,
        array_callbacks: *const c_void,
    }

    // SAFETY: function pointers and pointers to immutable framework constants
    unsafe impl Send for Api {}
    unsafe impl Sync for Api {}

    fn api() -> Result<&'static Api, String> {
        static API: OnceLock<Result<Api, String>> = OnceLock::new();
        API.get_or_init(load).as_ref().map_err(Clone::clone)
    }

    fn load() -> Result<Api, String> {
        let open = |p: &str| {
            let c = CString::new(p).expect("no NUL");
            // SAFETY: a NUL-terminated path; the result is checked
            let h = unsafe { dlopen(c.as_ptr(), RTLD_LAZY) };
            (!h.is_null()).then_some(h).ok_or(format!("can't load {p}"))
        };
        let cs = open("/System/Library/Frameworks/CoreServices.framework/CoreServices")?;
        let cf = open("/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation")?;
        let sym = |h: *mut c_void, name: &str| -> Result<*mut c_void, String> {
            let c = CString::new(name).expect("no NUL");
            // SAFETY: an open handle and a NUL-terminated name; the result is checked
            let p = unsafe { dlsym(h, c.as_ptr()) };
            (!p.is_null())
                .then_some(p)
                .ok_or(format!("{name} isn't there"))
        };
        // SAFETY (for every `func` below): each symbol is the documented function of that name,
        // and the field it goes in has that function's documented signature
        unsafe {
            Ok(Api {
                create: func(sym(cs, "FSEventStreamCreateRelativeToDevice")?),
                schedule: func(sym(cs, "FSEventStreamScheduleWithRunLoop")?),
                start: func(sym(cs, "FSEventStreamStart")?),
                stop: func(sym(cs, "FSEventStreamStop")?),
                invalidate: func(sym(cs, "FSEventStreamInvalidate")?),
                release_stream: func(sym(cs, "FSEventStreamRelease")?),
                copy_uuid: func(sym(cs, "FSEventsCopyUUIDForDevice")?),
                last_before: func(sym(cs, "FSEventsGetLastEventIdForDeviceBeforeTime")?),
                string: func(sym(cf, "CFStringCreateWithCString")?),
                array: func(sym(cf, "CFArrayCreate")?),
                release: func(sym(cf, "CFRelease")?),
                uuid_string: func(sym(cf, "CFUUIDCreateString")?),
                cstring: func(sym(cf, "CFStringGetCString")?),
                current_loop: func(sym(cf, "CFRunLoopGetCurrent")?),
                run_in_mode: func(sym(cf, "CFRunLoopRunInMode")?),
                now: func(sym(cf, "CFAbsoluteTimeGetCurrent")?),
                array_count: func(sym(cf, "CFArrayGetCount")?),
                array_at: func(sym(cf, "CFArrayGetValueAtIndex")?),
                dict_get: func(sym(cf, "CFDictionaryGetValue")?),
                number: func(sym(cf, "CFNumberGetValue")?),
                default_mode: sym(cf, "kCFRunLoopDefaultMode")? as *const CFStringRef,
                array_callbacks: sym(cf, "kCFTypeArrayCallBacks")? as *const c_void,
            })
        }
    }

    /// A looked-up symbol as a function pointer of type `F`.
    ///
    /// # Safety
    /// The symbol must be a function whose signature is `F`.
    unsafe fn func<F: Copy>(p: *mut c_void) -> F {
        assert_eq!(std::mem::size_of::<F>(), std::mem::size_of::<*mut c_void>());
        // SAFETY: same size (checked), and the caller vouches for the signature
        unsafe { std::mem::transmute_copy::<*mut c_void, F>(&p) }
    }

    /// The volume a path is on: its mount point, filesystem type and device number.
    fn volume_of(path: &Path) -> Result<(PathBuf, String, i32), String> {
        use std::os::unix::fs::MetadataExt;
        let c = CString::new(path.as_os_str().as_bytes()).map_err(|_| "a NUL in the path")?;
        // SAFETY: a NUL-terminated path and a zeroed buffer of the right type
        let st = unsafe {
            let mut st: libc::statfs = std::mem::zeroed();
            if libc::statfs(c.as_ptr(), &mut st) != 0 {
                return Err(format!("can't read {}'s volume", path.display()));
            }
            st
        };
        let text = |b: &[c_char]| {
            // SAFETY: statfs fills NUL-terminated strings within the arrays
            unsafe { CStr::from_ptr(b.as_ptr()) }
                .to_string_lossy()
                .into_owned()
        };
        let dev = std::fs::metadata(path).map_err(|e| e.to_string())?.dev() as i32;
        Ok((
            PathBuf::from(text(&st.f_mntonname)),
            text(&st.f_fstypename),
            dev,
        ))
    }

    /// The root's path relative to its volume (through the firmlinks that put /Users and the
    /// like on the data volume, mounted at /System/Volumes/Data).
    fn relative(root: &Path, mount: &Path) -> Option<String> {
        use std::os::unix::fs::MetadataExt;
        let want = std::fs::metadata(root).ok()?;
        let candidates = [root.strip_prefix(mount).ok(), root.strip_prefix("/").ok()];
        candidates.into_iter().flatten().find_map(|rel| {
            let m = std::fs::metadata(mount.join(rel)).ok()?;
            (m.dev() == want.dev() && m.ino() == want.ino())
                .then(|| rel.to_string_lossy().into_owned())
        })
    }

    fn uuid(api: &Api, dev: i32) -> Option<String> {
        // SAFETY: documented calls; every object made here is released
        unsafe {
            let u = (api.copy_uuid)(dev);
            if u.is_null() {
                return None;
            }
            let s = (api.uuid_string)(std::ptr::null(), u);
            (api.release)(u);
            if s.is_null() {
                return None;
            }
            let mut buf = [0 as c_char; 64];
            let ok = (api.cstring)(s, buf.as_mut_ptr(), buf.len() as isize, UTF8);
            (api.release)(s);
            (ok != 0).then(|| CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned())
        }
    }

    pub(crate) fn cursor(root: &Path) -> Result<Option<Cursor>, String> {
        let (_, fs, dev) = volume_of(root)?;
        if fs != "apfs" && fs != "hfs" {
            return Ok(None);
        }
        let api = api()?;
        let Some(uuid) = uuid(api, dev) else {
            return Ok(None); // no history kept for this volume
        };
        // SAFETY: documented calls with plain values
        let event = unsafe { (api.last_before)(dev, (api.now)()) };
        Ok(Some(Cursor::Fsevents { uuid, event }))
    }

    /// What the callback collects: (path, flags, event id, inode).
    type Raw = (String, u32, u64, Option<u64>);

    struct Collect {
        api: &'static Api,
        /// the extended data's keys: "path" and "fileID"
        key_path: CFStringRef,
        key_id: CFStringRef,
        events: Vec<Raw>,
        /// an event whose path couldn't be read
        unreadable: bool,
        done: bool,
    }

    extern "C" fn callback(
        _: FSEventStreamRef,
        info: *mut c_void,
        n: usize,
        data: *mut c_void,
        flags: *const u32,
        ids: *const u64,
    ) {
        // SAFETY: FSEvents passes, with extended data, a CFArray of `n` dictionaries (a path and
        // a file id each), `n` flags and ids, and our own `Collect` as info
        unsafe {
            let c = &mut *(info as *mut Collect);
            let api = c.api;
            let arr = data as CFArrayRef;
            let n = n.min((api.array_count)(arr).max(0) as usize);
            for i in 0..n {
                let f = *flags.add(i);
                if f & HISTORY_DONE != 0 {
                    c.done = true;
                }
                let d = (api.array_at)(arr, i as isize);
                let s = (api.dict_get)(d, c.key_path);
                let mut buf = vec![0 as c_char; 16 << 10];
                if s.is_null() || (api.cstring)(s, buf.as_mut_ptr(), buf.len() as isize, UTF8) == 0
                {
                    c.unreadable |= f & HISTORY_DONE == 0;
                    continue;
                }
                let p = CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned();
                let num = (api.dict_get)(d, c.key_id);
                let mut ino = 0i64;
                let ino = (!num.is_null()
                    && (api.number)(num, SINT64, &mut ino as *mut i64 as *mut c_void) != 0)
                    .then_some(ino as u64);
                c.events.push((p, f, *ids.add(i), ino));
            }
        }
    }

    /// Replays the volume's history since `since`, on a thread with its own run loop.
    fn replay(dev: i32, rel: String, since: u64) -> Result<Vec<Raw>, String> {
        let api = api()?;
        let worker = std::thread::spawn(move || -> Result<Vec<Raw>, String> {
            let rel = CString::new(rel).map_err(|_| "a NUL in the path")?;
            let key = |k: &str| {
                let k = CString::new(k).expect("no NUL");
                // SAFETY: a NUL-terminated string; released below
                unsafe { (api.string)(std::ptr::null(), k.as_ptr(), UTF8) }
            };
            // only ever reached through this pointer (here and in the callback), freed below
            let c = Box::into_raw(Box::new(Collect {
                api,
                key_path: key("path"),
                key_id: key("fileID"),
                events: Vec::new(),
                unreadable: false,
                done: false,
            }));
            let ctx = Context {
                version: 0,
                info: c as *mut c_void,
                retain: std::ptr::null(),
                release: std::ptr::null(),
                copy_description: std::ptr::null(),
            };
            // SAFETY: documented calls; the stream and every object made here are released
            // before `c` is freed, and the callback only runs inside CFRunLoopRunInMode below
            let started = unsafe {
                let s = (api.string)(std::ptr::null(), rel.as_ptr(), UTF8);
                let items = [s];
                let arr = (api.array)(std::ptr::null(), items.as_ptr(), 1, api.array_callbacks);
                (api.release)(s);
                let stream = (api.create)(
                    std::ptr::null(),
                    callback,
                    &ctx,
                    dev,
                    arr,
                    since,
                    0.0,
                    FILE_EVENTS | NO_DEFER | USE_CF_TYPES | EXTENDED_DATA,
                );
                (api.release)(arr);
                if stream.is_null() {
                    Err("the change history can't be opened")
                } else {
                    (api.schedule)(stream, (api.current_loop)(), *api.default_mode);
                    let ok = (api.start)(stream) != 0;
                    if ok {
                        let until = Instant::now() + Duration::from_secs(120);
                        while !(*c).done && Instant::now() < until {
                            (api.run_in_mode)(*api.default_mode, 0.5, 0);
                        }
                        (api.stop)(stream);
                    }
                    (api.invalidate)(stream);
                    (api.release_stream)(stream);
                    if ok {
                        Ok(())
                    } else {
                        Err("the change history can't be replayed")
                    }
                }
            };
            // SAFETY: made by Box::into_raw above; the stream that used it is gone; the keys were
            // made above and nothing else holds them
            let c = unsafe {
                let c = Box::from_raw(c);
                (api.release)(c.key_path);
                (api.release)(c.key_id);
                c
            };
            started?;
            if !c.done {
                return Err("replaying the change history took too long".into());
            }
            if c.unreadable {
                return Err("the change history named a path that can't be read".into());
            }
            Ok(c.events)
        });
        worker
            .join()
            .map_err(|_| "replaying the change history failed".to_string())?
    }

    pub(crate) fn changes(root: &Path, from: &Cursor, to: &Cursor) -> Result<ChangeSet, String> {
        let (Cursor::Fsevents { uuid: was, event }, Cursor::Fsevents { uuid: now, .. }) =
            (from, to)
        else {
            return Err("the previous scan noted another kind of change log".into());
        };
        if was != now {
            return Err(
                "the drive's change history was reset (or it's another drive) since the previous scan"
                    .into(),
            );
        }
        let (mount, _, dev) = volume_of(root)?;
        let rel = relative(root, &mount).ok_or("can't tell where the root is on its volume")?;
        // the root as the history spells it: under the volume's mount point
        let on_volume = super::absolute(&mount, &rel);
        let raw = replay(dev, rel, *event)?;
        let events: Vec<Event> = raw
            .into_iter()
            .map(|(p, flags, id, inode)| Event {
                path: super::absolute(&mount, &p),
                flags,
                id,
                inode,
            })
            .collect();
        let out = super::changes(&on_volume, &events).within(&on_volume);
        Ok(if on_volume == root {
            out
        } else {
            rebase(out, &on_volume, root)
        })
    }

    /// Moves a change set's paths from one spelling of the root to another.
    fn rebase(c: ChangeSet, from: &Path, to: &Path) -> ChangeSet {
        let move_one = |p: PathBuf| match p.strip_prefix(from) {
            Ok(rest) => to.join(rest),
            Err(_) => p,
        };
        ChangeSet {
            paths: c.paths.into_iter().map(move_one).collect(),
            subtrees: c.subtrees.into_iter().map(move_one).collect(),
            inodes: c.inodes,
            full: c.full,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_become_paths() {
        use crate::paths::native;
        let root = &native("/Volumes/Drive/Photos");
        let e = |p: &str, flags| Event {
            path: absolute(&native("/Volumes/Drive"), p),
            flags,
            id: 1,
            inode: (flags & ITEM_IS_FILE != 0).then_some(42),
        };
        let evs = vec![
            e("Photos/2019/a.jpg", ITEM_CREATED | ITEM_IS_FILE),
            e("/Photos/2020", ITEM_RENAMED | ITEM_IS_DIR),
            e("Photos/raw", MUST_SCAN_SUBDIRS),
            e("Music/x.mp3", ITEM_MODIFIED | ITEM_IS_FILE),
            e("", HISTORY_DONE),
        ];
        let c = changes(root, &evs).within(root);
        // as spelt, not just as compared (`/` and `\` are one separator to a Windows path)
        let spelt = |ps: &[&str]| -> Vec<String> {
            ps.iter().map(|p| native(p).display().to_string()).collect()
        };
        let got: Vec<String> = c.paths.iter().map(|p| p.display().to_string()).collect();
        assert_eq!(
            got,
            spelt(&[
                "/Volumes/Drive/Photos",
                "/Volumes/Drive/Photos/2019/a.jpg",
                "/Volumes/Drive/Photos/2020"
            ])
        );
        assert_eq!(
            c.subtrees.iter().next().unwrap().display().to_string(),
            spelt(&["/Volumes/Drive/Photos/raw"])[0]
        );
        assert!(c.full.is_none());
        assert_eq!(c.inodes.iter().copied().collect::<Vec<_>>(), [42]);
        for bad in [
            USER_DROPPED,
            KERNEL_DROPPED,
            EVENT_IDS_WRAPPED,
            ROOT_CHANGED,
        ] {
            assert!(changes(root, &[e("Photos", bad)]).full.is_some());
        }
    }
}
