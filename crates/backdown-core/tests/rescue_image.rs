//! Imaging a failing drive: simulated drives with bad sectors, weak areas, slow areas and a flaky
//! sector; stopping and resuming; the map file; and the places it refuses to write to.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use backdown_core::rescue::image::{
    self, Block, Map, Options, Progress, Source, Status, Summary, image_from, read_map,
};

const MIB: u64 = 1 << 20;
const SECTOR: u64 = 512;

fn pattern(len: u64) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31) ^ ((i >> 9) as u8).wrapping_add(7))
        .collect()
}

/// A simulated drive.
struct Fake {
    data: Vec<u8>,
    /// sectors that never read
    bad: BTreeSet<u64>,
    /// [start, end): any read bigger than 4 KiB touching it fails (single sectors, of 512 bytes or
    /// 4 KiB, read unless they're bad)
    weak: Vec<(u64, u64)>,
    /// sector -> how many more reads of it fail
    flaky: RefCell<HashMap<u64, u32>>,
    /// [start, end): every read touching it takes this long
    slow: Option<(u64, u64, Duration)>,
    /// a failing read returns what's before the bad sector first (like the kernel often does)
    partial: bool,
    /// every read asked for: (offset, length)
    log: RefCell<Vec<(u64, usize)>>,
    /// bytes handed out successfully
    good_bytes: Cell<u64>,
    /// called before each read
    hook: Option<Hook>,
}

/// (offset, length, reads so far)
type Hook = Box<dyn Fn(u64, usize, usize)>;

impl Fake {
    fn new(data: Vec<u8>) -> Fake {
        Fake {
            data,
            bad: BTreeSet::new(),
            weak: Vec::new(),
            flaky: RefCell::new(HashMap::new()),
            slow: None,
            partial: false,
            log: RefCell::new(Vec::new()),
            good_bytes: Cell::new(0),
            hook: None,
        }
    }

    /// The same drive, as it would be found again (a fresh log, no hook).
    fn again(&self) -> Fake {
        Fake {
            data: self.data.clone(),
            bad: self.bad.clone(),
            weak: self.weak.clone(),
            flaky: RefCell::new(self.flaky.borrow().clone()),
            slow: self.slow,
            partial: self.partial,
            ..Fake::new(Vec::new())
        }
    }

    fn stop_after(mut self, reads: usize, flag: Arc<AtomicBool>) -> Fake {
        self.hook = Some(Box::new(move |_, _, n| {
            if n >= reads {
                flag.store(true, Ordering::Relaxed);
            }
        }));
        self
    }

    /// What a perfect rescue gives: the data, with the bad sectors zeroed.
    fn expected(&self) -> Vec<u8> {
        let mut out = self.data.clone();
        for &s in &self.bad {
            let a = (s * SECTOR) as usize;
            let b = (a + SECTOR as usize).min(out.len());
            out[a..b].fill(0);
        }
        out
    }

    /// Bytes in its bad sectors (the last one may be short).
    fn bad_bytes(&self) -> u64 {
        self.bad
            .iter()
            .map(|&s| SECTOR.min(self.size() - s * SECTOR))
            .sum()
    }

    fn overlaps(a: u64, b: u64, lo: u64, hi: u64) -> bool {
        a < hi && lo < b
    }
}

impl Source for Fake {
    fn size(&self) -> u64 {
        self.data.len() as u64
    }

    fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
        let n = {
            let mut log = self.log.borrow_mut();
            log.push((off, buf.len()));
            log.len()
        };
        if let Some(hook) = &self.hook {
            hook(off, buf.len(), n);
        }
        let size = self.size();
        if off >= size {
            return Ok(0);
        }
        let end = (off + buf.len() as u64).min(size);
        if let Some((lo, hi, t)) = self.slow
            && Fake::overlaps(off, end, lo, hi)
        {
            std::thread::sleep(t);
        }
        let media_error = || io::Error::other("simulated media error");
        if end - off > 4096
            && self
                .weak
                .iter()
                .any(|&(lo, hi)| Fake::overlaps(off, end, lo, hi))
        {
            return Err(media_error());
        }
        for s in off / SECTOR..=(end - 1) / SECTOR {
            let at = (s * SECTOR).max(off);
            let flaky = self.flaky.borrow().get(&s).copied().unwrap_or(0) > 0;
            if self.bad.contains(&s) || flaky {
                if self.partial && at > off {
                    let got = (at - off) as usize;
                    buf[..got].copy_from_slice(&self.data[off as usize..at as usize]);
                    self.good_bytes.set(self.good_bytes.get() + got as u64);
                    return Ok(got);
                }
                if flaky {
                    *self.flaky.borrow_mut().get_mut(&s).unwrap() -= 1;
                }
                return Err(media_error());
            }
        }
        let got = (end - off) as usize;
        buf[..got].copy_from_slice(&self.data[off as usize..end as usize]);
        self.good_bytes.set(self.good_bytes.get() + got as u64);
        Ok(got)
    }
}

fn quiet() -> Options {
    Options {
        slow_ms: 0,
        ..Options::default()
    }
}

fn run(src: &Fake, img: &Path, map: &Path, opts: &Options) -> Summary {
    run_with(src, img, map, opts, &AtomicBool::new(false), &mut |_| {})
}

fn run_with(
    src: &Fake,
    img: &Path,
    map: &Path,
    opts: &Options,
    stop: &AtomicBool,
    progress: &mut dyn FnMut(&Progress),
) -> Summary {
    image_from(src, img, map, opts, progress, stop).unwrap()
}

fn places(dir: &Path) -> (PathBuf, PathBuf) {
    (dir.join("card.img"), dir.join("card.map"))
}

/// The map is ddrescue-shaped: sorted, contiguous, coalesced, from 0 to the size.
fn assert_well_formed(m: &Map, size: u64) {
    let mut end = 0;
    for (i, b) in m.blocks.iter().enumerate() {
        assert_eq!(b.pos, end, "block {i} doesn't follow on");
        assert!(b.size > 0);
        if i > 0 {
            assert_ne!(m.blocks[i - 1].status, b.status, "block {i} isn't merged");
        }
        end = b.end();
    }
    assert_eq!(end, size);
}

/// The sectors a map says are bad.
fn bad_sectors(m: &Map) -> BTreeSet<u64> {
    m.blocks
        .iter()
        .filter(|b| b.status == Status::Bad)
        .flat_map(|b| b.pos / SECTOR..b.end().div_ceil(SECTOR))
        .collect()
}

/// A drive with a bit of everything: lone bad sectors, a short bad run, a weak area (large reads
/// fail, single sectors mostly read) with bad sectors inside, a flaky sector, and a bad last sector
/// on a size that isn't a whole number of sectors.
fn failing(partial: bool) -> Fake {
    let size = 8 * MIB + 700;
    let mut f = Fake::new(pattern(size));
    let last = (size - 1) / SECTOR;
    f.bad = [300, 5000, 5001, 5002, 5003, 8500, 9000, 9001, 9002, last]
        .into_iter()
        .collect();
    f.weak = vec![(4 * MIB, 4 * MIB + MIB / 2)];
    f.flaky.borrow_mut().insert(12000, 2);
    f.partial = partial;
    f
}

/// The same on a smaller scale, for runs stopped every few reads; the weak area's first and last
/// sectors are bad, so trimming leaves nearly all of it to scraping.
fn small_failing(partial: bool) -> Fake {
    let size = MIB + 700;
    let mut f = Fake::new(pattern(size));
    let last = (size - 1) / SECTOR;
    f.bad = [30, 500, 501, 502, 503, 512, 600, 610, 611, 612, 639, last]
        .into_iter()
        .collect();
    f.weak = vec![(256 * 1024, 320 * 1024)];
    f.flaky.borrow_mut().insert(1200, 2);
    f.partial = partial;
    f
}

