use super::*;

#[test]
fn a_folder_is_one_folder_in_either_unicode_form() {
    let mac = folder_name(&["Entwu\u{308}rfe".to_string()]);
    let pc = folder_name(&["Entw\u{fc}rfe".to_string()]);
    assert_eq!(mac, pc);
    assert_eq!(
        folder_name(&["Archive".into(), "2019".into()]),
        "Archive.2019"
    );
    assert_eq!(folder_name(&["inbox".into()]), "INBOX");
}

#[test]
fn flags_both_ways() {
    assert_eq!(
        maildir_flags("1600000000.M1P2.host:2,FRS"),
        SEEN | REPLIED | FLAGGED
    );
    assert_eq!(maildir_flags("1600000000.M1P2.host!2,Sab"), SEEN);
    assert_eq!(maildir_flags("1600000000.M1P2.host"), 0);
    assert_eq!(letters(SEEN | REPLIED | FLAGGED | DRAFT), "DFRS");
    let h = parse_headers(b"Status: RO\nX-Status: AF\nX-Mozilla-Status: 0008\n");
    assert_eq!(mbox_flags(&h), SEEN | REPLIED | FLAGGED | TRASHED);
}

#[test]
fn headers_unfold_and_ids_normalise() {
    let h = parse_headers(
        b"Subject: a long\n\tsubject\r\nMessage-ID:\n <abc@example.org>\nX-Empty:\n\nbody: not a header\n",
    );
    assert_eq!(h.get("subject"), Some("a long subject"));
    assert_eq!(message_id(&h).as_deref(), Some("abc@example.org"));
    assert!(h.get("body").is_none());
    let msg = b"Message-ID: <x@example.org>\n\nHello\r\n>From here\n\n";
    let same = b"Message-ID: <x@example.org>\nStatus: RO\n\nHello\nFrom here\n";
    let (a, _) = identity(msg, &parse_headers(msg));
    let (b, _) = identity(same, &parse_headers(same));
    assert_eq!(
        a, b,
        "headers a program adds and mbox quoting don't make another message"
    );
    let other = b"Message-ID: <x@example.org>\n\nHello again\n";
    assert_ne!(identity(other, &parse_headers(other)).0, a);
    // without a Message-ID: every header but the volatile ones, and the body
    let id = |m: &[u8]| identity(m, &parse_headers(m));
    let a = id(b"From: a@example.org\nSubject: x\n\nsame\n");
    assert!(!a.1);
    assert_eq!(
        a,
        id(b"From: a@example.org\nStatus: RO\nSubject: x\n\nsame\n")
    );
    assert_ne!(a, id(b"From: b@example.org\nSubject: x\n\nsame\n"));
    // not a message at all (no blank line after headers): all of it counts, so two different
    // ones never pass for one
    assert_ne!(
        id(b"\x01\x02 binary\nno blank line"),
        id(b"\x01\x03 binary\nno blank line")
    );
}

#[test]
fn dates() {
    assert_eq!(
        rfc2822("Tue, 1 Jul 2003 10:52:37 +0200"),
        Some(1_057_049_557)
    );
    assert_eq!(
        rfc2822("1 Jul 2003 08:52:37 GMT (UTC)"),
        Some(1_057_049_557)
    );
    assert_eq!(rfc2822("Tue, 01 Jul 03 04:52:37 EDT"), Some(1_057_049_557));
    assert_eq!(rfc2822("garbage"), None);
    assert_eq!(
        envelope_date(b"From ann@example.org Tue Jul  1 08:52:37 2003\n"),
        Some(1_057_049_557)
    );
    assert_eq!(asctime(1_057_049_557), "Tue Jul  1 08:52:37 2003");
    assert_eq!(asctime(0), "Thu Jan  1 00:00:00 1970");
}

#[test]
fn mboxrd_quoting_round_trips() {
    let msg = b"Subject: x\n\nFrom the start\n>From quoted\n>>From deeper\nFromage\n";
    let mut q = Vec::new();
    quote(msg, &mut q);
    assert_eq!(
        q,
        b"Subject: x\n\n>From the start\n>>From quoted\n>>>From deeper\nFromage\n"
    );
    assert_eq!(unquote(&q), msg);
}

#[test]
fn an_mbox_reads_message_by_message() {
    let mbox = b"From a@example.org Mon Jan  1 12:00:00 2024\nSubject: one\n\nbody\n>From x\n\nFrom b@example.org Tue Jan  2 12:00:00 2024\nSubject: two\n\nlast\n";
    let mut r = MboxReader::new(&mbox[..]);
    let one = r.next_msg().unwrap().unwrap();
    assert_eq!(one.content, b"Subject: one\n\nbody\nFrom x\n");
    assert_eq!(
        &mbox[one.start as usize..(one.start + one.len) as usize],
        b"Subject: one\n\nbody\n>From x\n"
    );
    let two = r.next_msg().unwrap().unwrap();
    assert_eq!(two.content, b"Subject: two\n\nlast\n");
    assert!(r.next_msg().unwrap().is_none());
    let mut bad = MboxReader::new(&b"Subject: not an mbox\n"[..]);
    assert!(bad.next_msg().is_err());
}
