import 'dart:convert';

/// The service names folders and files by key: the path's bytes in unpadded base64url, so any
/// path (whatever its encoding) goes through a URL intact. `/` is `Lw`.
String keyOfPath(String path) => base64Url.encode(utf8.encode(path)).replaceAll('=', '');

/// The path a key stands for, for showing (bytes that aren't UTF-8 show as U+FFFD).
String pathOfKey(String key) {
  try {
    return utf8.decode(base64Url.decode(base64Url.normalize(key)), allowMalformed: true);
  } on FormatException {
    return key;
  }
}

/// The last part of a path: `/media/sam/Seagate` → `Seagate`, `/` → `/`.
String baseName(String path) {
  final trimmed = path.length > 1 && (path.endsWith('/') || path.endsWith(r'\'))
      ? path.substring(0, path.length - 1)
      : path;
  final i = trimmed.lastIndexOf(RegExp(r'[/\\]'));
  return i < 0 || i == trimmed.length - 1 ? trimmed : trimmed.substring(i + 1);
}