#[test]
fn a_healthy_source_copies_exactly_and_reads_everything_once() {
    let tmp = tempfile::tempdir().unwrap();
    let (img, map) = places(tmp.path());
    let src = Fake::new(pattern(3 * MIB + 1000));
    let s = run(&src, &img, &map, &quiet());
    assert!(s.complete && !s.stopped);
    assert_eq!(
        (s.size, s.rescued, s.bad, s.todo, s.bad_areas, s.errors),
        (src.size(), src.size(), 0, 0, 0, 0)
    );
    assert_eq!(fs::read(&img).unwrap(), src.data);
    let m = read_map(&map).unwrap();
    assert_eq!(
        m.blocks,
        [Block {
            pos: 0,
            size: src.size(),
            status: Status::Finished
        }]
    );
    assert_eq!(m.status, '+');
    assert_eq!(src.good_bytes.get(), src.size());
    // in whole blocks, forwards
    let log = src.log.borrow();
    assert_eq!(log.len(), 4);
    assert!(log.windows(2).all(|w| w[0].0 < w[1].0));

    // running it again finds nothing to do
    let again = src.again();
    let s2 = run(&again, &img, &map, &quiet());
    assert!(s2.complete);
    assert!(again.log.borrow().is_empty());
}

#[test]
fn a_failing_source_ends_with_exactly_its_bad_sectors() {
    for partial in [false, true] {
        for reverse in [true, false] {
            for sector in [512usize, 4096] {
                let tmp = tempfile::tempdir().unwrap();
                let (img, map) = places(tmp.path());
                let src = failing(partial);
                let opts = Options {
                    retries: 3,
                    reverse,
                    sector,
                    ..quiet()
                };
                let mut passes = Vec::new();
                let s = run_with(&src, &img, &map, &opts, &AtomicBool::new(false), &mut |p| {
                    if passes.last() != Some(&p.pass) {
                        passes.push(p.pass);
                    }
                    assert!(p.rescued + p.bad + p.todo == p.size && p.read_rate >= 0.0);
                });
                let case = format!("partial {partial}, reverse {reverse}, sector {sector}");
                assert_eq!(
                    passes,
                    ["copying", "trimming", "scraping", "retrying"],
                    "{case}"
                );
                assert!(!s.complete && !s.stopped, "{case}");
                // the bad sectors, widened to the sector size the rescue works in: the whole unit,
                // or from the bad sector on when the drive hands over what comes before it
                let per = sector as u64 / SECTOR;
                let want: BTreeSet<u64> = src
                    .bad
                    .iter()
                    .flat_map(|&b| {
                        let unit = b / per * per;
                        let first = if partial {
                            (unit..=b).find(|s| src.bad.contains(s)).unwrap()
                        } else {
                            unit
                        };
                        (first..unit + per).filter(|&s| s * SECTOR < src.size())
                    })
                    .collect();
                let m = read_map(&map).unwrap();
                assert_well_formed(&m, src.size());
                assert_eq!(bad_sectors(&m), want, "{case}");
                assert_eq!(m.status, '+', "{case}");
                // everything else is in the image, byte for byte; the bad parts are zeros
                let mut expected = src.data.clone();
                let len = expected.len();
                for &s in &want {
                    let a = (s * SECTOR) as usize;
                    expected[a..(a + SECTOR as usize).min(len)].fill(0);
                }
                assert!(fs::read(&img).unwrap() == expected, "{case}");
                assert_eq!(s.rescued + s.bad, src.size(), "{case}");
                assert_eq!(s.todo, 0, "{case}");
                // 300, 5000-5003, 8500, 9000-9002 and the last one (the flaky one came back)
                assert_eq!(s.bad_areas, 5, "{case}");
                assert_eq!(
                    src.good_bytes.get(),
                    s.rescued,
                    "{case}: good data read twice"
                );
            }
        }
    }
}

