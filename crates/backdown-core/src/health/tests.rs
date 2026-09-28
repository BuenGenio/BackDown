use super::*;
use crate::volumes::{Disk, Mount, Volume};

const DAY: i64 = 86_400;
const T0: i64 = 1_790_000_000;

fn hdd(at: i64) -> Health {
    Health {
        id: "sn:FAKE0001".into(),
        device: "/dev/sda".into(),
        name: "sda".into(),
        model: Some("FAKEDISK HD4000".into()),
        serial: Some("FAKE0001".into()),
        rotational: Some(true),
        protocol: Some("ata".into()),
        reported: Some(Verdict::Ok),
        temperature: Some(35),
        power_on_hours: Some(20_000),
        reallocated: Some(0),
        pending: Some(0),
        uncorrectable: Some(0),
        crc_errors: Some(0),
        source: "udisks2".into(),
        read_at: at,
        ..Health::default()
    }
}

fn ssd(at: i64) -> Health {
    Health {
        id: "sn:FAKENVME01".into(),
        device: "/dev/nvme0n1".into(),
        name: "nvme0n1".into(),
        rotational: Some(false),
        protocol: Some("nvme".into()),
        critical_warning: Some(0),
        available_spare: Some(100),
        spare_threshold: Some(10),
        percentage_used: Some(40),
        media_errors: Some(0),
        temperature: Some(40),
        read_at: at,
        ..Health::default()
    }
}

fn codes(ch: &[Change]) -> Vec<(&str, Verdict)> {
    ch.iter().map(|c| (c.code.as_str(), c.level)).collect()
}

#[test]
fn identities() {
    assert_eq!(
        identity(Some(" WD-WX00A1234567 "), None),
        "sn:WDWX00A1234567"
    );
    // Windows prints NVMe serials with underscores and a dot; the same drive either way
    assert_eq!(
        identity(Some("FNV0_0000_0000_0001."), None),
        identity(Some("FNV0 0000 0000 0001"), None)
    );
    assert_eq!(
        identity(Some("000000000000"), Some("0x5000c500abcd0001")),
        "wwn:5000C500ABCD0001"
    );
    assert_eq!(identity(Some("0123456789ABCDEF"), None), "");
    assert_eq!(identity(None, Some("")), "");
}

#[test]
fn verdicts_from_one_reading() {
    let ok = hdd(T0);
    assert_eq!(assess(&ok).verdict, Verdict::Ok);
    assert!(assess(&ok).findings.is_empty());

    let mut h = hdd(T0);
    h.reallocated = Some(8);
    h.crc_errors = Some(3);
    let a = assess(&h);
    assert_eq!(a.verdict, Verdict::Warning);
    // cable errors are worth knowing but aren't the drive's fault
    assert!(
        a.findings
            .iter()
            .any(|f| f.code == "crc" && f.level == Verdict::Ok)
    );

    h.pending = Some(12);
    assert_eq!(assess(&h).verdict, Verdict::Failing);

    let mut h = hdd(T0);
    h.reported = Some(Verdict::Failing);
    h.reported_as = Some("FAILED!".into());
    let a = assess(&h);
    assert_eq!(a.verdict, Verdict::Failing);
    assert!(a.findings[0].text.contains("FAILED!"));

    let mut h = hdd(T0);
    h.temperature = Some(57);
    assert_eq!(assess(&h).verdict, Verdict::Warning); // hot for a spinning disk
    let mut s = ssd(T0);
    s.temperature = Some(57);
    assert_eq!(assess(&s).verdict, Verdict::Ok); // fine for an SSD

    let mut s = ssd(T0);
    s.percentage_used = Some(93);
    assert_eq!(assess(&s).verdict, Verdict::Warning);
    s.percentage_used = Some(20);
    s.available_spare = Some(15);
    assert_eq!(assess(&s).verdict, Verdict::Warning); // within 10 points of its limit
    s.available_spare = Some(8);
    assert_eq!(assess(&s).verdict, Verdict::Failing);
    // Apple's SSDs put their limit at 99%: 99% left is fine, below it isn't
    let mut apple = ssd(T0);
    apple.spare_threshold = Some(99);
    apple.available_spare = Some(99);
    assert_eq!(assess(&apple).verdict, Verdict::Ok);
    apple.available_spare = Some(98);
    assert_eq!(assess(&apple).verdict, Verdict::Failing);
    s.available_spare = Some(100);
    s.critical_warning = Some(0x08);
    let a = assess(&s);
    assert_eq!(a.verdict, Verdict::Failing);
    assert!(a.findings[0].text.contains("read-only"));
    s.critical_warning = Some(0x02);
    assert_eq!(assess(&s).verdict, Verdict::Warning);

    // nothing readable at all: unknown, however hot it is
    let blank = Health {
        temperature: Some(80),
        ..Health::default()
    };
    assert_eq!(assess(&blank).verdict, Verdict::Unknown);
}

