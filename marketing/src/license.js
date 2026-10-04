// Licence keys: BD1.<base64url payload JSON>.<base64url Ed25519 signature over "BD1.<payload>">.
// The app verifies them offline against the public key built into it (crates/backdown-cli/src/license.rs), so the
// private key (secret LICENSE_SIGNING_KEY, PKCS#8 PEM) is the only thing that can mint one. Signing is deterministic
// (Ed25519) and the licence id is derived from the order, so issuing the same order twice yields the same key.
// The payload's `product` is the edition bought; the app maps it to a tier (crates/backdown-cli/src/tiers.rs).

const PRODUCTS = ['pro', 'team', 'forensic'];

const b64url = bytes => btoa(String.fromCharCode(...new Uint8Array(bytes))).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
const CROCKFORD = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';

let signingKey; // imported once per isolate
async function key(pem) {
  if (!signingKey) {
    const der = Uint8Array.from(atob(pem.replace(/-----[^-]+-----|\s+/g, '')), c => c.charCodeAt(0));
    signingKey = await crypto.subtle.importKey('pkcs8', der, { name: 'Ed25519' }, false, ['sign']);
  }
  return signingKey;
}

// 20 Crockford base32 characters from SHA-256(order id): stable per order, unguessable, easy to read out
export async function licenceId(orderId) {
  const h = new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(`backdown-licence:${orderId}`)));
  let bits = 0, acc = 0, out = '';
  for (const b of h) {
    acc = (acc << 8) | b; bits += 8;
    while (bits >= 5 && out.length < 20) { out += CROCKFORD[(acc >> (bits - 5)) & 31]; bits -= 5; }
    if (out.length === 20) break;
  }
  return out.match(/.{5}/g).join('-');
}

// product: the edition (pro, team or forensic), required so a caller can't mint Pro by leaving it out
export async function issue(env, { orderId, product, seats, email, name, issued }) {
  if (!env.LICENSE_SIGNING_KEY) throw new Error('LICENSE_SIGNING_KEY is not set');
  if (!PRODUCTS.includes(product)) throw new Error(`no such product: ${product}`);
  // (the claims keep this order: an order issued again must sign the very same bytes)
  const payload = { v: 1, id: await licenceId(orderId), product, kind: 'preorder', seats, email, name: name || undefined, issued };
  const body = `BD1.${b64url(new TextEncoder().encode(JSON.stringify(payload)))}`;
  const sig = await crypto.subtle.sign({ name: 'Ed25519' }, await key(env.LICENSE_SIGNING_KEY), new TextEncoder().encode(body));
  return { id: payload.id, key: `${body}.${b64url(sig)}` };
}