#[test]
fn healthy_areas_come_first_and_bad_areas_are_reached_from_both_sides() {
    let tmp = tempfile::tempdir().unwrap();
    let (img, map) = places(tmp.path());
    let size = 16 * MIB;
    let (lo, hi) = (5 * MIB + 100 * SECTOR, 6 * MIB + MIB / 2);
    let mut src = Fake::new(pattern(size));
    src.bad = (lo / SECTOR..hi / SECTOR).collect();
    // stop as trimming starts, to see what copying alone got
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    src.hook = Some(Box::new(move |_, len, _| {
        if len as u64 == SECTOR {
            flag.store(true, Ordering::Relaxed);
        }
    }));
    let s = run_with(&src, &img, &map, &quiet(), &stop, &mut |_| {});
    assert!(s.stopped);
    let m = read_map(&map).unwrap();
    assert_eq!(m.status, '*', "stopped while trimming");
    // everything more than a block away from the bad area is in, and nothing was left untried
    for b in &m.blocks {
        assert_ne!(b.status, Status::NonTried);
        if b.status != Status::Finished {
            assert!(b.pos + MIB >= lo && b.end() <= hi + MIB, "{b:?}");
        }
    }
    // pass 2 went backwards
    let log = src.log.borrow();
    assert!(
        log.windows(2)
            .any(|w| w[1].0 < w[0].0 && w[0].1 > SECTOR as usize)
    );
    // and skipping kept the failed reads few
    assert!(s.errors < 16, "{} failed reads while copying", s.errors);
    drop(log);

    let again = src.again();
    let s2 = run(&again, &img, &map, &quiet());
    assert!(!s2.stopped);
    assert_eq!(bad_sectors(&read_map(&map).unwrap()), src.bad);
    assert!(fs::read(&img).unwrap() == src.expected());
}

#[test]
fn stopping_and_resuming_never_reads_finished_areas_again() {
    for (partial, every) in [(false, 3), (true, 5), (false, 40)] {
        let tmp = tempfile::tempdir().unwrap();
        let (img, map) = places(tmp.path());
        let opts = Options {
            retries: 3,
            block: 64 * 1024,
            ..quiet()
        };
        let drive = small_failing(partial);
        let mut runs = 0;
        let mut good = 0;
        let mut phases = BTreeSet::new();
        let last = loop {
            runs += 1;
            assert!(runs < 10_000, "no progress");
            let stop = Arc::new(AtomicBool::new(false));
            // the drive as it is now (the flaky sector remembers how often it failed)
            let src = drive.again().stop_after(every, stop.clone());
            let before = if map.exists() {
                read_map(&map).unwrap()
            } else {
                Map::new(drive.size())
            };
            let s = run_with(&src, &img, &map, &opts, &stop, &mut |_| {});
            *drive.flaky.borrow_mut() = src.flaky.borrow().clone();
            good += src.good_bytes.get();
            // nothing finished before this run was read again
            for &(off, len) in src.log.borrow().iter() {
                for b in before
                    .blocks
                    .iter()
                    .filter(|b| b.status == Status::Finished)
                {
                    assert!(
                        off + len as u64 <= b.pos || b.end() <= off,
                        "run {runs} read {off}+{len} again, finished in {b:?}"
                    );
                }
            }
            let m = read_map(&map).unwrap();
            assert_well_formed(&m, drive.size());
            phases.insert(m.status);
            if !s.stopped {
                break s;
            }
        };
        assert!(runs > 3, "it should have been stopped a few times");
        if every == 3 {
            // stopped in every phase at least once
            assert!(
                ['?', '*', '/', '-'].iter().all(|p| phases.contains(p)),
                "{phases:?}"
            );
        }
        assert_eq!(bad_sectors(&read_map(&map).unwrap()), drive.bad);
        assert!(fs::read(&img).unwrap() == drive.expected());
        assert_eq!(last.rescued, drive.size() - drive.bad_bytes());
        assert_eq!(good, last.rescued, "every good byte read exactly once");
    }
}

