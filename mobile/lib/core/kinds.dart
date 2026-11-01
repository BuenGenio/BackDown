import 'package:flutter/material.dart';

/// What kind of file a name is, by its extension: the same table as the web UI (ui/icons.js), so
/// both show the same icon and colour, and the viewers agree on what opens how.
enum FileKind {
  text('Text', Icons.description_outlined, Color(0xff8a8f98)),
  code('Code', Icons.code, Color(0xff5b6b8c)),
  data('Data', Icons.table_rows_outlined, Color(0xff2e8b57)),
  sql('SQL', Icons.storage_outlined, Color(0xffb8800e)),
  db('Database', Icons.dns_outlined, Color(0xff1f8a8a)),
  image('Image', Icons.image_outlined, Color(0xff8e5bd1)),
  pdf('PDF', Icons.picture_as_pdf_outlined, Color(0xffd8432b)),
  doc('Document', Icons.article_outlined, Color(0xff3b6fd8)),
  sheet('Spreadsheet', Icons.grid_on_outlined, Color(0xff2e8b57)),
  slides('Slides', Icons.slideshow_outlined, Color(0xffd97a1b)),
  archive('Archive', Icons.folder_zip_outlined, Color(0xff8a6a3e)),
  audio('Audio', Icons.audiotrack_outlined, Color(0xffc2458a)),
  video('Video', Icons.movie_outlined, Color(0xff7a4fd6)),
  disk('Disk image', Icons.album_outlined, Color(0xff55606e)),
  mail('Mail', Icons.mail_outline, Color(0xff3b8fd8)),
  diff('Diff', Icons.difference_outlined, Color(0xff2e8b57)),
  file('File', Icons.insert_drive_file_outlined, Color(0xff8a8f98));

  const FileKind(this.label, this.icon, this.color);

  final String label;
  final IconData icon;
  final Color color;

  static const _exts = {
    text: 'txt md markdown rst log nfo readme',
    code:
        'js mjs cjs ts tsx jsx py rs go c h cc cpp hpp java kt swift rb php pl lua r sh bash zsh fish ps1 bat cmd '
        'css scss less html htm xml json yaml yml toml ini cfg conf env vue svelte dart scala cs vb m mm asm '
        'makefile dockerfile gradle',
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

  static final Map<String, FileKind> _byExt = {
    for (final MapEntry(:key, :value) in _exts.entries)
      for (final e in value.split(' ')) e: key,
  };

  /// The kind of a file called [name] (`Makefile` and `README` count by their whole name).
  static FileKind of(String name) => _byExt[extensionOf(name)] ?? file;

  /// The extensions of some kinds (the duplicates filter sends them to the service).
  static List<String> extensionsOf(Iterable<FileKind> kinds) => [for (final k in kinds) ...?_exts[k]?.split(' ')];
}

/// `Photo.JPG` → `jpg`; a name without a dot is its own extension (`Makefile` → `makefile`).
String extensionOf(String name) {
  final lower = name.toLowerCase();
  final dot = lower.lastIndexOf('.');
  return dot < 0 ? lower : lower.substring(dot + 1);
}

/// Images the phone can decode itself (not RAW, TIFF, HEIC, PSD).
bool isDecodableImage(String name) =>
    const {'png', 'jpg', 'jpeg', 'gif', 'webp', 'bmp', 'ico', 'avif'}.contains(extensionOf(name));
