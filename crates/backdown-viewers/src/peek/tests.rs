use std::io::Write;

use serde_json::{Value, json};

use super::*;

fn tmp(bytes: &[u8], suffix: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::Builder::new().suffix(suffix).tempfile().unwrap();
    f.write_all(bytes).unwrap();
    f.flush().unwrap();
    f
}

fn zip_of(parts: &[(&str, &[u8])]) -> Vec<u8> {
    use zip::write::SimpleFileOptions;
    let mut w = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
    for (i, (name, data)) in parts.iter().enumerate() {
        let method = if i % 2 == 0 {
            zip::CompressionMethod::Deflated
        } else {
            zip::CompressionMethod::Stored
        };
        let o = SimpleFileOptions::default().compression_method(method);
        if name.ends_with('/') {
            w.add_directory(*name, o).unwrap();
        } else {
            w.start_file(*name, o).unwrap();
            w.write_all(data).unwrap();
        }
    }
    w.finish().unwrap().into_inner()
}

fn read_part(p: Part) -> Vec<u8> {
    let mut out = Vec::new();
    let mut body = p.body;
    body.read_to_end(&mut out).unwrap();
    out
}

fn names(v: &Value) -> Vec<String> {
    v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn zip_listing_and_entries() {
    let z = zip_of(&[
        ("docs/", b""),
        ("docs/a.txt", b"hello hello hello hello"),
        ("b.bin", &[0, 1, 2, 3]),
        ("π/ünï.txt", b"unicode"),
    ]);
    let f = tmp(&z, ".zip");
    let v = archive(f.path(), 100, false).unwrap();
    assert_eq!(v["format"], "zip");
    assert_eq!(v["total"], 4);
    assert_eq!(v["complete"], true);
    assert_eq!(names(&v), ["docs/", "docs/a.txt", "b.bin", "π/ünï.txt"]);
    assert_eq!(v["entries"][0]["dir"], true);
    assert_eq!(v["entries"][1]["size"], 23);
    assert_eq!(v["total_size"], 23 + 4 + 7);
    assert!(v["entries"][1]["mtime"].as_i64().unwrap() > 0);
    let p = entry(f.path(), "docs/a.txt", None).unwrap();
    assert_eq!((p.name.as_str(), p.len), ("a.txt", Some(23)));
    assert_eq!(read_part(p), b"hello hello hello hello");
    assert_eq!(
        read_part(entry(f.path(), "b.bin", None).unwrap()),
        [0, 1, 2, 3]
    );
    assert_eq!(
        read_part(entry(f.path(), "π/ünï.txt", None).unwrap()),
        b"unicode"
    );
    assert_eq!(
        read_part(entry(f.path(), "docs/a.txt", Some(5)).unwrap()),
        b"hello"
    );
    assert!(entry(f.path(), "../etc/passwd", None).is_err());
    assert!(entry(f.path(), "docs/", None).is_err());
    // a listing cap still counts everything
    let v = archive(f.path(), 1, false).unwrap();
    assert_eq!((names(&v).len(), v["total"].as_u64()), (1, Some(4)));
}

#[test]
fn zip_behind_a_stub() {
    // a self-extractor: bytes in front, offsets in the directory don't know about them
    let mut z = b"MZ this is a stub".repeat(10);
    z.extend(zip_of(&[("x.txt", b"inside")]));
    let f = tmp(&z, ".exe");
    assert_eq!(names(&archive(f.path(), 10, false).unwrap()), ["x.txt"]);
    assert_eq!(
        read_part(entry(f.path(), "x.txt", None).unwrap()),
        b"inside"
    );
}

fn tar_bytes() -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    let mut h = tar::Header::new_gnu();
    h.set_entry_type(tar::EntryType::Directory);
    h.set_size(0);
    h.set_mode(0o755);
    h.set_mtime(1_700_000_000);
    b.append_data(&mut h, "dir/", io::empty()).unwrap();
    for (name, data) in [
        ("dir/one.txt", &b"first file"[..]),
        (
            &*format!("dir/{}.txt", "long".repeat(40)),
            &b"a long name"[..],
        ),
        ("two.bin", &[9u8; 3000][..]),
    ] {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_mtime(1_700_000_000);
        b.append_data(&mut h, name, data).unwrap();
    }
    b.into_inner().unwrap()
}

