//! Slide decks: PowerPoint (pptx) and OpenDocument (odp), slide by slide.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result, bail};
use quick_xml::events::Event;
use serde_json::{Value, json};

use super::*;

// ---- slide decks: PowerPoint (pptx) and OpenDocument (odp)

#[derive(Default)]
pub(super) struct Slide {
    pub(super) title: String,
    pub(super) paras: Vec<(String, u32)>,
    pub(super) images: Vec<String>,
    pub(super) notes: Vec<String>,
}

impl Slide {
    /// No title placeholder (text boxes only, as many generators write): a short first line is
    /// the title.
    pub(super) fn settle(&mut self) {
        if self.title.is_empty()
            && let Some((t, 0)) = self.paras.first()
            && t.len() <= 200
            && !t.contains('\n')
        {
            self.title = self.paras.remove(0).0;
        }
    }

    /// Slide `n` for the viewer; its text only while the deck's `room` for text lasts.
    pub(super) fn json(&self, n: usize, room: &mut usize) -> Value {
        let len = self.paras.iter().map(|(t, _)| t.len()).sum::<usize>()
            + self.notes.iter().map(String::len).sum::<usize>();
        let fits = len <= *room;
        *room = room.saturating_sub(len);
        json!({
            "n": n,
            "title": self.title,
            "paras": if fits { self.paras.iter().map(|(t, l)| json!({ "t": t, "lvl": l })).collect() } else { Vec::new() },
            "images": self.images,
            "notes": if fits { self.notes.join("\n") } else { String::new() },
            "cut": !fits,
        })
    }
}

/// The folder part of a package part name ("ppt/slides/slide1.xml" -> "ppt/slides").
pub(super) fn part_dir(name: &str) -> &str {
    name.rsplit_once('/').map_or("", |(d, _)| d)
}

/// A relative target resolved against a folder inside the package.
pub(super) fn resolve(dir: &str, target: &str) -> String {
    let mut out: Vec<&str> = if target.starts_with('/') {
        Vec::new()
    } else {
        dir.split('/').filter(|s| !s.is_empty()).collect()
    };
    for seg in target.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            s => out.push(s),
        }
    }
    out.join("/")
}

