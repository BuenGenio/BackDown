# BackDown social campaign research

Research checked 28 September 2026. Campaign window: 29 September–28 October 2026, Europe/London. This is a publishing plan and draft library; no social posts have been scheduled or published. Account ownership, follower counts, analytics baselines and posting permissions have not been verified.

## Positioning and channel priorities

Campaign: **The other copy.** Working positioning: **Know what's on your drives.** Use the new paired-bd identity as the opening, followed by evidence of the current command-line preview. The assumed primary goal is early-access sign-ups; useful preview downloads and actionable feedback are supporting goals. This choice should be revised if the owner prefers a different conversion goal.

Prioritize the founder's LinkedIn profile, if available, and an owned Bluesky account. Use Mastodon as an optional technical channel if an appropriate account and server exist. GitHub releases provide product evidence; Discussions can provide a feedback home if enabled. Reserve a single Show HN submission for a runnable walkthrough. Do not assume every recommended account exists.

The final proposed calendar has nine LinkedIn posts at 09:30, twelve Bluesky posts at 16:00, four optional Mastodon adaptations at 16:30, and one conditional Show HN at 16:30 on 6 October. Reserve 15 minutes for manual replies each day and 30 minutes for the weekly review. GitHub updates must track actual shipped work. This cadence and all selected posting times are hypotheses to test, not universal best-time claims. Use Europe/London timestamps: the campaign crosses the UK clock change on 25 October.

## Platform facts and source links

### LinkedIn

Use a founder explanation, an annotated screenshot or a short PDF checklist. LinkedIn accepts PDF document posts, and a published document cannot be replaced: review the export first. Documents can be posted from a profile or a Page. The official limits are 100 MB and 300 pages; our short documents are far below these limits.

Source: https://www.linkedin.com/help/linkedin/answer/a518909

### Bluesky

Keep each post, including visible link text, within 300 graphemes. Standard image embeds accept up to four images, with a current 2,000,000-byte limit per image. Export shared assets below 1.8 MB to leave a margin. Supply meaningful alt text and identify founder/commercial affiliation. Do not repeat promotional replies or manipulate engagement. The campaign does not depend on a particular video-duration limit.

Sources:
- https://raw.githubusercontent.com/bluesky-social/atproto/main/lexicons/app/bsky/feed/post.json
- https://raw.githubusercontent.com/bluesky-social/atproto/main/lexicons/app/bsky/embed/images.json
- https://bsky.social/about/support/community-guidelines

### Mastodon

The default post limit is 500 characters, and standard support allows up to four images. Check the chosen server's settings and rules before publishing. Use descriptive text, image descriptions and a small number of relevant hashtags. Links count as 23 characters in standard Mastodon; link shorteners are discouraged.

Source: https://docs.joinmastodon.org/user/posting/

### GitHub

Existing download destination: https://github.com/BuenGenio/backdown-releases/releases

Release notes should accompany a real shipped build and identify alpha builds as pre-releases. Do not create an artificial release to fill a marketing slot. Discussions support questions, announcements and feedback, but must be enabled before the campaign uses them as a destination. Do not claim Discussions currently exists without checking.

Sources:
- https://docs.github.com/en/repositories/releasing-projects-on-github/managing-releases-in-a-repository
- https://docs.github.com/en/discussions/quickstart

### Hacker News / Show HN

A downloadable CLI preview that people can run without registration is a suitable Show HN candidate. Early-stage work is allowed. Link to the working download/quick-start page rather than a logo announcement, waitlist, pre-order page or fundraiser. A disposable demo dataset reduces friction. The person who built it should be present to explain choices and respond. Minor upgrades generally do not justify another Show HN. Never request votes or coordinated comments.

Source: https://news.ycombinator.com/showhn.html

### Reddit

Do not treat relevant communities as unconditional distribution channels. r/DataHoarder requires moderator approval before company/software advertising, disallows “look what I built” posts, and prohibits AI-generated content/vibe-coded projects. Do not paste the campaign's generated copy into that community. Any future participation must follow its rules and the moderator route.

r/selfhosted requires promoted apps to be production-ready with documentation. Projects younger than three months are restricted to its current New Project Megathread. Its Wednesday tool exception does not establish that an alpha satisfies other requirements. Recheck rules and project eligibility before any community post; the prepared calendar does not authorize outreach or publication.

Sources:
- https://www.reddit.com/r/DataHoarder/about/rules.json
- https://www.reddit.com/r/selfhosted/about/rules.json

## Product claim guardrails

Source files reviewed: `marketing/i18n/en.json`, `marketing/downloads.json`, and `marketing/status.json`.

- The listed downloadable release is **v0.2.0-alpha.4**: Linux x86-64/ARM64, universal macOS, and Windows x86-64. Identify it as a command-line preview, including when showing its local browser.
- Shipped changelog entries substantiate indexing, tiered hashing, duplicate detection, hash-backed drive checks, scan-history browsing and read-only viewers. Linux supports the service installation workflow.
- Recognition, reports, planner and journaled copying are marked done in current work, but the status also says the next preview will contain all of that work. Verify the downloadable binary before claiming those functions are available in alpha.4.
- Drive-reclaim/deletion workflow, rescue and Pro pre-orders are marked in progress. Native desktop installers, encrypted cloud targets, fleet and forensic functionality must stay explicitly planned unless release evidence changes.
- The story's 27 million files, 15 TB and 5.9 TB duplicated describe one founder project. They are not customer results, benchmarks, average savings or guarantees.
- Label demo data in product screenshots. Do not invent customer quotes, testimonials, traction or performance comparisons.
- Explain a safe-to-wipe result as a hash-backed check against indexed copies. Do not imply that this is a guarantee against future data loss, and do not turn educational posts into irreversible deletion instructions.
- Do not call the product open source without checking the actual licence. The current site describes the intended core as source-available.
- The existing waitlist consent promises one readiness email. It is not permission to send a month-long email campaign.

## Measurement and iteration

The current production site has analytics disabled. The waitlist worker stores time and language, not UTM parameters, so the prepared tagged links do not provide channel-level signup attribution. Use platform click/engagement statistics and aggregate signup deltas; do not credit all list growth to social. GitHub asset-download counts are downloads, not unique people or completed activations. The app's no-telemetry posture means first-run activation cannot be inferred from downloads. Establish a baseline first, then test one change at a time (hook or publishing time). Record unavailable measurements as unknown rather than zero. Treat numbers as observed only when obtained from an actual source; any targets are planning assumptions, not forecasts.