#[test]
fn tar_in_every_wrapper() {
    let plain = tar_bytes();
    let long = format!("dir/{}.txt", "long".repeat(40));
    let gz = {
        let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        e.write_all(&plain).unwrap();
        e.finish().unwrap()
    };
    let bz = {
        let mut e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
        e.write_all(&plain).unwrap();
        e.finish().unwrap()
    };
    let xz = {
        let mut e =
            lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::default()).unwrap();
        e.write_all(&plain).unwrap();
        e.finish().unwrap()
    };
    let zst =
        ruzstd::encoding::compress_to_vec(&plain[..], ruzstd::encoding::CompressionLevel::Fastest);
    for (bytes, format) in [
        (plain.clone(), "tar"),
        (gz, "tar.gz"),
        (bz, "tar.bz2"),
        (xz, "tar.xz"),
        (zst, "tar.zst"),
    ] {
        let f = tmp(&bytes, ".t");
        let v = archive(f.path(), 100, false).unwrap();
        assert_eq!(v["format"], format);
        assert_eq!(v["total"], 4, "{format}: {v}");
        assert_eq!(
            names(&v),
            ["dir/", "dir/one.txt", long.as_str(), "two.bin"],
            "{format}"
        );
        assert_eq!(v["entries"][0]["dir"], true);
        assert_eq!(v["entries"][3]["size"], 3000);
        assert_eq!(v["entries"][1]["mtime"], 1_700_000_000);
        assert_eq!(
            read_part(entry(f.path(), "dir/one.txt", None).unwrap()),
            b"first file",
            "{format}"
        );
        assert_eq!(
            read_part(entry(f.path(), &long, None).unwrap()),
            b"a long name",
            "{format}"
        );
        assert_eq!(
            read_part(entry(f.path(), "two.bin", Some(10)).unwrap()),
            [9u8; 10]
        );
        assert!(entry(f.path(), "nope", None).is_err());
    }
}

#[test]
fn a_lone_compressed_file() {
    let body = b"just some text, compressed on its own\n".repeat(50);
    let mut e = flate2::GzBuilder::new()
        .filename("notes.txt")
        .mtime(1_600_000_000)
        .write(Vec::new(), flate2::Compression::default());
    e.write_all(&body).unwrap();
    let f = tmp(&e.finish().unwrap(), ".gz");
    let v = archive(f.path(), 10, false).unwrap();
    assert_eq!(v["format"], "gz");
    assert_eq!(names(&v), ["notes.txt"]);
    assert_eq!(v["entries"][0]["size"], body.len());
    assert_eq!(v["entries"][0]["mtime"], 1_600_000_000);
    assert_eq!(read_part(entry(f.path(), "notes.txt", None).unwrap()), body);
    // no name inside: ours minus the extension
    let zst =
        ruzstd::encoding::compress_to_vec(&body[..], ruzstd::encoding::CompressionLevel::Fastest);
    let f = tempfile::Builder::new().tempdir().unwrap();
    let p = f.path().join("server.log.zst");
    std::fs::write(&p, zst).unwrap();
    let v = archive(&p, 10, true).unwrap();
    assert_eq!(
        (v["format"].as_str(), names(&v)),
        (Some("zst"), vec!["server.log".to_string()])
    );
    assert_eq!(v["entries"][0]["size"], body.len());
    assert_eq!(
        read_part(entry(&p, "server.log", Some(4)).unwrap()),
        b"just"
    );
}

const XLSX_NS: &str = r#"xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;

fn xlsx() -> Vec<u8> {
    let rel = |id: &str, kind: &str, target: &str| {
        format!(
            r#"<Relationship Id="{id}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/{kind}" Target="{target}"/>"#
        )
    };
    let rels = |body: String| {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{body}</Relationships>"#
        )
    };
    let types = r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/worksheets/sheet2.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/></Types>"#;
    let root = rels(rel("rId1", "officeDocument", "xl/workbook.xml"));
    let wb = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><workbook {XLSX_NS}><sheets><sheet name="Data" sheetId="1" r:id="rId1"/><sheet name="Other" sheetId="2" r:id="rId2"/></sheets></workbook>"#
    );
    let wbrels = rels(
        rel("rId1", "worksheet", "worksheets/sheet1.xml")
            + &rel("rId2", "worksheet", "worksheets/sheet2.xml")
            + &rel("rId3", "sharedStrings", "sharedStrings.xml"),
    );
    let sst = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><sst {XLSX_NS} count="3" uniqueCount="3"><si><t>name</t></si><si><t>n</t></si><si><t>a</t></si></sst>"#
    );
    let s1 = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><worksheet {XLSX_NS}><dimension ref="A1:C4"/><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c></row><row r="2"><c r="A2" t="s"><v>2</v></c><c r="B2"><v>1</v></c></row><row r="3"><c r="A3" t="inlineStr"><is><t>b &amp; c</t></is></c><c r="B3"><v>2.5</v></c></row><row r="4"><c r="C4" t="b"><v>1</v></c></row></sheetData></worksheet>"#
    );
    let s2 = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><worksheet {XLSX_NS}><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>other</t></is></c></row></sheetData></worksheet>"#
    );
    zip_of(&[
        ("[Content_Types].xml", types.as_bytes()),
        ("_rels/.rels", root.as_bytes()),
        ("xl/workbook.xml", wb.as_bytes()),
        ("xl/_rels/workbook.xml.rels", wbrels.as_bytes()),
        ("xl/sharedStrings.xml", sst.as_bytes()),
        ("xl/worksheets/sheet1.xml", s1.as_bytes()),
        ("xl/worksheets/sheet2.xml", s2.as_bytes()),
    ])
}

#[test]
fn spreadsheet_bomb_is_refused() {
    // the central directory claims the shared strings unpack to 200 MB: refused before
    // calamine would inflate them
    let mut z = xlsx();
    let name = b"xl/sharedStrings.xml";
    let at = z.windows(name.len()).rposition(|w| w == name).unwrap() - 46;
    assert_eq!(&z[at..at + 4], b"PK\x01\x02");
    z[at + 24..at + 28].copy_from_slice(&(200u32 << 20).to_le_bytes());
    let err = sheet(tmp(&z, ".xlsx").path(), None, 0, 10).unwrap_err();
    assert!(
        err.to_string().contains("sharedStrings.xml unpacks to"),
        "{err}"
    );
}

