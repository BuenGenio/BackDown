import 'package:clock/clock.dart';
import 'package:intl/intl.dart';

/// Sizes as the web UI writes them (decimal units, like drive makers and the OS's file managers).
String bytes(num? n) {
  if (n == null) return '—';
  final v = n.toDouble();
  if (v >= 1e12) return '${(v / 1e12).toStringAsFixed(2)} TB';
  if (v >= 1e9) return '${(v / 1e9).toStringAsFixed(1)} GB';
  if (v >= 1e6) return '${(v / 1e6).toStringAsFixed(1)} MB';
  if (v >= 1e3) return '${(v / 1e3).round()} KB';
  return '${v.round()} B';
}

final _count = NumberFormat.decimalPattern();

/// 6979126 → 6,979,126 (in the phone's locale).
String count(num? n) => n == null ? '—' : _count.format(n);

/// "1 file", "2 files".
String plural(num n, String one, [String? many]) => '${count(n)} ${n == 1 ? one : (many ?? '${one}s')}';

/// Unix seconds → local date and time, "1 Nov 2026, 14:03".
String when(int? t) {
  if (t == null || t == 0) return '—';
  return DateFormat.yMMMd().add_Hm().format(DateTime.fromMillisecondsSinceEpoch(t * 1000));
}

/// Unix seconds → local date, "1 Nov 2026".
String day(int? t) {
  if (t == null || t == 0) return '—';
  return DateFormat.yMMMd().format(DateTime.fromMillisecondsSinceEpoch(t * 1000));
}

/// The server's clock minus this phone's, in seconds: times the server gives are compared with its
/// own "now" so a phone a few minutes off doesn't say "in 3 minutes" for something just done.
int nowSeconds([int skew = 0]) => clock.now().millisecondsSinceEpoch ~/ 1000 + skew;

/// "3 minutes ago", "in 2 hours", "just now", as the web UI says them.
String rel(int? t, {int skew = 0}) {
  if (t == null || t == 0) return '—';
  final s = t - nowSeconds(skew);
  final a = s.abs();
  String unit(int n, String u) => '$n $u${n == 1 ? '' : 's'}';
  String said(String what) => s < 0 ? '$what ago' : 'in $what';
  if (a >= 86400 * 2) return said(unit((a / 86400).round(), 'day'));
  if (a >= 86400) return s < 0 ? 'yesterday' : 'tomorrow';
  // from where rounding would say "60 minutes", it's an hour
  if (a >= 3570) return said(unit((a / 3600).round(), 'hour'));
  if (a >= 60) return said(unit((a / 60).round(), 'minute'));
  if (a < 10) return s <= 0 ? 'just now' : 'any moment';
  return said(unit(a, 'second'));
}

/// Seconds → "2 h 5 min", "3 d 4 h", "45 s", as the web UI says them.
String dur(num seconds) {
  final s = seconds.round().clamp(0, 1 << 40);
  if (s >= 86400) {
    final d = s ~/ 86400, h = ((s % 86400) / 3600).round();
    return h > 0 ? '$d d $h h' : '$d d';
  }
  if (s >= 3600) {
    final h = s ~/ 3600, m = ((s % 3600) / 60).round();
    return m > 0 ? '$h h $m min' : '$h h';
  }
  if (s >= 60) return '${(s / 60).round()} min';
  return '$s s';
}

/// 0.4231 → "42%".
String percent(num? f) => f == null ? '—' : '${(f * 100).round()}%';

/// A SHA-256 shortened for a list: the first 12 hex digits.
String shortHash(String? h) => h == null || h.length < 12 ? (h ?? '—') : h.substring(0, 12);
