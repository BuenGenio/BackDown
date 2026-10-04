# Going live: payments, licences and tiers

Decided on 4 Oct 2026:
- the store sells **Pro, Team and Forensic**, each as a pre-order fulfilled with an offline licence key;
- Pro, Team and Forensic features start **needing a licence in the release that opens the store**, never before, so anyone who loses a feature can buy it the same day.

## What's ready

- **The store:**
  - `marketing/src/store.js` sells three editions (PR #38): Pro machines 1–50, Team machines 5–500, Forensic sites 1–10;
  - the pricing page shows a pre-order button for each edition while its Stripe price is active;
  - the webhook issues a licence for the edition paid for;
  - tested against a fake Stripe (`node --test marketing/tools/store.test.mjs`, `node --test scripts/stripe-setup.test.mjs`).
- **The licence-signing key:** `~/.config/backdown/license-signing-key.pem` exists, is readable by its owner only, and matches the public key built into the app. A test licence minted from it verified on 4 Oct 2026.
- **Manual licences:** `scripts/mint-licence.mjs` mints any edition by hand (`--product pro|team|forensic --seats N`).
- **Signed releases:** these are in place since v1.0.0-beta.4, and `backdown update` refuses unsigned ones from then on.
- **The tiers table:** `crates/backdown-cli/src/tiers.rs` decides what each edition unlocks. Pre-release versions unlock everything while `PREVIEW_UNLOCKS_ALL` is true. `BACKDOWN_PRETEND_RELEASE=1` shows the locked experience.

## 1. The owner's part

Secrets stay in your own terminal and never go into a chat.

1. **A Stripe account**, with Stripe Tax turned on if you're collecting tax; the setup script checks.
2. **Test mode first.** From `marketing/`, run:
   ```sh
   STRIPE_SECRET_KEY=sk_test_… node ../scripts/stripe-setup.mjs --dry-run --price pro=CENTS --price team=CENTS --price forensic=CENTS
   STRIPE_SECRET_KEY=sk_test_… node ../scripts/stripe-setup.mjs --price pro=CENTS --price team=CENTS --price forensic=CENTS
   ```
   - The amounts are US cents, tax exclusive, per machine (Pro and Team) or per site (Forensic). Give only the editions you want to sell.
   - `--worker-key rk_test_…` gives the site a restricted key (Checkout Sessions write and Prices read are enough).
   - The script installs the site's secrets: the Stripe key, the webhook secret, and the licence-signing key from the PEM file.
3. **The seller and the terms.** In `marketing/site.json`:
   - set `preorder.enabled: true`;
   - fill in every `seller` field (name, address, email, governing law) and `contact_email`.

   Have the terms (`marketing/templates/terms.html`) reviewed, including two decisions:
   - a Forensic seat is defined as "one site: one location of your organisation, however many machines there run BackDown";
   - one `preorder.ship_by` date (2027-06-30) covers all three editions.
4. **Email.** backdown.io has no MX record, so it can't receive mail yet. Decide whether to set up Cloudflare Email Routing for the seller and contact addresses. To have licence keys emailed, add the Worker's `EMAIL` binding (Cloudflare Email Service). Without it, the thanks page shows the key and Stripe's receipt is the only email.

## 2. Then, with the lead

5. **Test purchases.** Run `python3 build.py && npx wrangler deploy`, then:
   - buy each edition with card 4242 4242 4242 4242;
   - check each key with `backdown license add KEY` and `backdown license show` (Forensic says sites);
   - refund one in the Stripe dashboard and check its licence is revoked.
6. **Live keys.** Run the same command as step 2 with `sk_live_…`, and `--worker-key rk_live_…`.
7. **The release that switches tiers on**, the same day:
   - First, mint licences for the owner's own machines, so nothing locks for them. A Forensic licence covers every edition. For example, on this machine and on nextcloud:
     `node scripts/mint-licence.mjs --key ~/.config/backdown/license-signing-key.pem --product forensic --seats 2 --email … | backdown license add -`
   - Set `PREVIEW_UNLOCKS_ALL = false` in `crates/backdown-cli/src/tiers.rs`. Check the tests that expect pre-releases to unlock everything.
   - Change everything that says paid features are free during the beta. As of 4 Oct 2026 that's:
     - `crates/backdown-cli/src/tiers.rs`, `crates/backdown-cli/src/main.rs`, `crates/backdown-cli/ui/settings.js` (the "Pro features are free during the preview" callout) and `crates/backdown-cli/README-release.txt`;
     - the docs guides: settings-and-licences, air-gapped, browse-without-restoring, chain-of-custody, drive-health, file-into-an-archive, free-up-a-drive, manifests-and-attestations, several-machines;
     - `marketing/docs/principles.md` and `marketing/status.json`;
     - the release notes' known limitations.

     Find them with `grep -rli "free during\|everything is unlocked" crates marketing/docs marketing/status.json`.
   - Build, sign and publish the release as usual (`build-local.sh`, `package-local.sh`, `publish-release.sh --local --with-packages`). Deploy the site, add the changelog entry, and install it on both machines.
8. **Closing an edition later:** archive its price in Stripe. The site notices within an hour.