#[test]
fn spreadsheet_pages() {
    let f = tmp(&xlsx(), ".xlsx");
    let v = sheet(f.path(), None, 0, 500).unwrap();
    assert_eq!(
        v["sheets"],
        json!([{ "name": "Data", "hidden": false }, { "name": "Other", "hidden": false }])
    );
    assert_eq!(v["sheet"], "Data");
    assert_eq!(v["rows"][0], json!(["name", "n"]));
    assert_eq!(v["rows"][1], json!(["a", 1.0]));
    assert_eq!(v["rows"][2], json!(["b & c", 2.5]));
    assert_eq!(v["rows"][3], json!([null, null, true]));
    assert_eq!(
        (
            v["width"].as_u64(),
            v["total_rows"].as_u64(),
            v["more"].as_bool()
        ),
        (Some(3), Some(4), Some(false))
    );
    let v = sheet(f.path(), Some("Data"), 1, 2).unwrap();
    assert_eq!(v["rows"], json!([["a", 1.0], ["b & c", 2.5]]));
    assert_eq!(v["more"], true);
    let v = sheet(f.path(), Some("Other"), 0, 10).unwrap();
    assert_eq!(v["rows"], json!([["other"]]));
    assert!(sheet(tmp(b"not a sheet", ".xlsx").path(), None, 0, 10).is_err());
}

const P: &str = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main""#;

fn pptx_bytes() -> Vec<u8> {
    let rels = |body: &str| {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{body}</Relationships>"#
        )
    };
    let r = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    let pres = format!(
        r#"<p:presentation {P}><p:sldIdLst><p:sldId id="256" r:id="rId2"/><p:sldId id="257" r:id="rId3"/></p:sldIdLst></p:presentation>"#
    );
    let prels = rels(&format!(
        r#"<Relationship Id="rId2" Type="{r}/slide" Target="slides/slide1.xml"/><Relationship Id="rId3" Type="{r}/slide" Target="slides/slide2.xml"/>"#
    ));
    let s1 = format!(
        r#"<p:sld {P}><p:cSld><p:spTree>
        <p:sp><p:nvSpPr><p:cNvPr id="2" name="Title"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>Hello &amp; welcome</a:t></a:r></a:p></p:txBody></p:sp>
        <p:sp><p:nvSpPr><p:cNvPr id="3" name="Body"/><p:cNvSpPr/><p:nvPr><p:ph idx="1"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>First</a:t></a:r></a:p><a:p><a:pPr lvl="1"/><a:r><a:t>Nested</a:t></a:r><a:br/><a:r><a:t>line</a:t></a:r></a:p><a:p/></p:txBody></p:sp>
        <p:pic><p:blipFill><a:blip r:embed="rId2"/></p:blipFill></p:pic>
        <p:sp><p:nvSpPr><p:cNvPr id="4" name="Num"/><p:cNvSpPr/><p:nvPr><p:ph type="sldNum"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:fld type="slidenum"><a:t>1</a:t></a:fld></a:p></p:txBody></p:sp>
        </p:spTree></p:cSld></p:sld>"#
    );
    let s1rels = rels(&format!(
        r#"<Relationship Id="rId1" Type="{r}/slideLayout" Target="../slideLayouts/slideLayout1.xml"/><Relationship Id="rId2" Type="{r}/image" Target="../media/image1.png"/>"#
    ));
    let s2 = format!(
        r#"<p:sld {P}><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="2" name="T"/><p:cNvSpPr/><p:nvPr><p:ph type="ctrTitle"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>Second</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#
    );
    let s2rels = rels(&format!(
        r#"<Relationship Id="rId9" Type="{r}/notesSlide" Target="../notesSlides/notesSlide2.xml"/>"#
    ));
    let notes = format!(
        r#"<p:notes {P}><p:cSld><p:spTree><p:sp><p:nvSpPr><p:cNvPr id="2" name="I"/><p:cNvSpPr/><p:nvPr><p:ph type="sldImg"/></p:nvPr></p:nvSpPr></p:sp><p:sp><p:nvSpPr><p:cNvPr id="3" name="N"/><p:cNvSpPr/><p:nvPr><p:ph type="body" idx="1"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>Speaker note</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:notes>"#
    );
    zip_of(&[
        ("ppt/presentation.xml", pres.as_bytes()),
        ("ppt/_rels/presentation.xml.rels", prels.as_bytes()),
        ("ppt/slides/slide1.xml", s1.as_bytes()),
        ("ppt/slides/_rels/slide1.xml.rels", s1rels.as_bytes()),
        ("ppt/slides/slide2.xml", s2.as_bytes()),
        ("ppt/slides/_rels/slide2.xml.rels", s2rels.as_bytes()),
        ("ppt/notesSlides/notesSlide2.xml", notes.as_bytes()),
        ("ppt/media/image1.png", b"\x89PNG fake"),
        ("docProps/thumbnail.jpeg", b"\xff\xd8 fake"),
    ])
}

