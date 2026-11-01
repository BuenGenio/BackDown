use super::*;

#[test]
fn scripts_describe_their_copies() {
    let t = parse_script(
        "#!/bin/sh\nDEST=/mnt/usb/backups\nexport SRC=root@mail.example.org:/var/vmail/\n\
         rsync -aHAX --delete -e 'ssh -p 2222' --exclude=.cache $SRC \"$DEST/mail/\"\n\
         tar -czf ${DEST}/etc.tgz /etc /root\nscp -P 22 user@web.example.org:/srv/www/site.sql $DEST/sql/\n\
         echo done && cp -a /home/alex/Photos /mnt/usb/backups/photos\n\
         0 3 * * * rclone sync --fast-list gdrive:Documents /mnt/usb/backups/gdrive\n",
    );
    assert_eq!(t.len(), 5, "{t:?}");
    assert_eq!(t[0].tool, "rsync");
    assert_eq!(
        t[0].sources,
        vec!["root@mail.example.org:/var/vmail/".to_string()]
    );
    assert_eq!(t[0].dest, "/mnt/usb/backups/mail/");
    assert_eq!(t[1].tool, "tar");
    assert_eq!(t[1].dest, "/mnt/usb/backups/etc.tgz");
    assert_eq!(t[1].sources, vec!["/etc".to_string(), "/root".to_string()]);
    assert_eq!(t[2].tool, "scp");
    assert_eq!(t[2].dest, "/mnt/usb/backups/sql/");
    assert_eq!(t[3].tool, "cp");
    assert_eq!(t[4].tool, "rclone");
    assert_eq!(
        remote_host(&t[0].sources[0]).as_deref(),
        Some("mail.example.org")
    );
    assert_eq!(remote_host("C:\\Users\\alex"), None);
    assert_eq!(remote_host("/home/alex"), None);
}

#[test]
fn batch_files_too() {
    let t = parse_script(
        "@echo off\r\nset TARGET=E:\\Backup\r\nrem nightly\r\nrobocopy C:\\Users\\Alex\\Documents %TARGET%\\Documents /MIR /R:1\r\n",
    );
    assert_eq!(t.len(), 1, "{t:?}");
    assert_eq!(t[0].tool, "robocopy");
    assert_eq!(t[0].dest, "E:\\Backup\\Documents");
}
