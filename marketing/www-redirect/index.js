// www.backdown.io → backdown.io (301, path and query kept), so search engines see one host.
// A separate Worker so only www traffic invokes code; the main site stays pure static assets.
export default {
  fetch(request) {
    const url = new URL(request.url);
    url.hostname = 'backdown.io';
    return Response.redirect(url.toString(), 301);
  },
};