#[test]
fn pptx_slides() {
    let f = tmp(&pptx_bytes(), ".pptx");
    let v = slides(f.path()).unwrap();
    assert_eq!(
        (v["format"].as_str(), v["count"].as_u64()),
        (Some("pptx"), Some(2))
    );
    let s = &v["slides"][0];
    assert_eq!(s["title"], "Hello & welcome");
    assert_eq!(
        s["paras"],
        json!([{ "t": "First", "lvl": 0 }, { "t": "Nested\nline", "lvl": 1 }])
    );
    assert_eq!(s["images"], json!(["ppt/media/image1.png"]));
    assert_eq!(v["slides"][1]["title"], "Second");
    assert_eq!(v["slides"][1]["notes"], "Speaker note");
    assert_eq!(v["thumbnail"], "docProps/thumbnail.jpeg");
    // the images are served by name out of the same zip
    assert_eq!(
        read_part(entry(f.path(), "ppt/media/image1.png", None).unwrap()),
        b"\x89PNG fake"
    );
}

#[test]
fn odp_slides() {
    let content = r#"<?xml version="1.0" encoding="UTF-8"?><office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0" xmlns:xlink="http://www.w3.org/1999/xlink"><office:body><office:presentation>
        <draw:page draw:name="page1"><draw:frame presentation:class="title"><draw:text-box><text:p>Plan<text:s text:c="2"/>2025</text:p></draw:text-box></draw:frame>
          <draw:frame presentation:class="outline"><draw:text-box><text:list><text:list-item><text:p>Top</text:p><text:list><text:list-item><text:p>Under</text:p></text:list-item></text:list></text:list-item></text:list></draw:text-box></draw:frame>
          <draw:frame><draw:image xlink:href="Pictures/pic.png"/></draw:frame>
          <presentation:notes><draw:frame presentation:class="notes"><draw:text-box><text:p>Say hi</text:p></draw:text-box></draw:frame></presentation:notes></draw:page>
        <draw:page draw:name="page2"><draw:frame presentation:class="title"><draw:text-box><text:p>End</text:p></draw:text-box></draw:frame></draw:page>
        </office:presentation></office:body></office:document-content>"#;
    let z = zip_of(&[
        (
            "mimetype",
            b"application/vnd.oasis.opendocument.presentation",
        ),
        ("content.xml", content.as_bytes()),
        ("Pictures/pic.png", b"png"),
    ]);
    let v = slides(tmp(&z, ".odp").path()).unwrap();
    assert_eq!(
        (v["format"].as_str(), v["count"].as_u64()),
        (Some("odp"), Some(2))
    );
    let s = &v["slides"][0];
    assert_eq!(s["title"], "Plan  2025");
    assert_eq!(
        s["paras"],
        json!([{ "t": "Top", "lvl": 0 }, { "t": "Under", "lvl": 1 }])
    );
    assert_eq!(s["images"], json!(["Pictures/pic.png"]));
    assert_eq!(s["notes"], "Say hi");
    assert_eq!(v["slides"][1]["title"], "End");
}

const EML: &str = "From: Ada Lovelace <ada@example.com>\r\nTo: Bob <bob@example.com>, carol@example.com\r\nCc: Dan <dan@example.com>\r\nSubject: =?UTF-8?B?w4lub3JtZQ==?= news\r\nDate: Tue, 2 Jan 2024 10:00:00 +0100\r\nMessage-ID: <1@example.com>\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"B\"\r\n\r\n--B\r\nContent-Type: multipart/alternative; boundary=\"A\"\r\n\r\n--A\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nHello Bob,\r\nthe plain part.\r\n--A\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>Hello <b>Bob</b></p>\r\n--A--\r\n--B\r\nContent-Type: application/pdf; name=\"report.pdf\"\r\nContent-Disposition: attachment; filename=\"report.pdf\"\r\nContent-Transfer-Encoding: base64\r\n\r\nJVBERi0xLjQK\r\n--B--\r\n";

#[test]
fn eml_message() {
    let f = tmp(EML.as_bytes(), ".eml");
    let v = mail(f.path(), None).unwrap();
    assert_eq!(v["from"], "Ada Lovelace <ada@example.com>");
    assert_eq!(v["to"], "Bob <bob@example.com>, carol@example.com");
    assert_eq!(v["cc"], "Dan <dan@example.com>");
    assert_eq!(v["subject"], "Énorme news");
    assert_eq!(v["date"], 1_704_186_000);
    assert_eq!(v["text"], "Hello Bob,\r\nthe plain part.");
    assert_eq!(v["from_html"], false);
    assert_eq!(
        v["attachments"],
        json!([{ "part": 0, "name": "report.pdf", "type": "application/pdf", "size": 9 }])
    );
    let p = attachment(f.path(), None, 0).unwrap();
    assert_eq!(p.name, "report.pdf");
    assert_eq!(read_part(p), b"%PDF-1.4\n");
    assert!(attachment(f.path(), None, 1).is_err());
}

