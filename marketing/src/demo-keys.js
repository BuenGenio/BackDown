// The docs' live demo (public/demo/) is BackDown's own UI answered from recorded API responses. One request is
// one key, made the same way when recording (marketing/tools/demo/) and in the Worker (worker.js): the method,
// the path as the app asks for it (/api/...), and the query with its pairs sorted and the volatile ones dropped.
const VOLATILE = new Set(['since', 'wait', '_']); // long-poll bookkeeping and cache-busting

export function demoKey(method, pathname, search) {
  const pairs = [...new URLSearchParams(search || '')].filter(([k]) => !VOLATILE.has(k));
  pairs.sort((a, b) => (a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : a[1] < b[1] ? -1 : a[1] > b[1] ? 1 : 0));
  const q = new URLSearchParams(pairs).toString();
  return `${method.toUpperCase()} ${pathname}${q ? '?' + q : ''}`;
}

/** The recorded response's file name: sha-1 of the key, hex. */
export async function keyFile(key) {
  const d = await crypto.subtle.digest('SHA-1', new TextEncoder().encode(key));
  return [...new Uint8Array(d)].map(b => b.toString(16).padStart(2, '0')).join('');
}
