//! "Allow BackDown to read this drive", without a terminal (Linux): udisks2 hands a process an
//! open, read-only file for one block device (`org.freedesktop.UDisks2.Block.OpenForBackup`)
//! once polkit agrees (`org.freedesktop.udisks2.open-device`, or `open-device-system` for an
//! internal drive). The daemon, which runs as the person at the screen, asks for it; the desktop
//! shows its own password dialog (the usual policy is `auth_admin_keep` even for the active
//! session: an administrator's password, remembered for a few minutes), and the file it gets is
//! kept for this process (`rescue::grant`) and read through by rescues and browsing. It's gone when
//! the daemon stops; the one-off `setfacl` command stays as the other way.
//!
//! The file comes back over D-Bus as a passed file descriptor, which `busctl` or `gdbus` can only
//! print (as a number meaningful in their own process), never hand on. So this is a small D-Bus
//! client of its own: the system bus's socket, SASL EXTERNAL with fd passing negotiated, Hello,
//! then the one method call, marshalled by hand; the reply's descriptor is taken with `recvmsg`
//! (rustix's safe wrapper, already a dependency through fs4). Nothing here writes to a device.

use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result};

/// udisks' object path for a block device: its name with anything but ASCII letters and digits
/// written as `_xx` (hex), as udisks makes them.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn object_path(device: &Path) -> Result<String> {
    let name = device
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|_| device.starts_with("/dev"))
        .with_context(|| format!("{} isn't a device", device.display()))?;
    let mut s = String::from("/org/freedesktop/UDisks2/block_devices/");
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() {
            s.push(b as char);
        } else {
            s.push_str(&format!("_{b:02x}"));
        }
    }
    Ok(s)
}

/// Why a request came back without a file: in words, and whether asking again (with the
/// password dialog) could help.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Denied {
    pub name: String,
    pub message: String,
}

impl Denied {
    /// polkit said no, but would say yes after authentication.
    pub fn needs_auth(&self) -> bool {
        self.name.ends_with("NotAuthorizedCanObtain")
    }

    pub fn dismissed(&self) -> bool {
        self.name.ends_with("NotAuthorizedDismissed")
    }

    /// For people.
    pub fn said(&self) -> String {
        if self.dismissed() {
            "the password dialog was closed without allowing it".into()
        } else if self.needs_auth() {
            "your system wants an administrator's password for that".into()
        } else if self.name.ends_with("NotAuthorized") {
            "your system doesn't allow this account to read drives that way".into()
        } else if self.name.ends_with("DeviceBusy") || self.message.contains("busy") {
            "the drive is in use (mounted?): it can only be opened this way when nothing else has it open".into()
        } else if self.name.ends_with("UnknownObject") || self.name.ends_with("UnknownMethod") {
            "udisks doesn't know that drive (is it still plugged in?)".into()
        } else if self.name.ends_with("ServiceUnknown") {
            "udisks isn't running on this computer".into()
        } else {
            format!("{} ({})", self.message, self.name)
        }
    }
}

impl std::fmt::Display for Denied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.said())
    }
}

impl std::error::Error for Denied {}

/// Asks udisks for a read-only file for `device`. With `interactive`, polkit may ask for a
/// password on the desktop (this waits for it); without, it answers at once.
#[cfg(target_os = "linux")]
pub fn open_for_backup(device: &Path, interactive: bool) -> Result<File> {
    let device =
        dunce::canonicalize(device).with_context(|| format!("can't find {}", device.display()))?;
    let path = object_path(&device)?;
    let mut bus = linux::Bus::system()?;
    let body = wire::options(&[("auth.no_user_interaction", !interactive)]);
    let fd = bus.call_for_fd(
        "org.freedesktop.UDisks2",
        &path,
        "org.freedesktop.UDisks2.Block",
        "OpenForBackup",
        "a{sv}",
        &body,
        interactive,
    )?;
    Ok(File::from(fd))
}