#[test]
fn what_got_worse() {
    let a = hdd(T0);
    let mut b = hdd(T0 + DAY);
    assert!(step(Some(&a), &b).is_empty());

    b.reallocated = Some(3);
    b.pending = Some(1);
    b.crc_errors = Some(5);
    let ch = step(Some(&a), &b);
    assert_eq!(
        codes(&ch),
        [
            ("verdict", Verdict::Warning),
            ("reallocated", Verdict::Warning),
            ("pending", Verdict::Warning),
            ("crc", Verdict::Warning),
        ]
    );
    assert!(ch[0].text.starts_with("was ok, now warning"));
    assert!(ch[1].text.contains("3 more sectors have gone bad"));
    assert!(ch[3].text.contains("cable"));
    assert_eq!(ch[1].key, "reallocated:3");

    // counters falling back (a different source, a reset) aren't news
    let mut c = b.clone();
    c.read_at += DAY;
    c.reallocated = Some(1);
    assert!(step(Some(&b), &c).is_empty());

    // the drive's own verdict flipping
    let mut d = b.clone();
    d.read_at += DAY;
    d.reported = Some(Verdict::Failing);
    let ch = step(Some(&b), &d);
    assert_eq!(
        codes(&ch),
        [
            ("verdict", Verdict::Failing),
            ("reported", Verdict::Failing)
        ]
    );

    // a temperature spike
    let mut e = a.clone();
    e.read_at += DAY;
    e.temperature = Some(58);
    let ch = step(Some(&a), &e);
    assert!(
        ch.iter()
            .any(|c| c.code == "temperature" && c.text.contains("58 °C"))
    );

    // the first reading of a drive that's already in trouble is news too
    let ch = step(None, &b);
    assert_eq!(codes(&ch), [("verdict", Verdict::Warning)]);
    assert!(step(None, &a).is_empty());
}

#[test]
fn ssd_wear_spare_and_critical_bits() {
    let a = ssd(T0);
    let mut b = ssd(T0 + DAY);
    b.percentage_used = Some(81);
    let ch = step(Some(&a), &b);
    // 40 -> 81 crosses 50 (for the record) and 80
    assert_eq!(
        codes(&ch),
        [("wear", Verdict::Ok), ("wear", Verdict::Warning)]
    );
    assert_eq!(ch[1].key, "wear:80");

    let mut c = b.clone();
    c.read_at += DAY;
    c.available_spare = Some(9);
    c.critical_warning = Some(0x01);
    let ch = step(Some(&b), &c);
    assert_eq!(
        codes(&ch),
        [
            ("verdict", Verdict::Failing),
            ("spare", Verdict::Failing),
            ("critical", Verdict::Failing)
        ]
    );
    let mut d = ssd(T0 + DAY);
    d.media_errors = Some(2);
    assert_eq!(
        codes(&step(Some(&a), &d)),
        [
            ("verdict", Verdict::Warning),
            ("media-errors", Verdict::Warning)
        ]
    );
}

#[test]
fn readings_are_kept_when_they_say_something() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("index.db")).unwrap();
    let r = record(&db, &hdd(T0)).unwrap();
    assert!(r.stored && r.first && r.changes.is_empty());
    assert_eq!(r.verdict, Verdict::Ok);
    // an hour later, nothing new but the temperature: not kept
    let mut h = hdd(T0 + 3600);
    h.temperature = Some(37);
    let r = record(&db, &h).unwrap();
    assert!(!r.stored);
    // a day later, unchanged: kept (a point on the trend)
    assert!(record(&db, &hdd(T0 + DAY)).unwrap().stored);
    // new bad sectors: kept, with what changed
    let mut h = hdd(T0 + DAY + 3600);
    h.reallocated = Some(2);
    let r = record(&db, &h).unwrap();
    assert!(r.stored && !r.first);
    assert_eq!(r.before, Some(Verdict::Ok));
    assert_eq!(r.verdict, Verdict::Warning);
    assert_eq!(codes(&r.changes)[1], ("reallocated", Verdict::Warning));
    // asleep (no data) and anonymous drives aren't kept
    let asleep = Health {
        id: "sn:FAKE0001".into(),
        read_at: T0 + 2 * DAY,
        ..Health::default()
    };
    assert!(!record(&db, &asleep).unwrap().stored);
    let mut anon = hdd(T0);
    anon.id.clear();
    assert!(!record(&db, &anon).unwrap().stored);

    let hist = history(&db, "sn:FAKE0001").unwrap();
    assert_eq!(hist.len(), 3);
    assert_eq!(hist[2].reallocated, Some(2));
    assert_eq!(
        last(&db, "sn:FAKE0001").unwrap().unwrap().read_at,
        T0 + DAY + 3600
    );
    let ds = drives(&db).unwrap();
    assert_eq!(
        ds,
        [("sn:FAKE0001".to_string(), T0 + DAY + 3600, Verdict::Warning)]
    );
    assert!(available(&db));
}

