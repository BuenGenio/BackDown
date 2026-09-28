// Build the campaign deliverables from reviewed content/schedule JSON and the production logo.
// Dependencies: @resvg/resvg-js, fontkit, docx, exceljs, pdfkit.
const fs = require('node:fs');
const path = require('node:path');
const root = __dirname;
const repo = path.resolve(root, '../../..');
const dep = process.env.BACKDOWN_CAMPAIGN_MODULES ? require('node:module').createRequire(path.join(process.env.BACKDOWN_CAMPAIGN_MODULES, '../package.json')) : require;
const { Resvg } = dep('@resvg/resvg-js');
const fontkit = dep('fontkit');
const PDFDocument = dep('pdfkit');
const ExcelJS = dep('exceljs');
const docx = dep('docx');
const content = JSON.parse(fs.readFileSync(path.join(root, 'content.json')));
const schedule = JSON.parse(fs.readFileSync(path.join(root, 'schedule.json')));
const display = fontkit.openSync(path.join(repo, 'marketing/public/fonts/bricolage-normal.woff2'));
const body = fontkit.openSync(path.join(repo, 'marketing/public/fonts/instrument-normal.woff2'));
const mark = fs.readFileSync(path.join(repo, 'marketing/public/brand/backdown-mark.svg'), 'utf8').match(/ d="([^"]+)"/)[1];
for (const dir of ['graphics', 'carousel', 'profile']) fs.mkdirSync(path.join(root, dir), { recursive: true });
const C = { orange: '#d8432b', ivory: '#f5f3ec', ink: '#17161b', muted: '#77717b', line: '#d8d3c8', darkLine: '#49434b' };
const esc = s => String(s).replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;').replaceAll('"', '&quot;');
function measure(str, size, font = body) { return font.layout(str).positions.reduce((s, p) => s + p.xAdvance, 0) * size / font.unitsPerEm; }
function line(str, x, y, size, color, font = body) {
  const r = font.layout(str), scale = size / font.unitsPerEm; let at = 0;
  const shapes = r.glyphs.map((g, i) => { const p = r.positions[i]; const s = `<path transform="translate(${at + p.xOffset} ${p.yOffset})" d="${g.path.toSVG()}"/>`; at += p.xAdvance; return s; }).join('');
  return `<g aria-label="${esc(str)}" fill="${color}" transform="translate(${x} ${y}) scale(${scale} ${-scale})">${shapes}</g>`;
}
function wrap(str, max, size, font = body) { const out = []; let s = ''; for (const word of str.split(/\s+/)) { const next = s ? s + ' ' + word : word; if (s && measure(next, size, font) > max) { out.push(s); s = word; } else s = next; } if (s) out.push(s); return out; }
function block(str, x, y, size, width, color, font = body, leading = 1.2) { return wrap(str, width, size, font).map((s, i) => line(s, x, y + size * leading * i, size, color, font)).join(''); }
function symbol(x, y, width, color = C.orange) { return `<path fill="${color}" fill-rule="evenodd" transform="translate(${x} ${y}) scale(${width / 80})" d="${mark}"/>`; }
function brand(x, y, color = C.ink, size = 36) { return symbol(x, y - size * .79, size * 1.32) + line('backdown', x + size * 1.62, y, size, color, display); }
function svg(w, h, bg, art, title) { return `<svg xmlns="http://www.w3.org/2000/svg" width="${w}" height="${h}" viewBox="0 0 ${w} ${h}" role="img"><title>${esc(title)}</title><rect width="${w}" height="${h}" fill="${bg}"/>${art}</svg>`; }
function exportSvg(name, data) { fs.writeFileSync(path.join(root, name + '.svg'), data); const png = new Resvg(data).render().asPng(); fs.writeFileSync(path.join(root, name + '.png'), png); if (png.length > 1800000) throw new Error(name + ' exceeds campaign image size budget'); }
function labelBox(x, y, w, title, sub, dark) { const fg = dark ? C.ivory : C.ink; return `<rect x="${x}" y="${y}" width="${w}" height="172" rx="18" fill="${dark ? '#242127' : '#ebe7dc'}"/>` + line(title, x + 25, y + 58, 29, fg, display) + block(sub, x + 25, y + 105, 22, w - 50, dark ? '#bbb5bd' : '#65616a'); }
function motif(p, dark) {
  const fg = dark ? C.ivory : C.ink, soft = dark ? '#bbb5bd' : '#65616a';
  if (p.id === 'P01' || p.id === 'P12') return symbol(810, 785, 280) + line('THE OTHER COPY', 80, 860, 27, C.orange, display) + block('Start with the files.\nFollow the evidence.', 80, 920, 37, 620, fg, display);
  if (p.id === 'P08') return ['27M', '15 TB', '5.9 TB'].map((v, i) => line(v, 80 + i * 360, 875, 92, i === 0 ? C.orange : fg, display) + line(['files indexed', 'read and hashed', 'duplication found'][i], 80 + i * 360, 929, 26, soft)).join('') + line('One founding archive. Not a benchmark or promised saving.', 80, 1005, 23, soft);
  const pairs = {
    P02: [['backup', 'Which drive? Which year?'], ['backup-old', 'Start with an inventory.']],
    P03: [['MATCHING CONTENTS', 'A comparison result.'], ['A COPY WORTH KEEPING', 'A backup decision.']],
    P04: [['FILE A', 'Compare the contents.'], ['FILE B', 'Inspect the matching hash.']],
    P05: [['YOUR DRIVES', 'The files stay with you.'], ['YOUR MACHINE', 'Local. No account. No telemetry.']],
    P06: [['DRIVE / FOLDER / FILE', 'Keep the location visible.'], ['SCAN / RESULT', 'Follow a result back to its context.']],
    P07: [['1  INDEX + BROWSE', 'Start with a small known folder.'], ['2  HASH + COMPARE', 'Try a known second copy.']],
    P09: [['PUBLIC ALPHA', 'Inventory, hashing, local browser.'], ['PLANNED', 'Native desktop. Broader workflows.']],
    P10: [['WHAT DID YOU EXPECT?', 'Describe the result you needed.'], ['WHAT HAPPENED?', 'Version + OS + a small example.']],
    P11: [['holiday.jpg', 'A name is a label.'], ['holiday-copy.jpg', 'Contents need their own check.']]
  };
  return (pairs[p.id] || []).map((a, i) => labelBox(80 + i * 540, 799, 500, ...a, dark)).join('');
}
function makeCards() {
  for (const p of content.posts) {
    const dark = ['P03', 'P05', 'P08', 'P11'].includes(p.id), bg = dark ? C.ink : C.ivory, fg = dark ? C.ivory : C.ink;
    let size = 90; while (wrap(p.visual_headline, 1040, size, display).length > 3) size -= 2;
    const head = wrap(p.visual_headline, 1040, size, display), lastY = 330 + (head.length - 1) * size * 1.08;
    const art = brand(80, 117, fg) + line('THE OTHER COPY  /  ' + p.pillar.toUpperCase(), 80, 222, 19, dark ? '#c4bec7' : C.muted) + head.map((s, i) => line(s, 80, 330 + i * size * 1.08, size, fg, display)).join('') + block(p.visual_subhead, 80, Math.max(585, lastY + 79), 31, 1000, dark ? '#c4bec7' : '#625d67', body, 1.3) + motif(p, dark) + `<path d="M80 1070H1120" stroke="${dark ? C.darkLine : C.line}"/>` + line(p.id === 'P07' ? 'TRY THE COMMAND-LINE ALPHA' : 'FOLLOW THE EARLY ALPHA', 80, 1122, 18, dark ? '#c4bec7' : C.muted) + line('backdown.io', 903, 1127, 28, fg, display);
    exportSvg('graphics/' + p.id, svg(1200, 1200, bg, art, p.alt_text));
  }
  fs.copyFileSync(path.join(repo, 'marketing/public/icon-512.png'), path.join(root, 'profile/avatar-512.png'));
  const header = (w, h) => svg(w, h, C.ivory, brand(280, 97, C.ink, 38) + block('Know what is on your drives.', 280, 207, 69, w - 380, C.ink, display) + line('Local inventory. Matching copies. Evidence you can inspect.', 282, 313, 27, '#625d67') + symbol(58, 62, 139), 'BackDown: know what is on your drives.');
  exportSvg('profile/linkedin-banner-1584x396', header(1584, 396));
  exportSvg('profile/bluesky-header-1500x500', header(1500, 500));
}
const carousel = [
  ['Before you clear that drive.', 'Four questions to ask about your archive.', 'A matching file is a useful finding. A decision about a whole drive needs a fuller picture. Use this checklist alongside your backup plan.'],
  ['What has actually been scanned?', '01 / Establish the scope', 'Check which drives and folders are included. Look for failed, skipped, unreadable or unscanned files. A partial inventory cannot account for the files outside it.'],
  ['Do the contents match?', '02 / Compare the data', 'Filenames and sizes are clues. Content checks give stronger evidence about the files compared. Review what the comparison covers and what is still unknown.'],
  ['Where does the other copy live?', '03 / Understand the copies', 'A repeated folder on the same disk has a different role from a copy on a separate drive. Identify which copies your backup plan needs before removing any.'],
  ['Can you open and recover the files?', '04 / Test what matters', 'Try the files you would most need to recover. A hash match is evidence about matching contents; it does not guarantee future recoverability. BackDown’s public alpha helps you inventory and compare.']
];
async function makeCarousel() {
  for (let i = 0; i < carousel.length; i++) {
    const [title, sub, explanation] = carousel[i];
    let art = brand(88, 130, C.ink, 40) + line('AN ARCHIVE CHECKLIST', 88, 252, 22, C.orange, display) + block(title, 88, 383, 107, 1000, C.ink, display, 1.08) + line(sub, 88, 800, 38, C.orange, display) + block(explanation, 88, 905, 39, 980, '#625d67', body, 1.35) + `<path d="M88 1300H1112" stroke="${C.line}"/>` + line('backdown.io', 88, 1370, 35, C.ink, display) + line(`${i + 1} / 5`, 1020, 1370, 30, C.muted);
    exportSvg('carousel/slide-' + (i + 1), svg(1200, 1500, C.ivory, art, title + ' ' + sub + '. ' + explanation));
  }
  await new Promise((resolve, reject) => { const doc = new PDFDocument({ autoFirstPage: false, info: { Title: 'Before you clear that drive — BackDown', Author: 'BackDown', Subject: 'Four questions for an understandable archive' } }); const out = fs.createWriteStream(path.join(root, 'carousel/Before-you-clear-that-drive.pdf')); doc.pipe(out); out.on('finish', resolve); out.on('error', reject); for (let i = 1; i <= 5; i++) { doc.addPage({ size: [600, 750], margin: 0 }); doc.image(path.join(root, 'carousel/slide-' + i + '.png'), 0, 0, { width: 600, height: 750 }); } doc.end(); });
  fs.writeFileSync(path.join(root, 'carousel/accessible-text.md'), '# Before you clear that drive\n\n' + carousel.map(([a,b,c],i) => `## Slide ${i+1}: ${a}\n\n${b}\n\n${c}`).join('\n\n') + '\n\nBackDown public alpha: https://backdown.io/download/\n');
}
function postFor(item) { return content.posts.find(p => p.id === (item.anchor_id || item.content_source?.post_id)); }
function copyFor(item) {
  const p = postFor(item);
  if (item.channel === 'hacker_news') return content.show_hn_title + '\n\nSubmission URL: ' + item.tracking_url + '\n\n' + content.first_comment;
  if (item.channel === 'linkedin') return p.linkedin_copy;
  if (item.channel === 'mastodon') return item.adapted_copy || item.copy || p.short_copy.replace(p.short_cta_url, item.tracking_url);
  return p.short_copy;
}
function rows() { return schedule.placements.map(p => { const post = postFor(p); return { date: p.proposed_time.local.slice(0, 10), time: p.proposed_time.local.slice(11,16), zone: p.proposed_time.zone_abbreviation, utc: p.proposed_time.utc, channel: p.channel, id: p.id, title: p.channel === 'hacker_news' ? 'Show HN launch' : post?.title || '', status: p.optional && p.conditional ? 'Optional / conditional draft' : p.conditional ? 'Conditional draft' : p.optional ? 'Optional draft' : 'Draft', url: p.tracking_url, asset: p.asset?.filename || '', copy: copyFor(p), alt: p.channel === 'hacker_news' ? '' : p.channel === 'linkedin' && p.anchor_id === 'P04' ? 'Five-page document: Before you clear that drive. Full text supplied in carousel/accessible-text.md.' : post?.alt_text || '', conditions: (p.conditions || []).join('\n') }; }); }
function csv(values) { return values.map(v => '"' + String(v ?? '').replaceAll('"','""') + '"').join(','); }
async function makeCalendar() {
  const data = rows(), headings = ['Date', 'Time London', 'Zone', 'UTC', 'Channel', 'Placement ID', 'Topic', 'Status', 'Link', 'Asset', 'Ready copy', 'Alt text', 'Publication conditions'];
  fs.writeFileSync(path.join(root, 'posting-calendar.csv'), '\ufeff' + [csv(headings), ...data.map(r => csv(Object.values(r)))].join('\r\n') + '\r\n');
  const wb = new ExcelJS.Workbook(); wb.creator = 'BackDown';
  const sheet = wb.addWorksheet('Posting calendar', { views: [{ state: 'frozen', ySplit: 1, xSplit: 1 }] });
  sheet.addRow(headings); data.forEach(r => sheet.addRow(Object.values(r)));
  sheet.columns.forEach((c, i) => c.width = [14,14,8,24,14,24,42,29,50,48,100,60,80][i]);
  sheet.autoFilter = { from: 'A1', to: 'M' + (data.length + 1) };
  const daily = wb.addWorksheet('Daily actions'); daily.addRow(['Day', 'Date', 'Day of week', 'Work', 'Planned actions']);
  schedule.daily_work.forEach(d => daily.addRow([d.day_number, d.date, d.weekday, d.classification, d.work.map(w => w.instruction || w.task || JSON.stringify(w)).join('\n\n')]));
  [9,15,16,20,110].forEach((n,i) => daily.getColumn(i+1).width = n);
  const metrics = wb.addWorksheet('Weekly review'); metrics.addRow(['Week ending','Channel','Impressions / reach','Outbound clicks','Useful conversations','Overall signup change','Release asset download change','What to repeat / change']);
  for (const date of ['2026-10-05','2026-10-12','2026-10-19','2026-10-26','2026-10-28']) metrics.addRow([date,'',null,null,null,null,null,'']);
  [17,18,23,22,24,25,32,90].forEach((n,i)=>metrics.getColumn(i+1).width=n);
  const notes = wb.addWorksheet('Read first'); notes.addRow(['Item','Guidance']); notes.getColumn(1).width=28;notes.getColumn(2).width=115;
  [['Status','All rows are drafts. Nothing has been posted or scheduled.'],['Timezone','All local times use Europe/London. Clocks return to GMT on 25 October; UTC values are already adjusted.'],['Primary scope','9 LinkedIn founder posts + 12 Bluesky posts. Mastodon is optional; Show HN has a readiness gate.'],['Measurement','Site analytics are off and waitlist records do not store UTMs. Use native platform clicks and aggregate waitlist changes; do not report per-channel signup conversion.'],['Blank review cells','Record observed values; blanks mean not yet measured, not zero. Asset download counts are not unique users.'],['Consent','The signup form promises one email when BackDown is ready. Do not send the campaign to the waitlist.'],['Links','UTM links are prepared for consistency; they do not enable attribution on their own.'],['Post review','Check current release status, copy, alt text and final asset before publishing.']].forEach(r=>notes.addRow(r));
  for (const s of wb.worksheets) { s.getRow(1).font={bold:true,color:{argb:'FFFFFFFF'}};s.getRow(1).fill={type:'pattern',pattern:'solid',fgColor:{argb:'FFD8432B'}};s.getRow(1).height=28;s.eachRow((r,n)=>{r.alignment={vertical:'top',wrapText:true};if(n>1){r.height=s===sheet?110:s===daily?90:52;if(n%2===0)r.fill={type:'pattern',pattern:'solid',fgColor:{argb:'FFF5F3EC'}};}}); }
  await wb.xlsx.writeFile(path.join(root, 'posting-calendar.xlsx'));
  const e = s => String(s).replaceAll('\\','\\\\').replaceAll('\n','\\n').replaceAll(';','\\;').replaceAll(',','\\,');
  const stamp = d => d.toISOString().replace(/[-:]/g,'').replace(/\.\d{3}Z/,'Z');
  const fold = str => { let result='', current=''; for(const ch of str) { if(Buffer.byteLength(current+ch)>73) {result+=current+'\r\n ';current=ch;} else current+=ch; } return result+current; };
  const events = schedule.placements.map((p,i)=> {const r=data[i], start=new Date(p.proposed_time.utc), end=new Date(+start+(p.channel==='hacker_news'?120:20)*60000);return ['BEGIN:VEVENT',`UID:${p.id}@backdown.io`,`DTSTAMP:20260928T120000Z`,`DTSTART:${stamp(start)}`,`DTEND:${stamp(end)}`,`SUMMARY:${e((p.optional&&p.conditional?'[Optional / conditional] ':p.conditional?'[Conditional] ':p.optional?'[Optional] ':'')+r.channel+': '+r.title)}`,`DESCRIPTION:${e('DRAFT — importing this event does not schedule a social post.\nAsset: '+r.asset+'\n\n'+r.copy+'\n\nPublication conditions (not part of the post):\n'+(r.conditions||'Review final copy and current release status.'))}`,'STATUS:TENTATIVE','TRANSP:TRANSPARENT','END:VEVENT'].map(fold).join('\r\n');});
  fs.writeFileSync(path.join(root, 'posting-calendar.ics'), ['BEGIN:VCALENDAR','VERSION:2.0','PRODID:-//BackDown//Social campaign drafts//EN','CALSCALE:GREGORIAN','X-WR-CALNAME:BackDown campaign drafts',...events,'END:VCALENDAR',''].join('\r\n'));
  fs.writeFileSync(path.join(root, 'placement-copy.md'), '# BackDown — ready copy by placement\n\nAll drafts. Times: Europe/London.\n\n'+data.map(r=>`## ${r.date} ${r.time} ${r.zone} — ${r.channel} — ${r.title}\n\n${r.status} · Asset: ${r.asset || 'Text only'}\n\n${r.copy}\n\nAlt text: ${r.alt || 'Not applicable.'}\n\nPublication conditions (not part of the post):\n${r.conditions || 'Review final copy and current release status.'}`).join('\n\n---\n\n')+'\n');
}
async function makeDocument() {
  const {Document,Packer,Paragraph,TextRun,HeadingLevel,PageBreak,ImageRun,Footer,PageNumber} = docx;
  const paragraphs=[];
  const h=(text,level=1)=>paragraphs.push(new Paragraph({text,heading:level===1?HeadingLevel.HEADING_1:HeadingLevel.HEADING_2,spacing:{before:260,after:120},keepNext:true}));
  const p=text=>paragraphs.push(new Paragraph({children:[new TextRun({text:String(text),font:'DejaVu Sans',size:21})],spacing:{after:150},keepNext:false}));
  const bullet=text=>paragraphs.push(new Paragraph({children:[new TextRun({text,font:'DejaVu Sans',size:21})],bullet:{level:0},spacing:{after:100}}));
  paragraphs.push(new Paragraph({text:'backdown',heading:HeadingLevel.TITLE}));h('The other copy');p('A complete 30-day organic social campaign');p('29 September–28 October 2026 · Europe/London');p('Prepared 28 September 2026. Ready copy and assets; all posts remain drafts.');
  paragraphs.push(new Paragraph({children:[new ImageRun({data:fs.readFileSync(path.join(root,'graphics/P01.png')),transformation:{width:390,height:390},type:'png'})]}));
  paragraphs.push(new Paragraph({children:[new PageBreak()]}));
  h('The campaign');
  p('Use the new paired bd identity to introduce a practical question: what is on your drives, and where is the other copy? Lead with the archive problem, demonstrate the public alpha, and invite early access. The logo is the opening, then useful product education carries the month.');
  p('Primary outcome: early-access signups and relevant product awareness. Secondary outcome: public CLI downloads and usable feedback. Assumptions: English-language content, organic distribution, a founder-operated account, no ad spend, and roughly 3–4 hours per week for posting, replies and review. Account handles and current audience sizes have not been supplied.');
  h('Who to reach');
  bullet('Primary: technically comfortable people with several external drives, old computers, NAS storage or years of project archives. They can try a command-line alpha.');
  bullet('Secondary: freelance creators and small technical teams who recognise archive uncertainty, but may prefer to join the list until the desktop experience matures.');
  p('Positioning: local drive inventory and content comparison with results you can inspect. Tone: precise, practical, warm. Avoid fear, deletion promises, artificial urgency and invented success stories.');
  h('Where and when');
  bullet('LinkedIn founder profile: 9 posts, Tuesdays and Thursdays at 09:30 London. Use the longer copy and square graphic; P04 uses the five-page PDF. Do not duplicate every post on a company Page on the same day.');
  bullet('Bluesky: 12 posts at 16:00 London on the anchor dates in the workbook. One image, one clear idea, one link, with alt text. Reply to genuine questions for 15 minutes after the posting window.');
  bullet('Mastodon: 4 optional posts at 16:30 on 1, 8, 15 and 22 October, only if there is an established account on a suitable server. Use the adapted placement copy and respect server rules.');
  bullet('Show HN: one conditional post, 6 October at 16:30 London. Link to the runnable CLI download; be available for two hours. Publish only after verifying a clean install and a useful small demo. Do not ask for votes.');
  bullet('GitHub: use the existing public releases as download evidence. Publish release notes only for an actual release. A Discussions feedback destination is optional and depends on that feature being enabled.');
  p('Reddit promotion is excluded. The relevant communities have approval, readiness and/or AI-content restrictions. Instagram, TikTok, YouTube and X are outside this first campaign: maintain two useful primary channels before adding video production or more accounts.');
  p('These posting times are a test schedule, not a claim about universal best times. Keep them stable for two weeks, then compare available engagement and outbound clicks. The calendar correctly changes from BST to GMT on 25 October.');
  h('The four-week story');
  bullet('Week 1: meet the new mark; recognise the drawer of old drives; distinguish a duplicate from a backup you still need.');
  bullet('Week 2: explain content hashes; show the local privacy model; explain inventory context. Run Show HN only if ready.');
  bullet('Week 3: invite a small first alpha trial; tell the founding archive story; separate what ships today from roadmap work.');
  bullet('Week 4: ask for useful feedback; compare names with contents; recap and invite early access.');
  h('Launch preparation');
  for (const s of schedule.setup_checklist) bullet(s.task);
  p('Use profile/avatar-512.png, the LinkedIn banner, and the Bluesky header. Confirm each crop in the platform preview. Use the supplied profile bio and pin the introductory post where supported. The direct CLI download remains available without a signup.');
  h('A realistic publishing routine');
  p('Before each post: check release status, the final link and the image or PDF, then include the supplied accessible description. After posting: record its permalink and platform metrics after about 24 hours; answer useful questions as yourself. Keep a short list of repeated questions and reproducible feedback. Sundays are rest days, and Mondays are review days. If an operational problem appears, pause upcoming promotion and correct the relevant product information.');
  p('Do not send this campaign to the existing waitlist. Its consent is for one readiness email. Do not use automated replies, unsolicited DMs, engagement pods, bought engagement or vote requests.');
  h('Measurement and decisions');
  p('Record baseline follower counts, available platform metrics, aggregate waitlist size and release-asset download counts on 28 September. Record weekly snapshots using the workbook. Unknown values stay blank. Downloads count asset requests, not unique people.');
  p('The live site has analytics disabled, and waitlist records store time and language rather than UTMs. The prepared UTM links do not make signup attribution available. Compare native outbound clicks by channel and aggregate signup changes separately; do not claim channel-specific conversions or a causal lift from those totals. No new tracking has been enabled.');
  p('Operational targets: complete the 21 primary placements, respond to substantive questions within one working day, and document the most useful five feedback conversations if that many occur. These are proposed working targets, not forecasts. Establish audience or conversion goals after the first week has a baseline.');
  p('At the first two weekly reviews, note which topic produces substantive questions and outbound clicks. Repeat the useful subject in a fresh example; reduce repetitive announcements. If the audience repeatedly expects an automatic cleanup product, clarify the alpha scope. If clicks are present but aggregate signups stay flat, inspect the landing-page expectation and CTA; do not assume a channel has failed without attribution.');
  h('Claims and proof');
  bullet('Public release baseline: v0.2.0-alpha.4. Index, hash, dupes, check, local browsing and supported read-only viewers. Linux x86-64/ARM64, universal macOS, Windows x86-64. Re-check later in the month.');
  bullet('Native desktop polish and broader archive planning/copying/cleanup workflows are roadmap work. Do not present them as shipped in the downloadable alpha.');
  bullet('27 million files, 15 TB read and hashed, and 5.9 TB of duplication describe the founding archive. They are not performance benchmarks, expected savings or customer outcomes.');
  bullet('A matching hash is evidence about the files compared, not a guarantee of future recoverability or permission to clear an entire drive.');
  bullet('Screenshots showing sample data must say demo data. The supplied campaign cards are explanatory graphics, not simulated product screens.');
  h('Profile bio');p(content.profile_bio);h('Pinned introductory post');content.pinned_post.split('\n\n').forEach(p);
  paragraphs.push(new Paragraph({children:[new PageBreak()]}));h('Ready-to-publish core posts');
  for (const post of content.posts) { h(post.id+' · '+post.title,2);p(post.date+' · '+post.pillar+' · graphics/'+post.id+'.png');p('LinkedIn version');post.linkedin_copy.split('\n\n').forEach(p);p('Bluesky version');p(post.short_copy);p('Alt text: '+post.alt_text); }
  h('Show HN submission');p('Conditional on readiness. Identify your affiliation, link directly to the usable preview, and answer candidly.');p(content.show_hn_title);content.first_comment.split('\n\n').forEach(p);
  h('Replies to common questions');for(const r of content.response_templates){h(r.question,2);p(r.reply);}
  h('Sources and platform guidance');p('Research checked 28 September 2026. Full notes and direct sources are in research.md. Re-check community rules before an optional community submission.');
  const research=fs.readFileSync(path.join(root,'research.md'),'utf8');const urls=[...new Set(research.match(/https?:\/\/[^\s)\]>]+/g)||[])];urls.forEach(p);
  const document=new Document({creator:'BackDown',title:'BackDown — The other copy: 30-day social campaign',styles:{default:{document:{run:{font:'DejaVu Sans',size:21,color:'17161B'},paragraph:{spacing:{line:285}}},title:{run:{font:'DejaVu Sans',bold:true,size:54,color:'17161B'}},heading1:{run:{font:'DejaVu Sans',bold:true,size:30,color:'D8432B'},paragraph:{keepNext:true}},heading2:{run:{font:'DejaVu Sans',bold:true,size:24,color:'17161B'},paragraph:{keepNext:true}}}},sections:[{properties:{page:{margin:{top:1000,right:1000,bottom:1000,left:1000}}},footers:{default:new Footer({children:[new Paragraph({children:[new TextRun({text:'BackDown · Campaign drafts  |  ',font:'DejaVu Sans',size:17,color:'77717B'}),new TextRun({children:[PageNumber.CURRENT],font:'DejaVu Sans',size:17,color:'77717B'})]})]})},children:paragraphs}]});
  fs.writeFileSync(path.join(root,'BackDown-social-campaign.docx'),await Packer.toBuffer(document));
}
(async()=>{makeCards();await makeCarousel();await makeCalendar();await makeDocument();console.log('Built 12 social cards, 5-page carousel, profile assets, calendar XLSX/CSV/ICS and campaign DOCX.');})().catch(e=>{console.error(e);process.exit(1);});
