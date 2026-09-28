// File-type icons: a Nautilus-style folder and a page with a coloured band + glyph per kind.

const EXT = {
  text: 'txt md markdown rst log nfo readme',
  code: 'js mjs cjs ts tsx jsx py rs go c h cc cpp hpp java kt swift rb php pl lua r sh bash zsh fish ps1 bat cmd css scss less html htm xml json yaml yml toml ini cfg conf env vue svelte dart scala cs vb m mm asm makefile dockerfile gradle',
  data: 'csv tsv',
  sql: 'sql',
  db: 'sqlite sqlite3 db db3 s3db',
  image: 'png jpg jpeg gif webp avif bmp ico svg tif tiff heic heif raw cr2 nef arw dng psd',
  pdf: 'pdf',
  doc: 'doc docx odt rtf pages',
  sheet: 'xls xlsx xlsm xlsb xltx xltm ods numbers',
  slides: 'ppt pptx pptm ppsx odp key',
  archive: 'zip jar apk epub war cbz whl tar gz tgz bz2 tbz tbz2 xz txz zst tzst 7z rar lz4',
  audio: 'mp3 m4a aac flac wav ogg oga opus wma aiff',
  video: 'mp4 m4v mov mkv webm avi wmv mpg mpeg 3gp',
  disk: 'img iso dd dmg vmdk vdi qcow qcow2 vhd vhdx',
  mail: 'eml mbox mbx msg',
  diff: 'diff patch',
};
const BY_EXT = {};
for (const [kind, list] of Object.entries(EXT)) for (const e of list.split(' ')) BY_EXT[e] = kind;

export function kindOf(name) {
  const lower = name.toLowerCase();
  const ext = lower.includes('.') ? lower.slice(lower.lastIndexOf('.') + 1) : lower;
  return BY_EXT[ext] || 'file';
}

const COLOR = {
  text: '#8a8f98', code: '#5b6b8c', data: '#2e8b57', sql: '#b8800e', db: '#1f8a8a', image: '#8e5bd1',
  pdf: '#d8432b', doc: '#3b6fd8', sheet: '#2e8b57', slides: '#d97a1b', archive: '#8a6a3e', audio: '#c2458a',
  video: '#7a4fd6', disk: '#55606e', mail: '#3b8fd8', diff: '#2e8b57', file: '#8a8f98',
};

const GLYPH = {
  text: '<path d="M9 13h6M9 16h6M9 19h4"/>',
  code: '<path d="m10 13-2 3 2 3M14 13l2 3-2 3"/>',
  data: '<path d="M8 13h8v7H8zM8 16.5h8M12 13v7"/>',
  sql: '<ellipse cx="12" cy="13.5" rx="4" ry="1.6"/><path d="M8 13.5v5c0 .9 1.8 1.6 4 1.6s4-.7 4-1.6v-5"/>',
  db: '<ellipse cx="12" cy="13.5" rx="4" ry="1.6"/><path d="M8 13.5v5c0 .9 1.8 1.6 4 1.6s4-.7 4-1.6v-5M8 16c0 .9 1.8 1.6 4 1.6s4-.7 4-1.6"/>',
  image: '<circle cx="10" cy="14" r="1.3"/><path d="m8 20 3-3 2 2 3-3 1 1"/>',
  pdf: '<path d="M9 19v-5h1.6a1.4 1.4 0 0 1 0 2.8H9M13.5 14v5M16.5 14v5M16.5 14H18M16.5 16.5h1.2"/>',
  doc: '<path d="M8.5 14l1.2 5 1.3-4 1.3 4 1.2-5"/>',
  sheet: '<path d="M8 13h8v7H8zM8 16.5h8M12 13v7"/>',
  slides: '<rect x="8" y="13" width="8" height="5.5" rx=".6"/><path d="M12 18.5V20"/>',
  archive: '<path d="M12 11v2M12 14v2M12 17v2"/><rect x="11" y="19" width="2" height="1.6" rx=".4"/>',
  audio: '<path d="M11 19.5v-6l4-1v6"/><circle cx="10" cy="19.5" r="1"/><circle cx="14" cy="18.5" r="1"/>',
  video: '<path d="m10.5 13.5 4.5 3-4.5 3z"/>',
  disk: '<circle cx="12" cy="16.5" r="3.2"/><circle cx="12" cy="16.5" r=".8"/>',
  mail: '<rect x="8" y="13" width="8" height="6" rx=".6"/><path d="m8 13.5 4 3 4-3"/>',
  diff: '<path d="M12 12.5v4M10 14.5h4M10 19h4"/>',
  file: '',
};

export function fileIcon(name, size = 48) {
  const k = kindOf(name);
  const c = COLOR[k];
  return `<svg class="ic" width="${size}" height="${size}" viewBox="0 0 24 24" aria-hidden="true">
    <path d="M6.5 2.5h7.8l4.2 4.2v14.1a.7.7 0 0 1-.7.7H6.5a.7.7 0 0 1-.7-.7V3.2a.7.7 0 0 1 .7-.7z" fill="var(--page)" stroke="var(--page-line)" stroke-width=".6"/>
    <path d="M14.3 2.5v3.6a.6.6 0 0 0 .6.6h3.6" fill="var(--page-fold)" stroke="var(--page-line)" stroke-width=".6"/>
    <rect x="5.8" y="8.2" width="12.7" height="1.6" fill="${c}" opacity=".9"/>
    <g fill="none" stroke="${c}" stroke-width="1.1" stroke-linecap="round" stroke-linejoin="round">${GLYPH[k]}</g>
  </svg>`;
}

export function folderIcon(size = 48, special = '') {
  const badge = special === 'git'
    ? '<g transform="translate(13.5 12.5)"><circle r="3.4" fill="#f05033"/><path d="M-1.2-1.3v2.6M-1.2-.5c0 .9 2.4.6 2.4 1.8" fill="none" stroke="#fff" stroke-width=".8" stroke-linecap="round"/></g>'
    : '';
  return `<svg class="ic" width="${size}" height="${size}" viewBox="0 0 24 24" aria-hidden="true">
    <path d="M2.5 6.2A1.7 1.7 0 0 1 4.2 4.5h4.6l2 2h9a1.7 1.7 0 0 1 1.7 1.7v1H2.5z" fill="var(--folder-back)"/>
    <path d="M2.5 8.7A1.2 1.2 0 0 1 3.7 7.5h16.6a1.2 1.2 0 0 1 1.2 1.2v9.6a1.7 1.7 0 0 1-1.7 1.7H4.2a1.7 1.7 0 0 1-1.7-1.7z" fill="var(--folder)"/>
    <path d="M2.5 9.3h19" stroke="var(--folder-edge)" stroke-width=".5"/>
    ${badge}
  </svg>`;
}

export function driveIcon() {
  return `<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="3" y="12" width="18" height="7" rx="2"/><path d="M5.5 12 8 5h8l2.5 7M7 15.5h.01M10.5 15.5h.01"/></svg>`;
}