#[test]
fn html_only_mail_becomes_text() {
    let raw = "From: a@example.com\r\nSubject: promo\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<html><head><style>p{color:red}</style><title>t</title></head><body><script>alert(1)</script><h1>Big&nbsp;news</h1><p>Line one<br>line&#32;two &amp; more</p><ul><li>first</li><li>second</li></ul><table><tr><td>a</td><td>b</td></tr></table><!-- hidden --></body></html>\r\n";
    let v = mail(tmp(raw.as_bytes(), ".eml").path(), None).unwrap();
    assert_eq!(v["from_html"], true);
    assert_eq!(
        v["text"],
        "Big news\n\nLine one\nline two & more\n\n• first\n• second\n\na   b"
    );
}

fn mbox_text() -> String {
    let msg = |n: u32, from: &str, subject: &str, body: &str| {
        format!(
            "From {from} Mon Jan  {n} 10:00:00 2024\nFrom: {from}\nSubject: {subject}\nDate: Mon, {n} Jan 2024 10:00:00 +0000\n\n{body}\n\n"
        )
    };
    msg(
        1,
        "ann@example.com",
        "Lunch",
        "See you at noon.\n>From here on, it's escaped.",
    ) + &msg(2, "bob@example.com", "Invoice 42", "Please pay.")
        + &msg(3, "ann@example.com", "Re: Invoice 42", "Paid.")
}