#[test]
fn summaries_trends_and_series() {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("index.db")).unwrap();
    for (i, re) in [0u64, 0, 4, 9].iter().enumerate() {
        let mut h = hdd(T0 + i as i64 * DAY);
        h.reallocated = Some(*re);
        h.power_on_hours = Some(20_000 + 24 * i as u64);
        h.mounts = vec!["/media/someone/Old".into()];
        record(&db, &h).unwrap();
    }
    // a drive seen before but not connected now
    record(&db, &ssd(T0)).unwrap();
    // the HDD is connected, now as sdc, and asleep: its numbers come from the last reading
    let live = Health {
        id: "sn:FAKE0001".into(),
        device: "/dev/sdc".into(),
        name: "sdc".into(),
        note: Some("it's asleep (spun down): not woken to read it".into()),
        mounts: vec!["/media/someone/Old".into()],
        read_at: T0 + 5 * DAY,
        ..Health::default()
    };
    let all = overview(Some(&db), &[live]).unwrap();
    assert_eq!(all.len(), 2);
    let s = &all[0];
    assert!(s.present);
    assert_eq!(s.verdict, Verdict::Warning);
    assert_eq!(s.health.device, "/dev/sdc");
    assert_eq!(s.health.reallocated, Some(9));
    assert!(s.health.note.as_deref().unwrap().contains("asleep"));
    assert_eq!(s.trend, ["+9 reallocated", "+72 h on"]);
    assert_eq!(s.first_at, Some(T0));
    assert_eq!(s.series.len(), 4);
    assert_eq!(s.series[3].bad, Some(9));
    assert_eq!(
        s.changes.iter().map(|c| c.key.as_str()).collect::<Vec<_>>(),
        ["verdict:warning", "reallocated:4", "reallocated:9"]
    );
    let away = &all[1];
    assert!(!away.present);
    assert_eq!(away.verdict, Verdict::Ok);
    // which drive a folder is on
    assert_eq!(
        drive_of("/media/someone/Old/Mirror", &all).map(|d| d.id.as_str()),
        Some("sn:FAKE0001")
    );
    assert!(drive_of("/elsewhere", &all).is_none());
}

#[test]
fn skeletons_from_the_volume_probe() {
    let vol = |m: &str| Volume {
        mounts: vec![Mount {
            path: m.into(),
            read_only: false,
        }],
        ..Volume::default()
    };
    let disks = vec![
        Disk {
            device: "/dev/nvme0n1".into(),
            name: "nvme0n1".into(),
            serial: Some("FAKENVME01".into()),
            volumes: vec![vol("/")],
            ..Disk::default()
        },
        // another namespace of the same SSD: the same drive
        Disk {
            device: "/dev/nvme0n2".into(),
            name: "nvme0n2".into(),
            serial: Some("FAKENVME01".into()),
            volumes: vec![vol("/srv")],
            ..Disk::default()
        },
        Disk {
            device: "/dev/loop3".into(),
            name: "loop3".into(),
            transport: Some("loop".into()),
            backing_file: Some("/tmp/x.img".into()),
            ..Disk::default()
        },
        Disk {
            device: "/dev/sda".into(),
            name: "sda".into(),
            size: 4_000_787_030_016,
            model: Some("FAKEDISK HD4000".into()),
            ..Disk::default()
        },
    ];
    let s = skeletons(&disks, T0);
    assert_eq!(s.len(), 2);
    assert_eq!(s[0].id, "sn:FAKENVME01");
    assert_eq!(s[0].mounts, ["/", "/srv"]);
    assert_eq!(s[1].id, "");
    assert_eq!(s[1].size, Some(4_000_787_030_016));
}

#[test]
fn trend_words() {
    let a = ssd(T0);
    let mut b = ssd(T0 + DAY);
    b.percentage_used = Some(43);
    b.available_spare = Some(97);
    assert_eq!(trend(&a, &b), ["wear +3%", "spare -3%"]);
    assert!(trend(&a, &a).is_empty());
}