#[test]
fn slow_areas_are_skipped_and_come_back_later() {
    let tmp = tempfile::tempdir().unwrap();
    let (img, map) = places(tmp.path());
    let (lo, hi) = (MIB, MIB + 256 * 1024);
    let mut src = Fake::new(pattern(2 * MIB));
    src.slow = Some((lo, hi, Duration::from_millis(40)));
    let opts = Options {
        block: 64 * 1024,
        slow_ms: 10,
        reverse: false,
        ..quiet()
    };
    let s = run(&src, &img, &map, &opts);
    assert!(s.complete);
    assert_eq!(fs::read(&img).unwrap(), src.data);
    let log = src.log.borrow();
    let touches = |&(off, len): &(u64, usize)| off < hi && lo < off + len as u64;
    let first = log.iter().position(touches).unwrap();
    let last = log.iter().rposition(touches).unwrap();
    assert!(
        log[first..last].iter().any(|&(off, _)| off >= hi),
        "the area after the slow one should be copied before the slow one is finished"
    );
    assert_eq!(src.good_bytes.get(), src.size());
}

#[test]
fn a_ddrescue_image_and_map_carry_on_where_they_were() {
    let tmp = tempfile::tempdir().unwrap();
    let (img, map) = places(tmp.path());
    let size = 2 * MIB;
    let data = pattern(size);
    // ddrescue got the first 1.5 MiB and stopped; its image ends there
    fs::write(&img, &data[..(MIB + MIB / 2) as usize]).unwrap();
    fs::write(
        &map,
        "# Mapfile. Created by GNU ddrescue version 1.27\n\
         # current_pos  current_status  current_pass\n\
         0x00180000     ?               1\n\
         #      pos        size  status\n\
         0x00000000  0x00180000  +\n\
         0x00180000  0x00080000  ?\n",
    )
    .unwrap();
    let src = Fake::new(data.clone());
    let s = run(&src, &img, &map, &quiet());
    assert!(s.complete);
    assert_eq!(fs::read(&img).unwrap(), data);
    assert!(
        src.log
            .borrow()
            .iter()
            .all(|&(off, _)| off >= MIB + MIB / 2)
    );
}

#[test]
fn the_map_file_is_ddrescues() {
    let sample = "\
# Mapfile. Created by GNU ddrescue version 1.27
# Command line: ddrescue -d /dev/sdX disk.img disk.map
# Start time:   2026-01-01 10:00:00
# Current time: 2026-01-01 10:05:00
# Copying non-tried blocks... Pass 1 (forwards)
# current_pos  current_status  current_pass
0x00120000     ?               1
#      pos        size  status
0x00000000  0x00117000  +
0x00117000  0x00000200  -
0x00117200  0x00001000  /
0x00118200  0x00007E00  *
0x00120000  0x048A0000  ?
";
    let m = Map::parse(sample).unwrap();
    assert_eq!((m.pos, m.status, m.pass), (0x120000, '?', 1));
    let got: Vec<(u64, u64, char)> = m
        .blocks
        .iter()
        .map(|b| (b.pos, b.size, b.status.as_char()))
        .collect();
    assert_eq!(
        got,
        [
            (0, 0x117000, '+'),
            (0x117000, 0x200, '-'),
            (0x117200, 0x1000, '/'),
            (0x118200, 0x7E00, '*'),
            (0x120000, 0x48A0000, '?'),
        ]
    );
    let t = m.totals();
    assert_eq!(
        (t.rescued, t.bad, t.todo, t.bad_areas),
        (0x117000, 0x200, 0x1000 + 0x7E00 + 0x48A0000, 1)
    );
    let text = m.to_string();
    assert_eq!(
        text,
        format!(
            "\
# Mapfile. Created by BackDown {}
# Copying non-tried blocks... Pass 1 (forwards)
# current_pos  current_status  current_pass
0x00120000     ?               1
#      pos        size  status
0x00000000  0x00117000  +
0x00117000  0x00000200  -
0x00117200  0x00001000  /
0x00118200  0x00007E00  *
0x00120000  0x048A0000  ?
",
            env!("CARGO_PKG_VERSION")
        )
    );
    assert_eq!(Map::parse(&text).unwrap(), m);
    // big positions keep all their digits
    let mut big = Map::new(0x1D1C0BEB000);
    big.set(0x1000, 0x1200, Status::Bad);
    assert!(big.to_string().contains("\n0x00001200  0x1D1C0BE9E00  ?\n"));
    assert_eq!(Map::parse(&big.to_string()).unwrap(), big);
    // maps from before ddrescue 1.21 have no pass on the status line
    let old =
        Map::parse("# Rescue Logfile\n0x00000000     +\n0x00000000  0x00100000  +\n").unwrap();
    assert_eq!((old.status, old.pass, old.size()), ('+', 1, 0x100000));
}