#[test]
fn mbox_pages_and_messages() {
    let text = mbox_text();
    let f = tmp(text.as_bytes(), ".mbox");
    let v = mbox(f.path(), 0, "", 200).unwrap();
    let msgs = v["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3);
    assert_eq!(v["next"], Value::Null);
    assert_eq!(msgs[0]["at"], 0);
    assert_eq!(msgs[1]["subject"], "Invoice 42");
    assert_eq!(msgs[1]["from"], "bob@example.com");
    let second = text.find("From bob").unwrap() as u64;
    assert_eq!(msgs[1]["at"], second);
    assert_eq!(msgs[0]["size"], second);
    // paging: two per page, then carry on from `next`
    let p1 = mbox(f.path(), 0, "", 2).unwrap();
    assert_eq!(p1["messages"].as_array().unwrap().len(), 2);
    let next = p1["next"].as_u64().unwrap();
    let p2 = mbox(f.path(), next, "", 2).unwrap();
    assert_eq!(p2["messages"][0]["subject"], "Re: Invoice 42");
    assert_eq!(p2["next"], Value::Null);
    // search over sender and subject
    let s = mbox(f.path(), 0, "INVOICE", 200).unwrap();
    assert_eq!(s["messages"].as_array().unwrap().len(), 2);
    let s = mbox(f.path(), 0, "ann@", 200).unwrap();
    assert_eq!(s["messages"].as_array().unwrap().len(), 2);
    // one message by its offset, ">From" unescaped, and nothing of the next one
    let m = mail(f.path(), Some(0)).unwrap();
    assert_eq!(m["subject"], "Lunch");
    assert_eq!(m["text"], "See you at noon.\nFrom here on, it's escaped.");
    let m = mail(f.path(), Some(second)).unwrap();
    assert_eq!(
        (m["subject"].as_str(), m["text"].as_str()),
        (Some("Invoice 42"), Some("Please pay."))
    );
    assert!(mail(f.path(), Some(5)).is_err());
}

fn put(b: &mut [u8], at: usize, v: u64, n: usize) {
    for i in 0..n {
        b[at + i] = (v >> (8 * i)) as u8;
    }
}

fn put_be(b: &mut [u8], at: usize, v: u64, n: usize) {
    for i in 0..n {
        b[at + i] = (v >> (8 * (n - 1 - i))) as u8;
    }
}

fn guid_bytes(s: &str) -> Vec<u8> {
    let h: Vec<u8> = s
        .replace('-', "")
        .as_bytes()
        .chunks(2)
        .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
        .collect();
    let mut g = vec![h[3], h[2], h[1], h[0], h[5], h[4], h[7], h[6]];
    g.extend_from_slice(&h[8..]);
    g
}

#[test]
fn mbr_with_filesystems() {
    let mut img = vec![0u8; 3 << 20];
    img[510] = 0x55;
    img[511] = 0xAA;
    let (e1, e2) = (446, 462);
    img[e1] = 0x80;
    img[e1 + 4] = 0x83;
    put(&mut img, e1 + 8, 2048, 4);
    put(&mut img, e1 + 12, 2048, 4);
    img[e2 + 4] = 0x0C;
    put(&mut img, e2 + 8, 4096, 4);
    put(&mut img, e2 + 12, 2048, 4);
    // ext4 at 1 MiB: 1 KiB blocks, 1024 of them, extents, label "root"
    let sb = (1 << 20) + 1024;
    put(&mut img, sb + 4, 1024, 4);
    put(&mut img, sb + 56, 0xEF53, 2);
    put(&mut img, sb + 96, 0x40, 4);
    img[sb + 120..sb + 124].copy_from_slice(b"root");
    // FAT32 at 2 MiB, label "BOOT"
    let v = 2 << 20;
    img[v..v + 3].copy_from_slice(&[0xEB, 0x58, 0x90]);
    put(&mut img, v + 11, 512, 2);
    img[v + 13] = 8;
    put(&mut img, v + 14, 32, 2);
    img[v + 16] = 2;
    put(&mut img, v + 32, 2048, 4);
    img[v + 71..v + 82].copy_from_slice(b"BOOT       ");
    img[v + 82..v + 90].copy_from_slice(b"FAT32   ");
    img[v + 510] = 0x55;
    img[v + 511] = 0xAA;
    let d = disk(tmp(&img, ".img").path(), None).unwrap();
    assert_eq!(
        (d["format"].as_str(), d["table"].as_str()),
        (Some("raw"), Some("MBR"))
    );
    let p = d["partitions"].as_array().unwrap();
    assert_eq!(p.len(), 2);
    assert_eq!(
        (
            p[0]["type"].as_str(),
            p[0]["start"].as_u64(),
            p[0]["size"].as_u64()
        ),
        (Some("Linux"), Some(1 << 20), Some(1 << 20))
    );
    assert_eq!(p[0]["bootable"], true);
    assert_eq!(
        p[0]["fs"],
        json!({ "kind": "ext4", "label": "root", "size": 1 << 20 })
    );
    assert_eq!(
        p[1]["fs"],
        json!({ "kind": "FAT32", "label": "BOOT", "size": 1 << 20 })
    );
}

#[test]
fn gpt_with_filesystems() {
    let mut img = vec![0u8; 2 << 20];
    img[446 + 4] = 0xEE;
    put(&mut img, 446 + 8, 1, 4);
    put(&mut img, 446 + 12, 0xFFFF_FFFF, 4);
    img[510] = 0x55;
    img[511] = 0xAA;
    let h = 512;
    img[h..h + 8].copy_from_slice(b"EFI PART");
    img[h + 56..h + 72].copy_from_slice(&guid_bytes("11111111-2222-3333-4444-555555555555"));
    put(&mut img, h + 72, 2, 8);
    put(&mut img, h + 80, 128, 4);
    put(&mut img, h + 84, 128, 4);
    let e = 1024;
    img[e..e + 16].copy_from_slice(&guid_bytes("0FC63DAF-8483-4772-8E79-3D69D8477DE4"));
    put(&mut img, e + 32, 2048, 8);
    put(&mut img, e + 40, 3071, 8);
    for (i, c) in "data".encode_utf16().enumerate() {
        put(&mut img, e + 56 + 2 * i, u64::from(c), 2);
    }
    let e = 1024 + 128;
    img[e..e + 16].copy_from_slice(&guid_bytes("EBD0A0A2-B9E5-4433-87C0-68B6B72699C7"));
    put(&mut img, e + 32, 3072, 8);
    put(&mut img, e + 40, 4095, 8);
    // XFS in the first, NTFS in the second
    let x = 2048 * 512;
    img[x..x + 4].copy_from_slice(b"XFSB");
    put_be(&mut img, x + 4, 4096, 4);
    put_be(&mut img, x + 8, 128, 8);
    img[x + 108..x + 114].copy_from_slice(b"xfsvol");
    let n = 3072 * 512;
    img[n + 3..n + 11].copy_from_slice(b"NTFS    ");
    put(&mut img, n + 11, 512, 2);
    put(&mut img, n + 40, 1024, 8);
    let d = disk(tmp(&img, ".img").path(), None).unwrap();
    assert_eq!(d["table"], "GPT");
    let p = d["partitions"].as_array().unwrap();
    assert_eq!(p.len(), 2);
    assert_eq!(
        (p[0]["type"].as_str(), p[0]["name"].as_str()),
        (Some("Linux filesystem"), Some("data"))
    );
    assert_eq!(
        p[0]["fs"],
        json!({ "kind": "XFS", "label": "xfsvol", "size": 128 * 4096 })
    );
    assert_eq!(p[1]["type"], "Microsoft basic data");
    assert_eq!(p[1]["fs"]["kind"], "NTFS");
    assert_eq!(p[1]["fs"]["size"], 1024 * 512);
    assert!(
        d["details"]
            .to_string()
            .contains("11111111-2222-3333-4444-555555555555")
    );
}

#[test]
fn a_bare_filesystem_image() {
    let mut img = vec![0u8; 1 << 20];
    let sb = 0x10000;
    img[sb + 0x40..sb + 0x48].copy_from_slice(b"_BHRfS_M");
    put(&mut img, sb + 0x70, 1 << 30, 8);
    img[sb + 0x12B..sb + 0x12B + 4].copy_from_slice(b"pool");
    let d = disk(tmp(&img, ".img").path(), None).unwrap();
    assert_eq!(d["table"], Value::Null);
    assert_eq!(
        d["fs"],
        json!({ "kind": "btrfs", "label": "pool", "size": 1u64 << 30 })
    );
    let d = disk(tmp(&[7u8; 5000], ".bin").path(), None).unwrap();
    assert_eq!(d["format"], "unknown");
}

fn iso_record(name: &[u8], lba: u64, len: u64, dir: bool) -> Vec<u8> {
    let nl = name.len();
    let rl = 33 + nl + (1 - nl % 2);
    let mut r = vec![0u8; rl];
    r[0] = rl as u8;
    put(&mut r, 2, lba, 4);
    put_be(&mut r, 6, lba, 4);
    put(&mut r, 10, len, 4);
    put_be(&mut r, 14, len, 4);
    r[18..25].copy_from_slice(&[124, 1, 2, 3, 4, 5, 0]);
    r[25] = if dir { 2 } else { 0 };
    r[32] = nl as u8;
    r[33..33 + nl].copy_from_slice(name);
    r
}

fn iso_bytes() -> Vec<u8> {
    let mut img = vec![0u8; 22 * 2048];
    let p = 16 * 2048;
    img[p] = 1;
    img[p + 1..p + 6].copy_from_slice(b"CD001");
    img[p + 6] = 1;
    img[p + 40..p + 72].copy_from_slice(format!("{:<32}", "TESTDISC").as_bytes());
    put(&mut img, p + 80, 22, 4);
    put(&mut img, p + 128, 2048, 2);
    let root = iso_record(&[0], 18, 2048, true);
    img[p + 156..p + 156 + 34].copy_from_slice(&root);
    let t = 17 * 2048;
    img[t] = 255;
    img[t + 1..t + 6].copy_from_slice(b"CD001");
    let mut dir = |at: usize, recs: Vec<Vec<u8>>| {
        let mut o = at;
        for r in recs {
            img[o..o + r.len()].copy_from_slice(&r);
            o += r.len();
        }
    };
    dir(
        18 * 2048,
        vec![
            iso_record(&[0], 18, 2048, true),
            iso_record(&[1], 18, 2048, true),
            iso_record(b"DOCS", 19, 2048, true),
            iso_record(b"README.TXT;1", 20, 5, false),
        ],
    );
    dir(
        19 * 2048,
        vec![
            iso_record(&[0], 19, 2048, true),
            iso_record(&[1], 18, 2048, true),
            iso_record(b"A.TXT;1", 21, 3, false),
        ],
    );
    img[20 * 2048..20 * 2048 + 5].copy_from_slice(b"hello");
    img[21 * 2048..21 * 2048 + 3].copy_from_slice(b"abc");
    img
}

#[test]
fn iso_listing_and_files() {
    let f = tmp(&iso_bytes(), ".iso");
    let d = disk(f.path(), None).unwrap();
    assert_eq!(
        (d["format"].as_str(), d["label"].as_str()),
        (Some("iso9660"), Some("TESTDISC"))
    );
    assert_eq!(names(&d), ["DOCS", "README.TXT"]);
    assert_eq!(d["entries"][0]["dir"], true);
    assert_eq!(d["entries"][1]["size"], 5);
    assert_eq!(
        d["entries"][1]["mtime"],
        days_from_civil(2024, 1, 2) * 86_400 + 3 * 3600 + 4 * 60 + 5
    );
    let d = disk(f.path(), Some("/DOCS")).unwrap();
    assert_eq!(names(&d), ["A.TXT"]);
    assert_eq!(
        read_part(entry(f.path(), "DOCS/A.TXT", None).unwrap()),
        b"abc"
    );
    assert_eq!(
        read_part(entry(f.path(), "/readme.txt", None).unwrap()),
        b"hello"
    );
    assert!(disk(f.path(), Some("/NOPE")).is_err());
    assert!(entry(f.path(), "DOCS", None).is_err());
}

#[test]
fn virtual_disks() {
    let mut q = vec![0u8; 512];
    q[..4].copy_from_slice(b"QFI\xfb");
    put_be(&mut q, 4, 3, 4);
    put_be(&mut q, 20, 16, 4);
    put_be(&mut q, 24, 10 << 30, 8);
    let d = disk(tmp(&q, ".qcow2").path(), None).unwrap();
    assert_eq!(
        (d["format"].as_str(), d["virtual_size"].as_u64()),
        (Some("qcow2"), Some(10 << 30))
    );

    let mut v = vec![0u8; 1024];
    v[..40].copy_from_slice(b"<<< Oracle VM VirtualBox Disk Image >>>\n");
    put(&mut v, 0x40, 0xBEDA_107F, 4);
    put(&mut v, 0x44, 0x0001_0001, 4);
    put(&mut v, 0x4C, 1, 4);
    put(&mut v, 0x170, 8 << 30, 8);
    put(&mut v, 0x178, 1 << 20, 4);
    put(&mut v, 0x184, 3, 4);
    let d = disk(tmp(&v, ".vdi").path(), None).unwrap();
    assert_eq!(
        (d["format"].as_str(), d["virtual_size"].as_u64()),
        (Some("vdi"), Some(8 << 30))
    );
    assert!(d["details"].to_string().contains("dynamic"));

    let mut h = vec![0u8; (1 << 20) + 512];
    let ft = 1 << 20;
    h[ft..ft + 8].copy_from_slice(b"conectix");
    h[ft + 28..ft + 32].copy_from_slice(b"test");
    put_be(&mut h, ft + 48, 1 << 20, 8);
    put_be(&mut h, ft + 60, 2, 4);
    let d = disk(tmp(&h, ".vhd").path(), None).unwrap();
    assert_eq!(
        (d["format"].as_str(), d["virtual_size"].as_u64()),
        (Some("vhd"), Some(1 << 20))
    );
    assert!(d["details"].to_string().contains("fixed"));

    let desc = "# Disk DescriptorFile\nversion=1\ncreateType=\"monolithicFlat\"\n\n# Extent description\nRW 2097152 FLAT \"disk-flat.vmdk\" 0\n\nddb.adapterType = \"lsilogic\"\n";
    let d = disk(tmp(desc.as_bytes(), ".vmdk").path(), None).unwrap();
    assert_eq!(
        (d["format"].as_str(), d["virtual_size"].as_u64()),
        (Some("vmdk"), Some(1 << 30))
    );
    assert!(d["details"].to_string().contains("monolithicFlat"));

    let mut x = vec![0u8; 320 << 10];
    x[..8].copy_from_slice(b"vhdxfile");
    let rt = 192 << 10;
    x[rt..rt + 4].copy_from_slice(b"regi");
    put(&mut x, rt + 8, 1, 4);
    x[rt + 16..rt + 32].copy_from_slice(&guid_bytes(VHDX_METADATA));
    put(&mut x, rt + 32, 256 << 10, 8);
    let mt = 256 << 10;
    x[mt..mt + 8].copy_from_slice(b"metadata");
    put(&mut x, mt + 10, 1, 2);
    x[mt + 32..mt + 48].copy_from_slice(&guid_bytes(VHDX_SIZE));
    put(&mut x, mt + 48, 0x8000, 4);
    put(&mut x, mt + 0x8000, 20 << 30, 8);
    let d = disk(tmp(&x, ".vhdx").path(), None).unwrap();
    assert_eq!(
        (d["format"].as_str(), d["virtual_size"].as_u64()),
        (Some("vhdx"), Some(20 << 30))
    );
}

#[test]
fn dmg_trailer_and_plist() {
    use base64::Engine;
    let mut mish = vec![0u8; 204 + 2 * 40];
    mish[..4].copy_from_slice(b"mish");
    put_be(&mut mish, 16, 2048, 8);
    put_be(&mut mish, 200, 2, 4);
    put_be(&mut mish, 204, 0x8000_0005, 4);
    put_be(&mut mish, 244, 0xFFFF_FFFF, 4);
    let b64 = base64::engine::general_purpose::STANDARD.encode(&mish);
    let plist = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict><key>resource-fork</key><dict><key>blkx</key><array><dict><key>Attributes</key><string>0x0050</string><key>CFName</key><string>disk image (Apple_HFS : 1)</string><key>Data</key><data>\n{b64}\n</data><key>ID</key><string>0</string><key>Name</key><string>disk image (Apple_HFS : 1)</string></dict></array></dict></dict></plist>"
    );
    let mut img = vec![0u8; 1000];
    img.extend_from_slice(plist.as_bytes());
    let mut k = vec![0u8; 512];
    k[..4].copy_from_slice(b"koly");
    put_be(&mut k, 4, 4, 4);
    put_be(&mut k, 8, 512, 4);
    put_be(&mut k, 32, 1000, 8);
    put_be(&mut k, 216, 1000, 8);
    put_be(&mut k, 224, plist.len() as u64, 8);
    put_be(&mut k, 492, 2048, 8);
    img.extend_from_slice(&k);
    let d = disk(tmp(&img, ".dmg").path(), None).unwrap();
    assert_eq!(
        (d["format"].as_str(), d["virtual_size"].as_u64()),
        (Some("dmg"), Some(1 << 20))
    );
    assert_eq!(d["compressed"], true);
    assert!(d["details"].to_string().contains("zlib (UDZO)"));
    assert_eq!(
        d["dmg_parts"],
        json!([{ "name": "disk image (Apple_HFS : 1)", "size": 1 << 20 }])
    );
}