#[cfg(not(target_os = "linux"))]
pub fn open_for_backup(_device: &Path, _interactive: bool) -> Result<File> {
    anyhow::bail!("only Linux's udisks can hand BackDown a drive to read; use the command instead")
}

/// D-Bus messages, marshalled and read (little-endian out; either in).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod wire {
    pub const METHOD_CALL: u8 = 1;
    pub const METHOD_RETURN: u8 = 2;
    pub const ERROR: u8 = 3;
    /// Let the callee ask the user (polkit's password dialog).
    pub const ALLOW_INTERACTIVE: u8 = 0x4;

    #[derive(Default)]
    pub struct W(pub Vec<u8>);

    impl W {
        pub fn pad(&mut self, n: usize) {
            while !self.0.len().is_multiple_of(n) {
                self.0.push(0);
            }
        }
        pub fn u32(&mut self, v: u32) {
            self.pad(4);
            self.0.extend(v.to_le_bytes());
        }
        pub fn str(&mut self, s: &str) {
            self.u32(s.len() as u32);
            self.0.extend(s.as_bytes());
            self.0.push(0);
        }
        pub fn sig(&mut self, s: &str) {
            self.0.push(s.len() as u8);
            self.0.extend(s.as_bytes());
            self.0.push(0);
        }
    }

    /// An `a{sv}` of booleans.
    pub fn options(kv: &[(&str, bool)]) -> Vec<u8> {
        let mut w = W::default();
        w.u32(0);
        w.pad(8);
        let start = w.0.len();
        for (k, v) in kv {
            w.pad(8);
            w.str(k);
            w.sig("b");
            w.u32(u32::from(*v));
        }
        let n = (w.0.len() - start) as u32;
        w.0[..4].copy_from_slice(&n.to_le_bytes());
        w.0
    }

    /// A method call with header fields path, interface, member, destination and signature.
    #[allow(clippy::too_many_arguments)]
    pub fn call(
        serial: u32,
        dest: &str,
        path: &str,
        iface: &str,
        member: &str,
        sig: &str,
        body: &[u8],
        flags: u8,
    ) -> Vec<u8> {
        let mut w = W(vec![b'l', METHOD_CALL, flags, 1]);
        w.u32(body.len() as u32);
        w.u32(serial);
        w.u32(0); // the header fields' length, filled in below
        let start = w.0.len();
        let field = |w: &mut W, code: u8, t: &str, v: &str| {
            w.pad(8);
            w.0.push(code);
            w.sig(t);
            if t == "g" {
                w.sig(v);
            } else {
                w.str(v);
            }
        };
        field(&mut w, 1, "o", path);
        if !iface.is_empty() {
            field(&mut w, 2, "s", iface);
        }
        field(&mut w, 3, "s", member);
        field(&mut w, 6, "s", dest);
        if !sig.is_empty() {
            field(&mut w, 8, "g", sig);
        }
        let n = (w.0.len() - start) as u32;
        w.0[12..16].copy_from_slice(&n.to_le_bytes());
        w.pad(8);
        w.0.extend(body);
        w.0
    }

    /// A message read off the bus.
    #[derive(Debug, Default, Clone, PartialEq, Eq)]
    pub struct Msg {
        pub kind: u8,
        pub serial: u32,
        pub reply_to: Option<u32>,
        pub error: Option<String>,
        pub sig: String,
        pub fds: u32,
        pub big_endian: bool,
        pub body: Vec<u8>,
    }

    impl Msg {
        pub fn u32_at(&self, at: usize) -> Option<u32> {
            let b: [u8; 4] = self.body.get(at..at + 4)?.try_into().ok()?;
            Some(if self.big_endian {
                u32::from_be_bytes(b)
            } else {
                u32::from_le_bytes(b)
            })
        }

        /// The first string of the body (an error's message).
        pub fn first_str(&self) -> Option<String> {
            if !self.sig.starts_with('s') {
                return None;
            }
            let n = self.u32_at(0)? as usize;
            let s = self.body.get(4..4 + n)?;
            Some(String::from_utf8_lossy(s).into_owned())
        }
    }

    /// The first whole message in `buf`, and how many bytes it takes; None when more is needed.
    /// Errors on what can't be a message.
    pub fn parse(buf: &[u8]) -> Result<Option<(usize, Msg)>, String> {
        if buf.len() < 16 {
            return Ok(None);
        }
        let big = match buf[0] {
            b'l' => false,
            b'B' => true,
            x => return Err(format!("not a D-Bus message (endianness {x:#x})")),
        };
        let u32_at = |b: &[u8], at: usize| -> Option<u32> {
            let x: [u8; 4] = b.get(at..at + 4)?.try_into().ok()?;
            Some(if big {
                u32::from_be_bytes(x)
            } else {
                u32::from_le_bytes(x)
            })
        };
        let body_len = u32_at(buf, 4).unwrap_or(0) as usize;
        let fields_len = u32_at(buf, 12).unwrap_or(0) as usize;
        if body_len > 128 << 20 || fields_len > 64 << 10 {
            return Err("a D-Bus message too big to be real".into());
        }
        let hdr_end = 16 + fields_len;
        let body_at = hdr_end.div_ceil(8) * 8;
        let total = body_at + body_len;
        if buf.len() < total {
            return Ok(None);
        }
        let mut m = Msg {
            kind: buf[1],
            serial: u32_at(buf, 8).unwrap_or(0),
            big_endian: big,
            body: buf[body_at..total].to_vec(),
            ..Msg::default()
        };
        let bad = || "a damaged D-Bus header".to_string();
        let mut p = 16;
        while p < hdr_end {
            p = p.div_ceil(8) * 8;
            if p >= hdr_end {
                break;
            }
            let code = buf[p];
            let sl = *buf.get(p + 1).ok_or_else(bad)? as usize;
            let t = buf.get(p + 2..p + 2 + sl).ok_or_else(bad)?.to_vec();
            p += 3 + sl;
            match t.as_slice() {
                b"s" | b"o" => {
                    p = p.div_ceil(4) * 4;
                    let n = u32_at(buf, p).ok_or_else(bad)? as usize;
                    let v = buf.get(p + 4..p + 4 + n).ok_or_else(bad)?;
                    let v = String::from_utf8_lossy(v).into_owned();
                    if code == 4 {
                        m.error = Some(v);
                    }
                    p += 5 + n;
                }
                b"g" => {
                    let n = *buf.get(p).ok_or_else(bad)? as usize;
                    let v = buf.get(p + 1..p + 1 + n).ok_or_else(bad)?;
                    if code == 8 {
                        m.sig = String::from_utf8_lossy(v).into_owned();
                    }
                    p += 2 + n;
                }
                b"u" => {
                    p = p.div_ceil(4) * 4;
                    let v = u32_at(buf, p).ok_or_else(bad)?;
                    match code {
                        5 => m.reply_to = Some(v),
                        9 => m.fds = v,
                        _ => {}
                    }
                    p += 4;
                }
                _ => return Err(bad()),
            }
        }
        Ok(Some((total, m)))
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::VecDeque;
    use std::io::{IoSliceMut, Read, Write};
    use std::mem::MaybeUninit;
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    use anyhow::{Context, Result, bail};
    use rustix::net::{RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, recvmsg};

    use super::{Denied, wire};

    pub struct Bus {
        s: UnixStream,
        buf: Vec<u8>,
        fds: VecDeque<OwnedFd>,
        serial: u32,
    }

    impl Bus {
        /// The system bus, authenticated as this process's user, with fd passing agreed.
        pub fn system() -> Result<Bus> {
            let addr = std::env::var("DBUS_SYSTEM_BUS_ADDRESS").ok();
            let path = addr
                .as_deref()
                .and_then(|a| {
                    a.split(';')
                        .find_map(|x| x.strip_prefix("unix:path="))
                        .map(|p| p.split(',').next().unwrap_or(p).to_string())
                })
                .unwrap_or_else(|| "/var/run/dbus/system_bus_socket".into());
            let s = UnixStream::connect(&path)
                .with_context(|| format!("can't reach the system bus at {path}"))?;
            Bus::over(s)
        }

        /// Talks D-Bus over `s` (authenticating first).
        pub fn over(mut s: UnixStream) -> Result<Bus> {
            s.set_write_timeout(Some(Duration::from_secs(10)))?;
            s.set_read_timeout(Some(Duration::from_secs(10)))?;
            let uid = rustix::process::getuid().as_raw().to_string();
            let hex: String = uid.bytes().map(|b| format!("{b:02x}")).collect();
            s.write_all(format!("\0AUTH EXTERNAL {hex}\r\n").as_bytes())?;
            let ok = line(&mut s)?;
            if !ok.starts_with("OK ") {
                bail!("the system bus didn't accept this account ({ok})");
            }
            s.write_all(b"NEGOTIATE_UNIX_FD\r\n")?;
            let agree = line(&mut s)?;
            if agree != "AGREE_UNIX_FD" {
                bail!("the system bus won't pass open files ({agree})");
            }
            s.write_all(b"BEGIN\r\n")?;
            let mut bus = Bus {
                s,
                buf: Vec::new(),
                fds: VecDeque::new(),
                serial: 0,
            };
            bus.send(
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "Hello",
                "",
                &[],
                0,
            )?;
            Ok(bus)
        }

        #[allow(clippy::too_many_arguments)]
        fn send(
            &mut self,
            dest: &str,
            path: &str,
            iface: &str,
            member: &str,
            sig: &str,
            body: &[u8],
            flags: u8,
        ) -> Result<u32> {
            self.serial += 1;
            let m = wire::call(self.serial, dest, path, iface, member, sig, body, flags);
            self.s.write_all(&m)?;
            Ok(self.serial)
        }

        /// Calls a method whose reply is one file descriptor (`h`), and takes it.
        #[allow(clippy::too_many_arguments)]
        pub fn call_for_fd(
            &mut self,
            dest: &str,
            path: &str,
            iface: &str,
            member: &str,
            sig: &str,
            body: &[u8],
            interactive: bool,
        ) -> Result<OwnedFd> {
            let flags = if interactive {
                wire::ALLOW_INTERACTIVE
            } else {
                0
            };
            let serial = self.send(dest, path, iface, member, sig, body, flags)?;
            // a password dialog can take a while
            self.s.set_read_timeout(Some(if interactive {
                Duration::from_secs(300)
            } else {
                Duration::from_secs(30)
            }))?;
            loop {
                let (m, mut fds) = self.next()?;
                if m.reply_to != Some(serial) {
                    continue; // Hello's reply, NameAcquired…: their files (none) dropped
                }
                match m.kind {
                    wire::METHOD_RETURN => {
                        let i = m.u32_at(0).unwrap_or(u32::MAX) as usize;
                        if m.sig != "h" || i >= fds.len() {
                            bail!("udisks answered without a file");
                        }
                        return Ok(fds.swap_remove(i));
                    }
                    wire::ERROR => {
                        return Err(Denied {
                            name: m.error.clone().unwrap_or_default(),
                            message: m.first_str().unwrap_or_default(),
                        }
                        .into());
                    }
                    _ => continue,
                }
            }
        }

        /// The next message, with the files it carries.
        fn next(&mut self) -> Result<(wire::Msg, Vec<OwnedFd>)> {
            loop {
                if let Some((n, m)) = wire::parse(&self.buf).map_err(anyhow::Error::msg)? {
                    self.buf.drain(..n);
                    let k = (m.fds as usize).min(self.fds.len());
                    let fds = self.fds.drain(..k).collect();
                    return Ok((m, fds));
                }
                let mut chunk = [0u8; 4096];
                let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(16))];
                let mut control = RecvAncillaryBuffer::new(&mut space);
                let got = recvmsg(
                    &self.s,
                    &mut [IoSliceMut::new(&mut chunk)],
                    &mut control,
                    RecvFlags::CMSG_CLOEXEC,
                )
                .context("reading from the system bus")?;
                for c in control.drain() {
                    if let RecvAncillaryMessage::ScmRights(fds) = c {
                        self.fds.extend(fds);
                    }
                }
                if got.bytes == 0 {
                    bail!("the system bus hung up");
                }
                self.buf.extend_from_slice(&chunk[..got.bytes]);
            }
        }
    }

    /// One line of the SASL exchange (read a byte at a time, so nothing after it is taken).
    fn line(s: &mut UnixStream) -> Result<String> {
        let mut out = Vec::new();
        let mut b = [0u8; 1];
        while out.len() < 1024 {
            s.read_exact(&mut b)?;
            out.push(b[0]);
            if out.ends_with(b"\r\n") {
                out.truncate(out.len() - 2);
                return Ok(String::from_utf8_lossy(&out).into_owned());
            }
        }
        bail!("the system bus said something too long")
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::io::IoSlice;
        use std::os::fd::AsFd;

        /// A pretend bus: agrees to everything, answers the second call with an open file.
        #[test]
        fn a_passed_file_is_taken() {
            let (a, mut b) = UnixStream::pair().unwrap();
            let dir = tempfile::tempdir().unwrap();
            let secret = dir.path().join("device.img");
            std::fs::write(&secret, b"the drive's bytes").unwrap();
            let server = std::thread::spawn(move || {
                let mut got = Vec::new();
                let mut byte = [0u8; 1];
                // the NUL, then three lines
                b.read_exact(&mut byte).unwrap();
                let mut lines = 0;
                while lines < 3 {
                    b.read_exact(&mut byte).unwrap();
                    got.push(byte[0]);
                    if got.ends_with(b"\r\n") {
                        lines += 1;
                        let l = String::from_utf8_lossy(&got).into_owned();
                        got.clear();
                        if l.starts_with("AUTH EXTERNAL ") {
                            b.write_all(b"OK 1234deadbeef\r\n").unwrap();
                        } else if l.starts_with("NEGOTIATE_UNIX_FD") {
                            b.write_all(b"AGREE_UNIX_FD\r\n").unwrap();
                        }
                    }
                }
                // Hello and the call: read both, whole
                let mut buf = Vec::new();
                let mut calls = Vec::new();
                while calls.len() < 2 {
                    let mut chunk = [0u8; 4096];
                    let n = b.read(&mut chunk).unwrap();
                    buf.extend_from_slice(&chunk[..n]);
                    while let Some((n, m)) = wire::parse(&buf).unwrap() {
                        buf.drain(..n);
                        calls.push(m);
                    }
                }
                assert_eq!(calls[1].sig, "a{sv}");
                // a signal first (with nothing attached), then the reply with the file
                let mut noise = wire::call(1, "x", "/x", "x.y", "NameAcquired", "", &[], 0);
                noise[1] = 4;
                b.write_all(&noise).unwrap();
                let mut reply = wire::W(vec![b'l', wire::METHOD_RETURN, 0, 1]);
                reply.u32(4);
                reply.u32(7);
                reply.u32(0);
                let start = reply.0.len();
                for (code, v) in [(5u8, calls[1].serial), (9, 1)] {
                    reply.pad(8);
                    reply.0.push(code);
                    reply.sig("u");
                    reply.u32(v);
                }
                reply.pad(8);
                reply.0.push(8);
                reply.sig("g");
                reply.sig("h");
                let n = (reply.0.len() - start) as u32;
                reply.0[12..16].copy_from_slice(&n.to_le_bytes());
                reply.pad(8);
                reply.0.extend(0u32.to_le_bytes());
                let f = std::fs::File::open(&secret).unwrap();
                let fds = [f.as_fd()];
                let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
                let mut control = rustix::net::SendAncillaryBuffer::new(&mut space);
                control.push(rustix::net::SendAncillaryMessage::ScmRights(&fds));
                rustix::net::sendmsg(
                    &b,
                    &[IoSlice::new(&reply.0)],
                    &mut control,
                    rustix::net::SendFlags::empty(),
                )
                .unwrap();
            });
            let mut bus = Bus::over(a).unwrap();
            let fd = bus
                .call_for_fd(
                    "org.freedesktop.UDisks2",
                    "/org/freedesktop/UDisks2/block_devices/sdz",
                    "org.freedesktop.UDisks2.Block",
                    "OpenForBackup",
                    "a{sv}",
                    &wire::options(&[("auth.no_user_interaction", true)]),
                    false,
                )
                .unwrap();
            server.join().unwrap();
            let mut s = String::new();
            std::fs::File::from(fd).read_to_string(&mut s).unwrap();
            assert_eq!(s, "the drive's bytes");
        }

        /// The real system bus, asked about a device that isn't there: an error comes back (and
        /// nothing is opened, and nobody is asked for a password).
        #[test]
        fn the_system_bus_says_no_for_a_missing_device() {
            let Ok(mut bus) = Bus::system() else {
                eprintln!("skipping: no system bus here");
                return;
            };
            let err = bus
                .call_for_fd(
                    "org.freedesktop.UDisks2",
                    "/org/freedesktop/UDisks2/block_devices/backdown_5ftest_5fnone",
                    "org.freedesktop.UDisks2.Block",
                    "OpenForBackup",
                    "a{sv}",
                    &wire::options(&[("auth.no_user_interaction", true)]),
                    false,
                )
                .unwrap_err();
            let d = err.downcast::<Denied>().expect("a D-Bus error");
            assert!(
                d.name.contains("UnknownObject")
                    || d.name.contains("UnknownMethod")
                    || d.name.contains("ServiceUnknown")
                    || d.name.contains("AccessDenied"),
                "{d:?}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_paths_as_udisks_makes_them() {
        let p = |d: &str| object_path(Path::new(d)).unwrap();
        assert_eq!(p("/dev/sdb"), "/org/freedesktop/UDisks2/block_devices/sdb");
        assert_eq!(
            p("/dev/nvme0n1p2"),
            "/org/freedesktop/UDisks2/block_devices/nvme0n1p2"
        );
        assert_eq!(
            p("/dev/dm-0"),
            "/org/freedesktop/UDisks2/block_devices/dm_2d0"
        );
        assert_eq!(
            p("/dev/a_b"),
            "/org/freedesktop/UDisks2/block_devices/a_5fb"
        );
        assert!(object_path(Path::new("/home/me/disk.img")).is_err());
    }

    #[test]
    fn messages_there_and_back() {
        let body = wire::options(&[("auth.no_user_interaction", true)]);
        // length, padding to 8, then the one entry: key, variant signature, bool
        assert_eq!(&body[..8], &[36, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(body.len(), 8 + 36);
        assert_eq!(wire::options(&[]), vec![0; 8]);
        let m = wire::call(
            2,
            "org.freedesktop.UDisks2",
            "/org/freedesktop/UDisks2/block_devices/sdb",
            "org.freedesktop.UDisks2.Block",
            "OpenForBackup",
            "a{sv}",
            &body,
            wire::ALLOW_INTERACTIVE,
        );
        assert_eq!(&m[..4], &[b'l', 1, 4, 1]);
        let (n, back) = wire::parse(&m).unwrap().unwrap();
        assert_eq!(n, m.len());
        assert_eq!((back.kind, back.serial, back.sig.as_str()), (1, 2, "a{sv}"));
        assert_eq!(back.body, body);
        // cut short: wait for more; garbage: an error
        assert_eq!(wire::parse(&m[..m.len() - 1]).unwrap(), None);
        assert!(wire::parse(b"xxxxxxxxxxxxxxxxxxxx").is_err());
        let d = Denied {
            name: "org.freedesktop.UDisks2.Error.NotAuthorizedCanObtain".into(),
            message: "Not authorized to perform operation".into(),
        };
        assert!(d.needs_auth() && d.said().contains("password"));
    }
}