#[test]
fn stopping_saves_a_map_that_says_where_it_was() {
    let tmp = tempfile::tempdir().unwrap();
    let (img, map) = places(tmp.path());
    let stop = Arc::new(AtomicBool::new(false));
    let src = Fake::new(pattern(4 * MIB)).stop_after(2, stop.clone());
    let s = run_with(&src, &img, &map, &quiet(), &stop, &mut |_| {});
    assert!(s.stopped && !s.complete);
    assert_eq!(s.rescued, 2 * MIB);
    let m = read_map(&map).unwrap();
    assert_eq!((m.pos, m.status, m.pass), (2 * MIB, '?', 1));
    assert_eq!(m.totals().rescued, 2 * MIB);
    // no temporary file left behind
    let names: Vec<_> = fs::read_dir(tmp.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names.len(), 2, "{names:?}");
}

fn err_of(r: anyhow::Result<Summary>) -> String {
    format!("{:#}", r.expect_err("should have been refused"))
}

#[test]
fn it_refuses_what_could_hurt_the_source_or_mix_up_jobs() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let src_path = dir.join("old-card.img");
    let data = pattern(MIB);
    fs::write(&src_path, &data).unwrap();
    let (img, map) = places(dir);
    let stop = AtomicBool::new(false);
    let go = |src: &Path, img: &Path, map: &Path, opts: &Options| {
        image::image(src, img, map, opts, &mut |_| {}, &stop)
    };
    let anywhere = Options {
        allow_same_drive: true,
        ..quiet()
    };

    // the source itself, as the image or as the map, even when the same drive is allowed
    assert!(err_of(go(&src_path, &src_path, &map, &anywhere)).contains("itself"));
    assert!(err_of(go(&src_path, &img, &src_path, &anywhere)).contains("itself"));
    #[cfg(unix)]
    {
        let link = dir.join("link.img");
        std::os::unix::fs::symlink(&src_path, &link).unwrap();
        assert!(err_of(go(&src_path, &link, &map, &anywhere)).contains("itself"));
    }
    // next to a source file: the same drive, refused unless allowed
    assert!(err_of(go(&src_path, &img, &map, &quiet())).contains("same drive"));
    assert!(!img.exists() && !map.exists());
    // the image and the map can't be one file
    assert!(err_of(go(&src_path, &img, &img, &anywhere)).contains("two different files"));
    // bad options
    let odd = Options {
        sector: 1000,
        ..anywhere.clone()
    };
    assert!(err_of(go(&src_path, &img, &map, &odd)).contains("sector size"));
    // a folder, or nothing, as the source
    assert!(go(dir, &img, &map, &anywhere).is_err());
    assert!(go(&dir.join("missing"), &img, &map, &anywhere).is_err());
    // an empty source
    let empty = dir.join("empty.img");
    fs::write(&empty, b"").unwrap();
    assert!(err_of(go(&empty, &img, &map, &anywhere)).contains("empty"));

    // an image that's already there without a map is never overwritten
    fs::write(&img, b"someone's file").unwrap();
    assert!(err_of(go(&src_path, &img, &map, &anywhere)).contains("won't overwrite"));
    assert_eq!(fs::read(&img).unwrap(), b"someone's file");
    fs::remove_file(&img).unwrap();
    // a folder where the image goes
    fs::create_dir(dir.join("a-folder")).unwrap();
    assert!(err_of(go(&src_path, &dir.join("a-folder"), &map, &anywhere)).contains("isn't a file"));

    // and with the same drive allowed, a file source images fine, untouched
    let before = fs::metadata(&src_path).unwrap().modified().unwrap();
    let s = go(&src_path, &img, &map, &anywhere).unwrap();
    assert!(s.complete);
    assert_eq!(fs::read(&img).unwrap(), data);
    assert_eq!(fs::read(&src_path).unwrap(), data);
    assert_eq!(fs::metadata(&src_path).unwrap().modified().unwrap(), before);

    // a map of something else's size
    let other = dir.join("other.img");
    fs::write(&other, pattern(2 * MIB)).unwrap();
    let e = err_of(go(&other, &img, &map, &anywhere));
    assert!(
        e.contains("doesn't fit") && e.contains("same drive?"),
        "{e}"
    );
    // a map that says things were rescued, without their image
    fs::remove_file(&img).unwrap();
    assert!(err_of(go(&src_path, &img, &map, &anywhere)).contains("isn't there"));
    // a map with nothing rescued yet and no image just starts (over)
    fs::write(&map, Map::new(MIB).to_string()).unwrap();
    assert!(go(&src_path, &img, &map, &anywhere).unwrap().complete);
}