/// A part's relationships: id -> (resolved target, type).
pub(super) fn rels(z: &Zip, part: &str) -> Result<HashMap<String, (String, String)>> {
    let dir = part_dir(part);
    let file = part.rsplit('/').next().unwrap_or(part);
    let name = if dir.is_empty() {
        format!("_rels/{file}.rels")
    } else {
        format!("{dir}/_rels/{file}.rels")
    };
    let mut out = HashMap::new();
    let Some(bytes) = z.read(&name, XML_CAP)? else {
        return Ok(out);
    };
    let mut r = xml(&bytes);
    loop {
        match r.read_event()? {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == b"Relationship" => {
                if attr(&e, b"TargetMode").as_deref() == Some("External") {
                    continue;
                }
                if let (Some(id), Some(target)) = (attr(&e, b"Id"), attr(&e, b"Target")) {
                    let kind = attr(&e, b"Type").unwrap_or_default();
                    out.insert(id, (resolve(dir, &target), kind));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

/// A paragraph: its text, bullet level, and the placeholder type of its shape ("title", "body",
/// "" for none).
pub(super) type Para = (String, u32, String);

/// A PresentationML part's paragraphs, plus the images it embeds.
pub(super) fn ooxml_text(
    bytes: &[u8],
    rels: &HashMap<String, (String, String)>,
) -> Result<(Vec<Para>, Vec<String>)> {
    let mut r = xml(bytes);
    let (mut paras, mut images) = (Vec::new(), Vec::new());
    let (mut ph, mut para, mut lvl, mut in_t) = (String::new(), None::<String>, 0u32, false);
    loop {
        let ev = r.read_event()?;
        let empty = matches!(ev, Event::Empty(_));
        match ev {
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref() {
                b"sp" if !empty => ph.clear(),
                b"ph" => ph = attr(&e, b"type").unwrap_or_else(|| "body".into()),
                b"p" if !empty => {
                    para = Some(String::new());
                    lvl = 0;
                }
                b"pPr" => lvl = attr(&e, b"lvl").and_then(|v| v.parse().ok()).unwrap_or(0),
                b"t" if !empty => in_t = true,
                b"br" => {
                    if let Some(p) = para.as_mut() {
                        p.push('\n');
                    }
                }
                b"blip" => {
                    if let Some((target, _)) = attr_ns(&e, b"embed").and_then(|id| rels.get(&id))
                        && !images.contains(target)
                        && images.len() < 50
                    {
                        images.push(target.clone());
                    }
                }
                _ => {}
            },
            Event::Text(t) if in_t => {
                if let Some(p) = para.as_mut() {
                    p.push_str(&t.decode().unwrap_or_default());
                }
            }
            Event::GeneralRef(g) if in_t => {
                if let (Some(p), Some(c)) = (para.as_mut(), entity(&g)) {
                    p.push(c);
                }
            }
            Event::End(e) => match e.local_name().as_ref() {
                b"t" => in_t = false,
                b"p" => {
                    if let Some(p) = para.take() {
                        let p = p.trim();
                        if !p.is_empty() && paras.len() < 500 {
                            paras.push((clip(p, 10_000).into_owned(), lvl, ph.clone()));
                        }
                    }
                }
                b"sp" => ph.clear(),
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Ok((paras, images))
}

pub(super) fn pptx(z: &Zip) -> Result<Value> {
    let pres = z
        .read("ppt/presentation.xml", XML_CAP)?
        .context("not a PowerPoint deck (no ppt/presentation.xml)")?;
    let prels = rels(z, "ppt/presentation.xml")?;
    let mut order = Vec::new();
    let mut r = xml(&pres);
    loop {
        match r.read_event()? {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == b"sldId" => {
                if let Some((target, _)) = attr_ns(&e, b"id").and_then(|id| prels.get(&id)) {
                    order.push(target.clone());
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if order.is_empty() {
        // no slide list: take ppt/slides/slideN.xml in number order
        let mut names: Vec<(u32, String)> = z
            .entries
            .keys()
            .filter_map(|n| {
                let num = n.strip_prefix("ppt/slides/slide")?.strip_suffix(".xml")?;
                Some((num.parse().ok()?, n.clone()))
            })
            .collect();
        names.sort();
        order = names.into_iter().map(|(_, n)| n).collect();
    }
    let (mut slides, mut room) = (Vec::new(), 2 * TEXT_CAP);
    for (i, name) in order.iter().take(MAX_SLIDES).enumerate() {
        let Some(bytes) = z.read(name, XML_CAP)? else {
            continue;
        };
        let srels = rels(z, name)?;
        let (paras, images) = ooxml_text(&bytes, &srels)?;
        let mut s = Slide {
            images,
            ..Default::default()
        };
        for (t, lvl, ph) in paras {
            match ph.as_str() {
                "title" | "ctrTitle" => {
                    if !s.title.is_empty() {
                        s.title.push(' ');
                    }
                    s.title.push_str(&t.replace('\n', " "));
                }
                "dt" | "ftr" | "sldNum" | "hdr" => {}
                _ => s.paras.push((t, lvl)),
            }
        }
        if let Some((notes, _)) = srels.values().find(|(_, k)| k.ends_with("/notesSlide"))
            && let Some(nb) = z.read(notes, XML_CAP)?
        {
            let (np, _) = ooxml_text(&nb, &HashMap::new())?;
            // the notes placeholder, or plain text boxes (not the slide image, number, header...)
            s.notes = np
                .into_iter()
                .filter(|(_, _, ph)| ph == "body" || ph.is_empty())
                .map(|(t, _, _)| t)
                .collect();
        }
        s.settle();
        slides.push(s.json(i + 1, &mut room));
    }
    let thumb = [
        "docProps/thumbnail.jpeg",
        "docProps/thumbnail.jpg",
        "docProps/thumbnail.png",
    ]
    .into_iter()
    .find(|n| z.get(n).is_some());
    Ok(json!({ "format": "pptx", "count": order.len(), "slides": slides, "thumbnail": thumb }))
}

pub(super) fn odp(z: &Zip) -> Result<Value> {
    let bytes = z
        .read("content.xml", XML_CAP)?
        .context("not an OpenDocument presentation (no content.xml)")?;
    let mut r = xml(&bytes);
    let (mut slides, mut room, mut count) = (Vec::new(), 2 * TEXT_CAP, 0usize);
    let mut cur: Option<Slide> = None;
    let (mut class, mut in_notes, mut lists) = (String::new(), false, 0u32);
    let (mut para, mut para_lvl) = (None::<String>, 0u32);
    loop {
        let ev = r.read_event()?;
        let empty = matches!(ev, Event::Empty(_));
        match ev {
            Event::Start(e) | Event::Empty(e) => match e.local_name().as_ref() {
                b"page" if !empty => cur = Some(Slide::default()),
                b"frame" => class = attr(&e, b"class").unwrap_or_default(),
                b"notes" if !empty => in_notes = true,
                b"list" if !empty => lists += 1,
                b"p" | b"h" if !empty => {
                    para = Some(String::new());
                    para_lvl = lists.saturating_sub(1);
                }
                b"s" => {
                    if let Some(p) = para.as_mut() {
                        let n = attr(&e, b"c")
                            .and_then(|c| c.parse().ok())
                            .unwrap_or(1usize);
                        p.extend(std::iter::repeat_n(' ', n.min(100)));
                    }
                }
                b"tab" => {
                    if let Some(p) = para.as_mut() {
                        p.push('\t');
                    }
                }
                b"line-break" => {
                    if let Some(p) = para.as_mut() {
                        p.push('\n');
                    }
                }
                b"image" => {
                    if let (Some(s), Some(href)) = (cur.as_mut(), attr(&e, b"href"))
                        && !in_notes
                        && !href.contains("://")
                    {
                        let href = resolve("", &href);
                        if z.get(&href).is_some()
                            && !s.images.contains(&href)
                            && s.images.len() < 50
                        {
                            s.images.push(href);
                        }
                    }
                }
                _ => {}
            },
            Event::Text(t) => {
                if let Some(p) = para.as_mut() {
                    p.push_str(&t.decode().unwrap_or_default());
                }
            }
            Event::GeneralRef(g) => {
                if let (Some(p), Some(c)) = (para.as_mut(), entity(&g)) {
                    p.push(c);
                }
            }
            Event::End(e) => match e.local_name().as_ref() {
                b"p" | b"h" => {
                    if let (Some(s), Some(p)) = (cur.as_mut(), para.take()) {
                        let p = p.trim();
                        if p.is_empty() {
                        } else if in_notes {
                            s.notes.push(p.to_string());
                        } else if class == "title" {
                            if !s.title.is_empty() {
                                s.title.push(' ');
                            }
                            s.title.push_str(&p.replace('\n', " "));
                        } else if s.paras.len() < 500 {
                            s.paras.push((clip(p, 10_000).into_owned(), para_lvl));
                        }
                    }
                }
                b"list" => lists = lists.saturating_sub(1),
                b"frame" => class.clear(),
                b"notes" => in_notes = false,
                b"page" => {
                    if let Some(mut s) = cur.take() {
                        count += 1;
                        if slides.len() < MAX_SLIDES {
                            s.settle();
                            slides.push(s.json(count, &mut room));
                        }
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    let thumb = z
        .get("Thumbnails/thumbnail.png")
        .map(|_| "Thumbnails/thumbnail.png");
    Ok(json!({ "format": "odp", "count": count, "slides": slides, "thumbnail": thumb }))
}

/// A deck's slides in order: title, paragraphs with bullet levels, embedded images (by their entry
/// name, for `entry`) and speaker notes.
pub fn slides(path: &Path) -> Result<Value> {
    let z = Zip::open(path)?;
    if z.get("ppt/presentation.xml").is_some() {
        pptx(&z)
    } else if z.get("content.xml").is_some() {
        odp(&z)
    } else {
        bail!("not a slide deck BackDown can read")
    }
}