#[test]
fn small_helpers() {
    assert_eq!(days_from_civil(1970, 1, 1), 0);
    assert_eq!(days_from_civil(2024, 3, 1), 19_783);
    assert_eq!(dos_time(0x5821, 0x6000), Some(1_704_110_400)); // 2024-01-01 12:00:00
    assert_eq!(cp437(b"caf\x82"), "café");
    assert_eq!(resolve("ppt/slides", "../media/a.png"), "ppt/media/a.png");
    assert_eq!(resolve("ppt", "/ppt/slides/s.xml"), "ppt/slides/s.xml");
    assert_eq!(
        guid(&guid_bytes("C12A7328-F81F-11D2-BA4B-00A0C93EC93B")),
        "C12A7328-F81F-11D2-BA4B-00A0C93EC93B"
    );
    assert!(is_from_line(b"From x Mon Jan  1 00:00:00 2024\n", false));
    assert!(!is_from_line(b"From here we go\n", false));
    assert!(is_from_line(b"From here we go\n", true));
    assert_eq!(
        decode_entities("a &amp; b &#x41;&#66; &bogus; &"),
        "a & b AB &bogus; &"
    );
    // a deck of plain text boxes: the first short line stands in for the title
    let mut s = Slide {
        paras: vec![("Loose title".into(), 0), ("body".into(), 0)],
        ..Default::default()
    };
    s.settle();
    assert_eq!((s.title.as_str(), s.paras.len()), ("Loose title", 1));
}