#[test]
fn it_refuses_an_image_that_would_not_fit() {
    // no drive in a test has room for an exabyte
    struct Huge;
    impl Source for Huge {
        fn size(&self) -> u64 {
            1 << 60
        }
        fn read_at(&self, _: u64, _: &mut [u8]) -> io::Result<usize> {
            unreachable!("nothing should be read")
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let (img, map) = places(tmp.path());
    let r = image_from(
        &Huge,
        &img,
        &map,
        &quiet(),
        &mut |_| {},
        &AtomicBool::new(false),
    );
    assert!(err_of(r).contains("not enough room"));
    assert!(!img.exists() && !map.exists());
}

#[test]
fn a_source_that_goes_away_stops_the_job_and_keeps_what_was_rescued() {
    struct Unplugged {
        data: Vec<u8>,
        reads: Cell<u32>,
    }
    impl Source for Unplugged {
        fn size(&self) -> u64 {
            self.data.len() as u64
        }
        fn read_at(&self, off: u64, buf: &mut [u8]) -> io::Result<usize> {
            self.reads.set(self.reads.get() + 1);
            if self.reads.get() > 2 {
                return Err(io::ErrorKind::NotFound.into());
            }
            let n = buf.len().min(self.data.len() - off as usize);
            buf[..n].copy_from_slice(&self.data[off as usize..off as usize + n]);
            Ok(n)
        }
    }
    let tmp = tempfile::tempdir().unwrap();
    let (img, map) = places(tmp.path());
    let src = Unplugged {
        data: pattern(8 * MIB),
        reads: Cell::new(0),
    };
    let r = image_from(
        &src,
        &img,
        &map,
        &quiet(),
        &mut |_| {},
        &AtomicBool::new(false),
    );
    assert!(err_of(r).contains("stopped answering"));
    assert_eq!(src.reads.get(), 3, "it gave up at once");
    let m = read_map(&map).unwrap();
    assert_eq!(m.totals().rescued, 2 * MIB);
    assert_eq!(m.totals().bad, 0);
}
