# BackDown — The other copy

A complete organic social campaign for **29 September–28 October 2026**, using the new paired **bd** identity. The primary goal is early-access signups; the secondary goal is useful alpha trials and feedback. All posts are drafts: nothing has been posted or scheduled.

## Start here

1. Read **BackDown-social-campaign.docx** for the campaign strategy, channels, posting routine, complete core copy and reply templates.
2. Open **posting-calendar.xlsx** for the exact dates, London times, channel-specific copy, asset names and links. Its Daily actions sheet covers all 30 days.
3. Review the asset attached to each placement, then publish manually. P04 on LinkedIn uses the five-page PDF; P04 on Bluesky uses the square image.

The first posts are **Tuesday 29 September: LinkedIn at 09:30 BST and Bluesky at 16:00 BST**. The calendar already handles the return to GMT on 25 October.

## What is included

- **21 primary placements:** 9 LinkedIn founder-profile posts and 12 Bluesky posts.
- **5 additional conditional placements:** 4 Mastodon posts if a suitable active account exists, and 1 Show HN launch if the preview and author availability pass the readiness check.
- **graphics/**: 12 square 1200×1200 PNGs, with editable SVG masters containing outlined type.
- **carousel/**: a five-page PDF, individual PNG/SVG pages, and accessible text for “Before you clear that drive”.
- **profile/**: avatar and correctly sized LinkedIn/Bluesky banners.
- **posting-calendar.csv** and **posting-calendar.ics**: optional import files. Importing the calendar creates tentative planning events; it does not schedule posts on a social network.
- **placement-copy.md**: each scheduled placement's finished copy, link, filename and alt text in plain text.
- **demo-data.zip**: five harmless example files and verified alpha commands. It contains no real archive data or generated index.
- **research.md**: official platform guidance and product-claim evidence.
- **content.json / schedule.json**: editable source data for the campaign.

## Assumptions and limits

English-language, organic campaign; no paid ad budget or account handles were supplied. Prioritize the founder's LinkedIn profile and Bluesky. Mastodon and Show HN have explicit conditions in the calendar. Reddit promotion is excluded because the relevant communities' rules do not support simply posting this campaign copy.

The alpha claims are tied to public version **v0.2.0-alpha.4** as reviewed on 28 September. Check later release changes before posting. The supplied demo was tested with an existing local binary reporting that version; that does not replace checking the public downloadable build before Show HN.

The live site currently has analytics disabled and its waitlist does not record UTM attribution. Compare native platform clicks with aggregate signup changes; do not claim channel-specific signup conversions. The website signup promises **one email when BackDown is ready**, not an ongoing email campaign.

## Editing or rebuilding

Copy edits belong in content.json and schedule.json. Visuals and document exports are produced by build-kit.cjs using the existing production SVG and bundled site fonts. Its dependencies are @resvg/resvg-js, fontkit, docx, exceljs and pdfkit. The generated graphics do not depend on installed fonts to display correctly.

Do not publish research notes, source JSON or this internal planning document as customer-facing content. Use the finished cards, platform copy and carousel.
