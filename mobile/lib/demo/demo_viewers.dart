import 'dart:convert';
import 'dart:typed_data';

import 'package:flutter/services.dart' show rootBundle;

import '../api/api.dart';
import '../api/demo_api.dart';
import '../api/keys.dart';
import 'demo_fs.dart';

/// The demo's answers for the viewers: what's inside the made-up computer's containers (the 2012
/// mbox and the mail server's Maildir, the website backup, the laptop's disk image and the Ubuntu
/// ISO, the contacts database, the tax spreadsheet, the shop's SQL dump), and the bytes of files
/// inside them. Everything here is invented.
final Map<String, DemoHandler> demoViewerGets = {
  'mail-kind': (d, p) => {'kind': _maildirMessage(_node(p)) != null ? 'message' : null},
  'mail': (d, p) => _mailJson(_node(p), int.tryParse(p['at'] ?? '')),
  'threads': (d, p) => _threads(d, p),
  'thread': (d, p) => _thread(d, p),
  'archive': (d, p) => _archive(_node(p), int.tryParse(p['limit'] ?? '')),
  'disk': (d, p) => _disk(_node(p), p['path']),
  'fs': (d, p) => _fsList(_node(p), p['part'] ?? '0', p['path'] ?? '', p['deleted'] == '1'),
  'sqlite': (d, p) => _sqlite(_node(p), p['table'], int.tryParse(p['offset'] ?? '') ?? 0),
  'sheet': (d, p) => _sheet(_node(p), p['sheet'], int.tryParse(p['offset'] ?? '') ?? 0, int.tryParse(p['limit'] ?? '')),
  'sqldump': (d, p) => _sqldump(_node(p), p['more']),
  'slides': (d, p) => throw ApiException(ApiFailure.refused, 'not a presentation BackDown can read', status: 400),
};

final Map<String, DemoHandler> demoViewerPosts = {};

final Map<String, DemoBytesHandler> demoViewerBytes = {
  'entry': (d, p) => _entryBytes(_node(p), p['name'] ?? '', p['part']),
  'attachment': (d, p) => _attachmentBytes(_node(p), int.tryParse(p['at'] ?? ''), int.tryParse(p['part'] ?? '') ?? 0),
};

DemoFs get _fs => DemoFs.instance;

DemoNode _node(Map<String, String> p) {
  final n = _fs.byId[int.tryParse(p['id'] ?? '') ?? 0];
  if (n == null) throw ApiException(ApiFailure.refused, 'no such file', status: 400);
  return n;
}

/// Unix seconds for a date and time, UTC.
int _t(int y, int m, int d, [int h = 12, int min = 0]) => DateTime.utc(y, m, d, h, min).millisecondsSinceEpoch ~/ 1000;

Never _notThis(String what) => throw ApiException(ApiFailure.refused, what, status: 400);

Future<Uint8List> _asset(String path) async => (await rootBundle.load(path)).buffer.asUint8List();

Uint8List _text(String s) => Uint8List.fromList(utf8.encode(s));

// ---- people (all made up; example.* domains)

const _sam = 'Sam Taylor <sam@mail.example.org>';
const _ana = 'Ana Pereira <ana.pereira@example.com>';
const _tom = 'Tom Becker <tom@becker.example.net>';
const _priya = 'Priya Raman <priya@example.org>';
const _jo = 'Jo Taylor <jo.taylor@example.com>';
const _helen = 'Helen Brooks <helen@brooks-accounts.example>';
const _marta = 'Marta Novak <marta.novak@example.net>';
const _lena = 'Lena Fischer (Harbour Lettings) <lena@harbour-lettings.example>';
const _northwind = 'Northwind Studio <accounts@northwind.example>';
const _bookshop = 'Corner Bookshop <orders@cornerbooks.example>';
const _club = 'Riverside Cycling Club <news@riverside-cc.example>';
const _hosting = 'Example Hosting <support@hosting.example>';

// ---- mail: the 2012 mbox and the mail server's Maildir

class _Att {
  const _Att(this.name, this.type, this.size, {this.asset, this.text});
  final String name, type;
  final int size;
  final String? asset, text;
}

class _Msg {
  _Msg(
    this.thread,
    this.depth,
    this.from,
    this.to,
    this.subject,
    this.date,
    this.text, {
    this.flags = 'S',
    this.atts = const [],
  });
  final int thread, depth;
  final String from, to, subject;
  final int date;
  final String text;
  final String flags;
  final List<_Att> atts;

  /// Its offset in the mbox, or its Maildir file's index id.
  int key = 0;
  int size = 0;

  bool get unread => !flags.contains('S');
}

const _photoAtts = [
  _Att('IMG_2014.JPG', 'image/jpeg', 2841733, asset: 'assets/demo/photo1.jpg'),
  _Att('IMG_2017.JPG', 'image/jpeg', 3102918, asset: 'assets/demo/photo3.jpg'),
];

/// archive-2012.mbox: a year of Sam's mail, in file order (oldest first).
final List<_Msg> _mbox = () {
  final m = [
    _Msg(
      1,
      0,
      _helen,
      _sam,
      'Tax return 2011',
      _t(2012, 1, 9, 9, 12),
      'Hi Sam,\n\nThe deadline for the 2011 return is the end of the month. Could you send me the invoices you '
          'raised last year and the receipts for the new laptop?\n\nBest wishes,\nHelen',
    ),
    _Msg(
      1,
      1,
      _sam,
      _helen,
      'Re: Tax return 2011',
      _t(2012, 1, 10, 20, 41),
      'Hi Helen,\n\nAttached is the list of invoices. The laptop receipt is in the post.\n\nSam',
      flags: 'RS',
      atts: [const _Att('invoices-2011.csv', 'text/csv', 1290, text: _invoicesCsv)],
    ),
    _Msg(
      1,
      2,
      _helen,
      _sam,
      'Re: Tax return 2011',
      _t(2012, 1, 23, 16, 5),
      'Thanks Sam, all filed. You should get about £340 back in March.\n\nHelen',
    ),
    _Msg(
      2,
      0,
      _lena,
      _sam,
      'Flat viewing on Saturday',
      _t(2012, 2, 14, 11, 30),
      'Dear Sam,\n\nThis is to confirm the viewing of 14 Quay Street, flat 3, on Saturday at 10:30. The landlord '
          'will be there to answer questions about the heating.\n\nKind regards,\nLena Fischer\nHarbour Lettings',
    ),
    _Msg(
      2,
      1,
      _sam,
      _lena,
      'Re: Flat viewing on Saturday',
      _t(2012, 2, 14, 18, 2),
      'Hi Lena, thanks, see you then. Is there a bike shed?\n\nSam',
      flags: 'RS',
    ),
    _Msg(
      2,
      2,
      _lena,
      _sam,
      'Re: Flat viewing on Saturday',
      _t(2012, 2, 15, 9, 15),
      'Yes, there is a locked bike store in the courtyard.\n\nLena',
    ),
    _Msg(
      3,
      0,
      _tom,
      _sam,
      'Server move to the new VPS',
      _t(2012, 3, 3, 22, 10),
      'Sam,\n\nThe old box is out of disk again. I set up a VPS at the new host; can you check the mail config '
          'before we switch the DNS on Sunday?\n\nThe Postfix bits are attached.\n\nTom',
      atts: [const _Att('main.cf', 'text/plain', 1840, text: _postfix)],
    ),
    _Msg(
      3,
      1,
      _sam,
      _tom,
      'Re: Server move to the new VPS',
      _t(2012, 3, 4, 10, 47),
      'Looks fine apart from the relay line: it should only allow our own network. Fixed it in the copy on the '
          'server.\n\nSam',
      flags: 'RS',
    ),
    _Msg(3, 2, _tom, _sam, 'Re: Server move to the new VPS', _t(2012, 3, 4, 11, 20), 'Thanks. Switching tonight then.'),
    _Msg(
      3,
      3,
      _tom,
      _sam,
      'Re: Server move to the new VPS',
      _t(2012, 3, 5, 8, 2),
      'Done. Mail is flowing; the old box can go next month.\n\nTom',
      flags: 'FS',
    ),
    _Msg(
      4,
      0,
      _priya,
      _sam,
      'Dinner on Friday?',
      _t(2012, 4, 18, 13, 5),
      'Hey! We\'re trying the new Lebanese place on Friday, 8pm. Coming? Ana is in too.\n\nP.',
    ),
    _Msg(
      4,
      1,
      _sam,
      _priya,
      'Re: Dinner on Friday?',
      _t(2012, 4, 18, 13, 40),
      'Yes please! I\'ll bring Jo.',
      flags: 'RS',
    ),
    _Msg(4, 2, _priya, _sam, 'Re: Dinner on Friday?', _t(2012, 4, 19, 9, 0), 'Booked for five. See you there.'),
    _Msg(
      5,
      0,
      _northwind,
      _sam,
      'Invoice 2012-031',
      _t(2012, 5, 2, 9, 0),
      'Dear Sam,\n\nPlease find attached remittance advice for invoice 2012-031. Payment was made today.\n\n'
          'Northwind Studio accounts',
      atts: [const _Att('remittance-2012-031.pdf', 'application/pdf', 48312, asset: 'assets/demo/invoice.pdf')],
    ),
    _Msg(
      5,
      1,
      _sam,
      _northwind,
      'Re: Invoice 2012-031',
      _t(2012, 5, 2, 14, 30),
      'Received, thank you.\n\nSam Taylor',
      flags: 'RS',
    ),
    _Msg(
      6,
      0,
      _ana,
      _sam,
      'Photos from the lake',
      _t(2012, 6, 11, 21, 15),
      'Sam,\n\nHere are the two good ones from Sunday. The one with the boat is my favourite.\n\nAna x',
      atts: _photoAtts,
    ),
    _Msg(
      6,
      1,
      _sam,
      _ana,
      'Re: Photos from the lake',
      _t(2012, 6, 12, 7, 50),
      'They\'re lovely! Can I print the boat one for Mum?',
      flags: 'RS',
    ),
    _Msg(
      6,
      2,
      _ana,
      _sam,
      'Re: Photos from the lake',
      _t(2012, 6, 12, 8, 30),
      'Of course. I\'ll send you the full size.',
    ),
    _Msg(
      7,
      0,
      _marta,
      _sam,
      'Conference talk: slides draft',
      _t(2012, 7, 20, 16, 0),
      'Hi Sam,\n\nHere is the outline for our talk in September. I\'d like your part on backups to come second, '
          'after the intro.\n\nMarta',
      atts: [const _Att('talk-outline.txt', 'text/plain', 412, text: _outline)],
    ),
    _Msg(
      7,
      1,
      _sam,
      _marta,
      'Re: Conference talk: slides draft',
      _t(2012, 7, 21, 10, 12),
      'Works for me. I\'ll do 15 minutes on why people lose photos and how to stop it.',
      flags: 'RS',
    ),
    _Msg(
      7,
      2,
      _marta,
      _sam,
      'Re: Conference talk: slides draft',
      _t(2012, 7, 22, 9, 40),
      'Perfect. Rehearsal on the 10th?',
    ),
    _Msg(7, 3, _sam, _marta, 'Re: Conference talk: slides draft', _t(2012, 7, 22, 12, 1), 'The 10th is good.'),
    _Msg(
      8,
      0,
      _bookshop,
      _sam,
      'Your order has shipped',
      _t(2012, 8, 6, 15, 22),
      'Hello Sam,\n\nYour order #40121 (2 items) has shipped and should arrive in 2–3 days.\n\nCorner Bookshop',
    ),
    _Msg(
      9,
      0,
      _jo,
      _sam,
      'Mum\'s birthday',
      _t(2012, 9, 1, 19, 30),
      'Sam, it\'s Mum\'s 60th on the 22nd. Shall we go halves on the weekend in the Lakes?\n\nJo',
    ),
    _Msg(
      9,
      1,
      _sam,
      _jo,
      'Re: Mum\'s birthday',
      _t(2012, 9, 1, 21, 5),
      'Yes, great idea. I can print Ana\'s lake photo for a card.',
      flags: 'RS',
    ),
    _Msg(9, 2, _jo, _sam, 'Re: Mum\'s birthday', _t(2012, 9, 2, 8, 44), 'Booked the cottage. £180 each.'),
    _Msg(9, 3, _sam, _jo, 'Re: Mum\'s birthday', _t(2012, 9, 2, 9, 10), 'Sent you my half.'),
    _Msg(
      10,
      0,
      _tom,
      _sam,
      'Backup drive',
      _t(2012, 10, 14, 20, 0),
      'The Seagate you lent me is full of old photos — want it back, or shall I wipe it?\n\nTom',
    ),
    _Msg(
      10,
      1,
      _sam,
      _tom,
      'Re: Backup drive',
      _t(2012, 10, 14, 21, 12),
      'Don\'t wipe it! That\'s the only copy of 2009. Bring it on Friday.',
      flags: 'FRS',
    ),
    _Msg(
      11,
      0,
      _ana,
      _sam,
      'Weekend in Lisbon',
      _t(2012, 11, 2, 18, 30),
      'Flights are cheap at the end of the month. Lisbon? I know a place in Alfama.',
    ),
    _Msg(11, 1, _sam, _ana, 'Re: Weekend in Lisbon', _t(2012, 11, 2, 19, 0), 'I\'m in. 23rd to 25th?', flags: 'RS'),
    _Msg(11, 2, _ana, _sam, 'Re: Weekend in Lisbon', _t(2012, 11, 3, 10, 5), 'Booked! Sending the details.'),
    _Msg(
      11,
      3,
      _ana,
      _sam,
      'Fwd: Your booking in Alfama',
      _t(2012, 11, 3, 10, 7),
      'Here you go.\n\n---------- Forwarded message ----------\nCheck-in from 15:00 on 23 November.',
    ),
    _Msg(11, 4, _sam, _ana, 'Re: Weekend in Lisbon', _t(2012, 11, 26, 9, 0), 'Best weekend of the year. Thank you!'),
    _Msg(
      12,
      0,
      _club,
      _sam,
      'Newsletter: December',
      _t(2012, 12, 1, 7, 0),
      'Riverside Cycling Club — December\n\n• Winter rides start at 9:30 from the bridge\n• Annual dinner on the '
          '14th\n• Subscriptions for 2013 are due in January',
      flags: '',
    ),
    _Msg(
      13,
      0,
      _helen,
      _sam,
      'Year-end reminder',
      _t(2012, 12, 20, 10, 0),
      'Hi Sam, a reminder to keep your receipts for 2012 together. Happy holidays!\n\nHelen',
      flags: '',
    ),
  ];
  var at = 0;
  for (final x in m) {
    x.key = at;
    x.size = 3200 + x.text.length * 2 + x.atts.fold<int>(0, (a, b) => a + b.size * 4 ~/ 3);
    at += x.size;
  }
  return m;
}();

/// The mail server's Maildir: its messages by file, as demo_fs.dart lays them out (cur/ then new/).
final Map<int, _Msg> _maildir = () {
  final box = _fs.byKey[keyOfPath('/media/sam/Seagate Expansion/Mail/mail.example.org')];
  if (box == null) return <int, _Msg>{};
  final files = [
    for (final sub in box.children!)
      if (sub.isDir && (sub.name == 'cur' || sub.name == 'new')) ...sub.children!,
  ];
  final msgs = [
    _Msg(
      1,
      0,
      _hosting,
      _sam,
      'mail.example.org is shutting down on 31 May',
      _t(2020, 4, 20, 9, 0),
      'Dear customer,\n\nWe are closing our mail hosting. Your mailbox sam@mail.example.org will stop receiving '
          'mail on 31 May 2020. Please move your mail before then.\n\nExample Hosting',
    ),
    _Msg(
      2,
      0,
      _sam,
      _tom,
      'Moving your mail',
      _t(2020, 4, 21, 18, 0),
      'Tom, the host is closing. I\'m copying the whole maildir to the Seagate every night until it goes. '
          'Shall I do yours too?',
      flags: 'RS',
    ),
    _Msg(2, 1, _tom, _sam, 'Re: Moving your mail', _t(2020, 4, 22, 8, 15), 'Yes please. Mine is tom@, same server.'),
    _Msg(
      2,
      2,
      _sam,
      _tom,
      'Re: Moving your mail',
      _t(2020, 4, 22, 21, 40),
      'Done: both copied, and the script runs nightly.',
    ),
    _Msg(
      3,
      0,
      _jo,
      _sam,
      'Photos for the album',
      _t(2020, 4, 23, 17, 30),
      'The ones from Mum\'s garden.',
      atts: _photoAtts,
    ),
    _Msg(
      4,
      0,
      _hosting,
      _sam,
      'Your invoice for April',
      _t(2020, 4, 25, 6, 0),
      'Your invoice for April 2020 is attached. Thank you for being a customer.',
      atts: [const _Att('invoice-2020-04.pdf', 'application/pdf', 49312, asset: 'assets/demo/invoice.pdf')],
    ),
    _Msg(
      1,
      1,
      _hosting,
      _sam,
      'Last reminder: mail.example.org closes today',
      _t(2020, 5, 31, 8, 0),
      'This is the last reminder: mail to sam@mail.example.org stops today at midnight.',
      flags: '',
    ),
  ];
  return {
    for (final (i, f) in files.indexed)
      if (i < msgs.length)
        f.id!: msgs[i]
          ..key = f.id!
          ..size = f.size,
  };
}();

const _invoicesCsv = '''Invoice,Date,Client,Amount
2011-001,2011-01-28,Northwind Studio,1450.00
2011-002,2011-02-25,Northwind Studio,1450.00
2011-003,2011-03-30,Blue Harbour Cafe,380.00
2011-004,2011-05-31,Northwind Studio,2100.00
2011-005,2011-07-29,Hillside School,950.00
2011-006,2011-09-30,Northwind Studio,1450.00
2011-007,2011-11-30,Blue Harbour Cafe,420.00
2011-008,2011-12-21,Northwind Studio,1450.00
''';

const _postfix = r'''# /etc/postfix/main.cf (new VPS)
myhostname = mail.example.org
mydomain = example.org
myorigin = $mydomain
inet_interfaces = all
mydestination = $myhostname, localhost.$mydomain, localhost
mynetworks = 127.0.0.0/8 10.0.0.0/24
relayhost =
home_mailbox = Maildir/
smtpd_tls_cert_file = /etc/ssl/certs/mail.pem
smtpd_tls_key_file = /etc/ssl/private/mail.key
''';

const _outline = '''Keeping what matters — outline
1. Intro (Marta, 5 min)
2. Why people lose photos (Sam, 15 min)
   - one copy is no copy
   - old drives in drawers
3. A plan anyone can follow (Marta, 10 min)
4. Questions
''';

_Msg? _maildirMessage(DemoNode n) => _maildir[n.id];

bool _isMbox(DemoNode n) => n.name == 'archive-2012.mbox';

_Msg _message(DemoNode n, int? at) {
  if (at != null && _isMbox(n)) {
    return _mbox.where((m) => m.key == at).firstOrNull ?? _notThis('no message at $at in this mbox');
  }
  return _maildirMessage(n) ?? _notThis('not a mail message');
}

Map<String, Object?> _mailJson(DemoNode n, int? at) {
  final m = _message(n, at);
  return {
    'from': m.from,
    'to': m.to,
    'cc': '',
    'reply_to': '',
    'subject': m.subject,
    'date': m.date,
    'date_text': DateTime.fromMillisecondsSinceEpoch(m.date * 1000, isUtc: true).toIso8601String(),
    'message_id': '<${m.key}.${m.date}@demo.example>',
    'text': m.text,
    'from_html': m.from == _club,
    'attachments': [
      for (final (i, a) in m.atts.indexed) {'part': i, 'name': a.name, 'type': a.type, 'size': a.size},
    ],
    'truncated': false,
  };
}

Future<Uint8List> _attachmentBytes(DemoNode n, int? at, int part) async {
  final m = _message(n, at);
  if (part < 0 || part >= m.atts.length) throw ApiException(ApiFailure.notFound, 'no such attachment', status: 404);
  final a = m.atts[part];
  if (a.asset != null) return _asset(a.asset!);
  return _text(a.text ?? '');
}

typedef _Box = ({List<_Msg> msgs, String kind, int read, int size, bool done});

/// What the demo has "read" of a mailbox: the mbox comes in two slices ("Keep reading" reads the
/// rest), the Maildir at once.
_Box _box(DemoBackDownApi d, Map<String, String> p) {
  final dir = p['dir'];
  if (dir != null && dir.isNotEmpty) {
    final n = _fs.byKey[dir];
    if (n == null || !n.isDir) _notThis('not an indexed folder');
    final kids = {for (final c in n.children!) c.name};
    if (!kids.contains('cur') && !kids.contains('new')) {
      _notThis('${n.path} isn\'t a Maildir (it has no cur/ or new/ folder)');
    }
    final msgs = _maildir.values.toList();
    return (msgs: msgs, kind: 'maildir', read: msgs.length, size: msgs.length, done: true);
  }
  final n = _node(p);
  if (!_isMbox(n)) _notThis('not an mbox');
  final state = (d.state['viewers'] ??= <String, Object?>{}) as Map<String, Object?>;
  if (p['more'] == '1') state['mbox read'] = true;
  final all = state['mbox read'] == true;
  final msgs = all ? _mbox : _mbox.sublist(0, 24);
  return (msgs: msgs, kind: 'mbox', read: all ? n.size : (n.size * 0.58).round(), size: n.size, done: all);
}

String _person(String from) {
  final i = from.lastIndexOf(' <');
  return i > 0 ? from.substring(0, i) : from;
}

bool _matches(_Msg m, String q) => m.from.toLowerCase().contains(q) || m.subject.toLowerCase().contains(q);

Map<String, Object?> _summary(List<_Msg> t, String q, {bool flat = false}) {
  final who = <String>[];
  final seen = <String>{};
  for (final m in t) {
    if (seen.add(m.from)) who.add(_person(m.from));
  }
  final any = t.map((m) => m.flags).join();
  return {
    'key': t.first.key,
    'subject': flat ? t.first.subject : t.first.subject.replaceFirst(RegExp(r'^(Re|Fwd): ', caseSensitive: false), ''),
    'who': who.take(4).toList(),
    'people': who.length,
    'count': t.length,
    'first': t.first.date,
    'last': t.last.date,
    'unread': t.where((m) => m.unread).length,
    'flags': [if (any.contains('F')) 'F', if (any.contains('R')) 'R'].join(),
    'hits': q.isEmpty ? 0 : t.where((m) => _matches(m, q)).length,
  };
}

Map<String, Object?> _status(_Box b) => {
  'kind': b.kind,
  'messages': b.msgs.length,
  'read': b.read,
  'size': b.size,
  'done': b.done,
  'capped': false,
  'unreadable': 0,
};

List<List<_Msg>> _grouped(List<_Msg> msgs, bool flat) {
  if (flat) {
    return [
      for (final m in msgs) [m],
    ];
  }
  final by = <int, List<_Msg>>{};
  for (final m in msgs) {
    (by[m.thread] ??= []).add(m);
  }
  return by.values.toList();
}

Map<String, Object?> _threads(DemoBackDownApi d, Map<String, String> p) {
  final b = _box(d, p);
  final q = (p['q'] ?? '').trim().toLowerCase();
  final flat = p['flat'] == '1';
  final groups = [
    for (final g in _grouped(b.msgs, flat))
      if (q.isEmpty || g.any((m) => _matches(m, q))) g,
  ]..sort((a, b) => b.last.date.compareTo(a.last.date));
  final offset = int.tryParse(p['offset'] ?? '') ?? 0;
  final limit = int.tryParse(p['limit'] ?? '') ?? 100;
  return {
    ..._status(b),
    'threads': [for (final g in groups.skip(offset).take(limit)) _summary(g, q, flat: flat)],
    'total': groups.length,
    'offset': offset,
    if (b.kind == 'maildir' && offset == 0) ...{'folders': <Object?>[], 'name': 'mail.example.org'},
  };
}

Map<String, Object?> _thread(DemoBackDownApi d, Map<String, String> p) {
  final b = _box(d, p);
  final key = int.tryParse(p['key'] ?? '');
  final q = (p['q'] ?? '').trim().toLowerCase();
  final flat = p['flat'] == '1';
  final g = _grouped(b.msgs, flat).where((g) => g.any((m) => m.key == key)).firstOrNull;
  if (g == null) _notThis('that message isn\'t in what has been read of this mailbox');
  final who = <String>[];
  for (final m in g) {
    if (!who.contains(_person(m.from))) who.add(_person(m.from));
  }
  return {
    ..._status(b),
    'thread': _summary(g, '', flat: flat),
    'who': who,
    'messages': [
      for (final m in g)
        {
          'key': m.key,
          'from': m.from,
          'subject': m.subject,
          'date': m.date,
          'size': m.size,
          'depth': flat ? 0 : m.depth.clamp(0, 3),
          'flags': m.flags,
          'unread': m.unread,
          'dup': false,
          'orphan': false,
          'hit': q.isNotEmpty && _matches(m, q),
        },
    ],
  };
}

// ---- site-backup.zip

class _Entry {
  const _Entry(this.name, this.size, this.mtime, {this.asset, this.text});
  final String name;
  final int size, mtime;
  final String? asset, text;
  bool get dir => name.endsWith('/');
}

const _indexHtml = '''<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>Taylor Photography</title>
  <link rel="stylesheet" href="css/site.css">
</head>
<body>
  <header><img src="images/header.jpg" alt=""><h1>Taylor Photography</h1></header>
  <nav><a href="index.html">Home</a> <a href="gallery.html">Gallery</a> <a href="contact.php">Contact</a></nav>
  <main>
    <p>Weddings, portraits and landscapes in the North West since 2009.</p>
    <p><a href="downloads/price-list-2018.pdf">Price list for 2018</a></p>
  </main>
  <script src="js/app.js"></script>
</body>
</html>
''';

const _siteCss = '''body { font: 16px/1.5 Georgia, serif; margin: 0; color: #222; }
header { position: relative; }
header img { width: 100%; height: 320px; object-fit: cover; }
header h1 { position: absolute; bottom: 16px; left: 24px; color: #fff; }
nav a { margin-right: 12px; color: #a33; }
.gallery { display: grid; grid-template-columns: repeat(auto-fill, minmax(220px, 1fr)); gap: 8px; }
''';

const _appJs = r'''// lightbox for the gallery
document.querySelectorAll('.gallery img').forEach(img => {
  img.addEventListener('click', () => {
    const big = document.createElement('div');
    big.className = 'lightbox';
    big.innerHTML = `<img src="${img.dataset.full}" alt="">`;
    big.onclick = () => big.remove();
    document.body.append(big);
  });
});
''';

const _contactPhp = r'''<?php
// contact form: mails the studio, nothing stored
$to = 'studio@taylor-photo.example';
if ($_SERVER['REQUEST_METHOD'] === 'POST') {
    $name = trim($_POST['name'] ?? '');
    $msg = trim($_POST['message'] ?? '');
    if ($name !== '' && $msg !== '') {
        mail($to, "Website enquiry from $name", $msg);
        header('Location: thanks.html');
        exit;
    }
}
?>
<form method="post"><input name="name"><textarea name="message"></textarea><button>Send</button></form>
''';

const _ordersCsv = '''order,date,customer,item,amount
1001,2018-01-08,Hillside School,Class photos (32),448.00
1002,2018-01-22,R. Ahmed,Portrait session,120.00
1003,2018-02-03,Blue Harbour Cafe,Menu photography,260.00
1004,2018-02-17,M. & D. Clarke,Wedding (full day),1650.00
1005,2018-03-09,Northwind Studio,Product shots (40),520.00
1006,2018-03-24,L. Fischer,Portrait session,120.00
1007,2018-04-14,Riverside Cycling Club,Event coverage,300.00
1008,2018-04-28,K. Osei,Prints (A3 x 4),96.00
1009,2018-05-02,Hillside School,Sports day,280.00
''';

const _htaccess = '''RewriteEngine On
RewriteCond %{HTTPS} off
RewriteRule ^ https://%{HTTP_HOST}%{REQUEST_URI} [L,R=301]
''';

final List<_Entry> _site = [
  _Entry('site/', 0, _t(2018, 5, 5, 3)),
  _Entry('site/index.html', _indexHtml.length, _t(2018, 4, 30), text: _indexHtml),
  _Entry('site/gallery.html', 5120, _t(2018, 4, 30)),
  _Entry('site/contact.php', _contactPhp.length, _t(2017, 11, 2), text: _contactPhp),
  _Entry('site/.htaccess', _htaccess.length, _t(2016, 6, 1), text: _htaccess),
  _Entry('site/robots.txt', 34, _t(2016, 6, 1), text: 'User-agent: *\nDisallow: /downloads/\n'),
  _Entry('site/css/', 0, _t(2018, 4, 30)),
  _Entry('site/css/site.css', _siteCss.length, _t(2018, 4, 30), text: _siteCss),
  _Entry('site/js/app.js', _appJs.length, _t(2018, 2, 11), text: _appJs),
  _Entry('site/images/header.jpg', 412880, _t(2017, 3, 3), asset: 'assets/demo/photo2.jpg'),
  _Entry('site/images/studio.jpg', 288410, _t(2017, 3, 3), asset: 'assets/demo/photo4.jpg'),
  for (var i = 0; i < 18; i++)
    _Entry(
      'site/gallery/2017/IMG_${3401 + i * 7}.JPG',
      2600000 + (i * 91733) % 900000,
      _t(2017, 1 + i % 12, 3 + i),
      asset: 'assets/demo/photo${1 + i % 4}.jpg',
    ),
  _Entry('site/downloads/price-list-2018.pdf', 49312, _t(2018, 1, 2), asset: 'assets/demo/invoice.pdf'),
  _Entry('site/data/orders-2018.csv', _ordersCsv.length, _t(2018, 5, 2), text: _ordersCsv),
  _Entry('site/videos/promo-2017.mp4', 631242752, _t(2017, 9, 9)),
  _Entry('backups/db-2018-05-01.sql.gz', 48210944, _t(2018, 5, 1, 4)),
];

Map<String, Object?> _archive(DemoNode n, int? limit) {
  if (n.name != 'site-backup.zip') _notThis('not an archive BackDown can read');
  final list = limit == null ? _site : _site.take(limit).toList();
  bool packed(_Entry e) => e.asset != null || e.name.endsWith('.gz') || e.name.endsWith('.mp4');
  return {
    'format': 'zip',
    'entries': [
      for (final e in list)
        {
          'name': e.name,
          'size': e.size,
          'csize': e.dir ? 0 : (packed(e) ? e.size : e.size ~/ 3),
          'mtime': e.mtime,
          'dir': e.dir,
          'encrypted': false,
        },
    ],
    'total': _site.length,
    'total_size': _site.fold<int>(0, (a, e) => a + e.size),
    'complete': true,
    'note': null,
  };
}

// ---- old-laptop.img (a ThinkPad's disk) and the Ubuntu ISO

const _gib = 1 << 30;
const _esp = 512 << 20;

int _rootSize(DemoNode n) => n.size - 8 * _gib - _esp - (1 << 20);

Map<String, Object?> _disk(DemoNode n, String? path) {
  if (n.name == 'old-laptop.img') {
    return {
      'file_size': n.size,
      'format': 'raw',
      'virtual_size': n.size,
      'table': 'GPT',
      'details': [
        ['Disk GUID', '7D3B1E52-90A4-4C6B-9F21-5E0C8A41D7B3'],
      ],
      'partitions': [
        {
          'index': 1,
          'type': 'EFI system',
          'code': 'C12A7328-F81F-11D2-BA4B-00A0C93EC93B',
          'name': 'EFI System Partition',
          'start': 1 << 20,
          'size': _esp,
          'fs': {'kind': 'FAT32', 'label': null, 'size': _esp},
        },
        {
          'index': 2,
          'type': 'Linux filesystem',
          'code': '0FC63DAF-8483-4772-8E79-3D69D8477DE4',
          'name': null,
          'start': (1 << 20) + _esp,
          'size': _rootSize(n),
          'fs': {'kind': 'ext4', 'label': 'thinkpad', 'size': _rootSize(n)},
        },
        {
          'index': 3,
          'type': 'Linux swap',
          'code': '0657FD6D-A4AB-43C4-84E5-0933C84B4F4F',
          'name': null,
          'start': n.size - 8 * _gib,
          'size': 8 * _gib - (1 << 20),
          'fs': {'kind': 'Linux swap', 'label': null, 'size': 8 * _gib - (1 << 20)},
        },
      ],
    };
  }
  if (n.name.endsWith('.iso')) {
    final at = (path ?? '').replaceAll(RegExp(r'^/+|/+$'), '');
    final dir = _isoDirs[at] ?? _notThis('$at isn\'t on this disc');
    return {
      'file_size': n.size,
      'format': 'iso9660',
      'virtual_size': n.size,
      'label': 'Ubuntu 24.04 LTS amd64',
      'details': [
        ['System', 'LINUX'],
        ['Application', 'XORRISO-1.5.6 2023.06.07.180001, LIBISOBURN-1.5.6, LIBISOFS-1.5.6, LIBBURN-1.5.6'],
        ['Created', '2024-04-24 11:29'],
        ['Bootable', 'yes (El Torito)'],
      ],
      'path': at,
      'entries': dir,
      'truncated': false,
    };
  }
  return {'file_size': n.size, 'format': 'unknown'};
}

final _isoTime = _t(2024, 4, 24, 11, 29);

Map<String, Object?> _iso(String name, int size, {bool dir = false}) => {
  'name': name,
  'size': size,
  'dir': dir,
  'mtime': _isoTime,
};

final Map<String, List<Map<String, Object?>>> _isoDirs = {
  '': [
    for (final d in ['boot', 'casper', 'dists', 'EFI', 'install', 'pool', 'preseed']) _iso(d, 0, dir: true),
    _iso('boot.catalog', 2048),
    _iso('md5sum.txt', _md5sum.length),
  ],
  'casper': [
    _iso('filesystem.manifest', 61440),
    _iso('filesystem.size', 11),
    _iso('initrd', 71303168),
    _iso('minimal.squashfs', 3221225472),
    _iso('vmlinuz', 14680064),
  ],
  'boot': [_iso('grub', 0, dir: true)],
  'boot/grub': [_iso('grub.cfg', _grubCfg.length), _iso('font.pf2', 2394108)],
  for (final d in ['dists', 'EFI', 'install', 'pool', 'preseed']) d: <Map<String, Object?>>[],
};

const _md5sum = '''e3b0c44298fc1c149afbf4c8996fb924  ./.disk/info
5d41402abc4b2a76b9719d911017c592  ./boot/grub/grub.cfg
098f6bcd4621d373cade4e832627b4f6  ./casper/vmlinuz
ad0234829205b9033196ba818f7a872b  ./casper/initrd
8ad8757baa8564dc136c1e07507f4a98  ./casper/minimal.squashfs
''';

const _grubCfg = '''set timeout=30
loadfont unicode
set menu_color_normal=white/black
set menu_color_highlight=black/light-gray

menuentry "Try or Install Ubuntu" {
\tset gfxpayload=keep
\tlinux\t/casper/vmlinuz  ---
\tinitrd\t/casper/initrd
}
menuentry "Ubuntu (safe graphics)" {
\tset gfxpayload=keep
\tlinux\t/casper/vmlinuz  nomodeset  ---
\tinitrd\t/casper/initrd
}
''';

const _bashrc = '''# ~/.bashrc
HISTSIZE=5000
HISTCONTROL=ignoredups
alias ll='ls -alF'
alias backup='rsync -a --info=progress2 ~/Pictures/ "/media/sam/Seagate Expansion/Photos/"'
export EDITOR=vim
''';

const _profile = r'''# ~/.profile
if [ -n "$BASH_VERSION" ]; then
  . "$HOME/.bashrc"
fi
PATH="$HOME/bin:$PATH"
''';

const _laptopNotes = '''Things to move off the ThinkPad before it goes
- Pictures (the 2016 ones aren't on the Seagate yet)
- thesis draft
- ~/.ssh keys
The fan is making the noise again.
''';

const _thesis = r'''\documentclass{article}
\title{Keeping Family Photos}
\author{Sam Taylor}
\begin{document}
\maketitle
\section{Introduction}
Most families keep one copy of their photos, on one drive, in one drawer.
\end{document}
''';

const _fstab = '''UUID=3f1d2c4e-5a6b-4c7d-8e9f-0a1b2c3d4e5f /         ext4 errors=remount-ro 0 1
UUID=8A3C-11F2                            /boot/efi vfat umask=0077        0 1
/swapfile                                 none      swap sw                0 0
''';

const _efiGrub = r'''search.fs_uuid 3f1d2c4e-5a6b-4c7d-8e9f-0a1b2c3d4e5f root
set prefix=($root)'/boot/grub'
configfile $prefix/grub.cfg
''';

class _FsFile {
  const _FsFile(this.name, this.size, this.mtime, {this.dir = false, this.text, this.asset, this.link, this.deleted});
  final String name;
  final int size, mtime;
  final bool dir;
  final String? text, asset, link;
  final Map<String, Object?>? deleted;

  Map<String, Object?> get json => {
    'name': name,
    'dir': dir,
    'size': size,
    'mtime': mtime,
    'link': ?link,
    'deleted': ?deleted,
  };
}

final _laptopTime = _t(2017, 10, 11, 22);

/// The laptop's two filesystems: the EFI partition (FAT32, with deleted files) and ext4.
final Map<String, Map<String, List<_FsFile>>> _laptop = {
  '1': {
    '': [
      _FsFile('EFI', 0, _t(2016, 3, 1), dir: true),
      _FsFile(
        'OLDBOOT.EFI',
        1187328,
        _t(2016, 2, 20),
        deleted: {
          'state': 'recoverable',
          'confidence': 92,
          'why': 'Its clusters are still free and its size matches them.',
          'token': 7,
          'seg': 'OLDBOOT.EFI\u00007',
        },
      ),
      _FsFile(
        'BOOTX64.BAK',
        958464,
        _t(2016, 2, 20),
        deleted: {
          'state': 'partly overwritten',
          'confidence': 41,
          'why': 'Some of its clusters now belong to other files.',
          'token': 9,
          'seg': 'BOOTX64.BAK\u00009',
        },
      ),
    ],
    'EFI': [_FsFile('BOOT', 0, _t(2016, 3, 1), dir: true), _FsFile('ubuntu', 0, _t(2017, 4, 20), dir: true)],
    'EFI/BOOT': [_FsFile('BOOTX64.EFI', 958464, _t(2017, 4, 20)), _FsFile('fbx64.efi', 86528, _t(2017, 4, 20))],
    'EFI/ubuntu': [
      _FsFile('grubx64.efi', 1187328, _t(2017, 4, 20)),
      _FsFile('shimx64.efi', 958464, _t(2017, 4, 20)),
      _FsFile('grub.cfg', _efiGrub.length, _t(2017, 4, 20), text: _efiGrub),
    ],
  },
  '2': {
    '': [
      for (final d in ['boot', 'etc', 'home', 'lost+found', 'opt', 'root', 'srv', 'tmp', 'usr', 'var'])
        _FsFile(d, 0, _laptopTime, dir: true),
      _FsFile('bin', 7, _t(2017, 4, 20), link: 'usr/bin'),
      _FsFile('lib', 7, _t(2017, 4, 20), link: 'usr/lib'),
    ],
    'home': [_FsFile('sam', 0, _laptopTime, dir: true)],
    'home/sam': [
      for (final d in ['Documents', 'Music', 'Pictures']) _FsFile(d, 0, _laptopTime, dir: true),
      _FsFile('.bashrc', _bashrc.length, _t(2017, 6, 2), text: _bashrc),
      _FsFile('.profile', _profile.length, _t(2017, 4, 20), text: _profile),
      _FsFile('notes.txt', _laptopNotes.length, _t(2017, 10, 9), text: _laptopNotes),
    ],
    'home/sam/Documents': [
      _FsFile('thesis-draft.tex', 184220, _t(2017, 9, 30), text: _thesis),
      _FsFile('cv-2017.odt', 38104, _t(2017, 5, 12)),
      _FsFile('budget.ods', 21877, _t(2017, 8, 1)),
    ],
    'home/sam/Pictures': [
      for (var i = 0; i < 8; i++)
        _FsFile(
          'IMG_${5101 + i}.JPG',
          2800000 + i * 137411,
          _t(2016, 1 + i, 3 + i),
          asset: 'assets/demo/photo${1 + i % 4}.jpg',
        ),
    ],
    'home/sam/Music': <_FsFile>[],
    'boot': [
      _FsFile('grub', 0, _t(2017, 10, 2), dir: true),
      _FsFile('vmlinuz-4.10.0-37-generic', 7451504, _t(2017, 10, 2)),
    ],
    'boot/grub': [_FsFile('grub.cfg', _grubCfg.length, _t(2017, 10, 2), text: _grubCfg)],
    'etc': [
      _FsFile('fstab', _fstab.length, _t(2017, 4, 20), text: _fstab),
      _FsFile('hostname', 9, _t(2017, 4, 20), text: 'thinkpad\n'),
      _FsFile('shadow', 1278, _t(2017, 6, 1)),
    ],
    for (final d in ['lost+found', 'opt', 'root', 'srv', 'tmp', 'usr', 'var']) d: <_FsFile>[],
  },
};

const _extWhy =
    'ext3 and ext4 erase where a deleted file\'s data was, so deleted files can\'t be brought back by name here. In '
    'Rescue, “Files were deleted, or it was formatted” can still find photos, videos and documents on it by what\'s '
    'inside them';

/// A path with deleted entries' tokens taken off ("OLDBOOT.EFI\u00007" → "OLDBOOT.EFI").
String _plain(String path) => path.split('/').map((s) => s.split('\u0000').first).join('/');

Map<String, Object?> _fsList(DemoNode n, String part, String path, bool deleted) {
  if (n.name != 'old-laptop.img') _notThis('no filesystem BackDown can read in partition $part');
  final parts = _laptop[part] ?? _notThis('no filesystem BackDown can read in partition $part');
  final at = path.replaceAll(RegExp(r'^/+|/+$'), '');
  final all = parts[at] ?? _notThis('$at isn\'t a folder here');
  final ext = part == '2';
  return {
    'part': part,
    'fs': {
      'kind': ext ? 'ext4' : 'FAT32',
      'label': ext ? 'thinkpad' : null,
      'size': ext ? _rootSize(n) : _esp,
      'block': 4096,
      'notes': ext ? ['it wasn\'t unmounted cleanly: the last changes before it stopped may be missing'] : <String>[],
    },
    'path': at,
    'entries': [
      for (final f in all)
        if (f.deleted == null) f.json,
    ],
    'truncated': false,
    'undelete': ext ? _extWhy : null,
    'in_deleted': false,
    if (deleted && !ext) ...{
      'deleted': [
        for (final f in all)
          if (f.deleted != null) f.json,
      ],
      'deleted_truncated': false,
      'deleted_note': null,
    },
  };
}

Future<Uint8List> _entryBytes(DemoNode n, String name, String? part) async {
  String? text, asset;
  if (n.name == 'site-backup.zip') {
    final e = _site.where((e) => e.name == name).firstOrNull;
    if (e == null) throw ApiException(ApiFailure.notFound, '$name isn\'t in this archive', status: 404);
    text = e.text;
    asset = e.asset;
  } else if (n.name == 'old-laptop.img' && part != null) {
    final plain = _plain(name.replaceAll(RegExp(r'^/+'), ''));
    final i = plain.lastIndexOf('/');
    final dir = i < 0 ? '' : plain.substring(0, i);
    final f = _laptop[part]?[dir]?.where((f) => f.name == plain.substring(i + 1)).firstOrNull;
    if (f == null) throw ApiException(ApiFailure.notFound, '$name isn\'t in this filesystem', status: 404);
    text = f.text;
    asset = f.asset;
  } else if (n.name.endsWith('.iso')) {
    text = switch (name.replaceAll(RegExp(r'^/+'), '')) {
      'md5sum.txt' => _md5sum,
      'boot/grub/grub.cfg' => _grubCfg,
      'casper/filesystem.size' => '9876543210\n',
      _ => null,
    };
  }
  if (asset != null) return _asset(asset);
  if (text != null) return _text(text);
  throw ApiException(ApiFailure.notFound, 'The demo has no content for ${baseName(name)}.', status: 404);
}

// ---- contacts.sqlite and the iPhone backup's Manifest.db

const _firsts = [
  'Ana', 'Ben', 'Chloe', 'Dev', 'Ella', 'Femi', 'Grace', 'Hamza', 'Isla', 'Jonas', 'Kira', 'Liam', 'Maya', 'Noah', //
  'Olu', 'Pia', 'Quinn', 'Rosa', 'Sami', 'Tess',
];
const _lasts = [
  'Ahmed', 'Brooks', 'Clarke', 'Dubois', 'Evans', 'Fischer', 'Garcia', 'Hughes', 'Ito', 'Jensen', 'Kowalski', //
  'Lopez', 'Moreau', 'Nakamura', 'Osei',
];
const _groups = ['Family', 'Friends', 'Work', 'Cycling club', 'Clients', 'School'];

String _two(int n) => n.toString().padLeft(2, '0');

List<Object?> _contact(int i) {
  final first = _firsts[i % _firsts.length], last = _lasts[(i * 7) % _lasts.length];
  return [
    i + 1,
    first,
    last,
    '${first.toLowerCase()}.${last.toLowerCase()}@example.${['com', 'net', 'org'][i % 3]}',
    i % 5 == 3 ? null : '+44 7700 900${(100 + i * 37 % 900).toString().padLeft(3, '0')}',
    i % 4 == 0 ? '19${60 + i % 35}-${_two(1 + i % 12)}-${_two(1 + i % 28)}' : null,
    i % 9 == 0 ? 'Met at the ${['cycling club', 'school gate', 'conference', 'wedding'][i % 4]}' : null,
    _t(2012 + i % 6, 1 + i % 12, 1 + i % 28),
  ];
}

typedef _Table = ({String kind, String sql, int rows, List<String> cols, List<Object?> Function(int) row});

final Map<String, _Table> _contactsDb = {
  'addresses': (
    kind: 'table',
    sql:
        'CREATE TABLE addresses (id INTEGER PRIMARY KEY, contact_id INTEGER REFERENCES contacts(id), street TEXT, '
        'city TEXT, postcode TEXT, country TEXT)',
    rows: 158,
    cols: ['id', 'contact_id', 'street', 'city', 'postcode', 'country'],
    row: (i) => [
      i + 1,
      1 + (i * 3) % 214,
      '${10 + i * 7 % 180} ${['High Street', 'Mill Lane', 'Quay Street', 'Church Road', 'Park Avenue'][i % 5]}',
      ['Lancaster', 'Kendal', 'Preston', 'Manchester', 'Leeds', 'York'][i % 6],
      'LA${1 + i % 9} ${i % 9}${String.fromCharCode(65 + i % 26)}${String.fromCharCode(66 + i % 24)}',
      'United Kingdom',
    ],
  ),
  'contact_groups': (
    kind: 'table',
    sql:
        'CREATE TABLE contact_groups (contact_id INTEGER REFERENCES contacts(id), group_id INTEGER REFERENCES '
        'groups(id), PRIMARY KEY (contact_id, group_id))',
    rows: 390,
    cols: ['contact_id', 'group_id'],
    row: (i) => [1 + i % 214, 1 + (i * 5) % 6],
  ),
  'contacts': (
    kind: 'table',
    sql:
        'CREATE TABLE contacts (id INTEGER PRIMARY KEY, first_name TEXT NOT NULL, last_name TEXT, email TEXT, '
        'phone TEXT, birthday TEXT, notes TEXT, created INTEGER)',
    rows: 214,
    cols: ['id', 'first_name', 'last_name', 'email', 'phone', 'birthday', 'notes', 'created'],
    row: _contact,
  ),
  'groups': (
    kind: 'table',
    sql: 'CREATE TABLE groups (id INTEGER PRIMARY KEY, name TEXT NOT NULL UNIQUE)',
    rows: _groups.length,
    cols: ['id', 'name'],
    row: (i) => [i + 1, _groups[i]],
  ),
  'v_birthdays': (
    kind: 'view',
    sql:
        'CREATE VIEW v_birthdays AS SELECT first_name, last_name, birthday FROM contacts WHERE birthday IS NOT NULL '
        'ORDER BY substr(birthday, 6)',
    rows: 54,
    cols: ['first_name', 'last_name', 'birthday'],
    row: (i) {
      final c = _contact(i * 4);
      return [c[1], c[2], c[5]];
    },
  ),
};

final Map<String, _Table> _manifestDb = {
  'Files': (
    kind: 'table',
    sql: 'CREATE TABLE Files (fileID TEXT PRIMARY KEY, domain TEXT, relativePath TEXT, flags INTEGER, file BLOB)',
    rows: 48231,
    cols: ['fileID', 'domain', 'relativePath', 'flags', 'file'],
    row: (i) => [
      (i * 2654435761 % 0xffffffffff).toRadixString(16).padLeft(10, '0') * 4,
      ['CameraRollDomain', 'HomeDomain', 'AppDomain-com.example.notes'][i % 3],
      [
        'Media/DCIM/100APPLE/IMG_${(1 + i).toString().padLeft(4, '0')}.HEIC',
        'Library/SMS/Attachments/${i % 97}/IMG_${i % 500}.jpeg',
        'Documents/note-$i.json',
      ][i % 3],
      i % 7 == 0 ? 2 : 1,
      '<blob ${300 + i % 400} B>',
    ],
  ),
  'Properties': (
    kind: 'table',
    sql: 'CREATE TABLE Properties (key TEXT PRIMARY KEY, value BLOB)',
    rows: 3,
    cols: ['key', 'value'],
    row: (i) => [
      ['salt', 'passwordHash', 'Version'][i],
      ['<blob 16 B>', '<blob 32 B>', '3.3'][i],
    ],
  ),
};

Map<String, Object?> _sqlite(DemoNode n, String? table, int offset) {
  final db = switch (n.name) {
    'contacts.sqlite' => _contactsDb,
    'Manifest.db' => _manifestDb,
    _ => _notThis('not a SQLite database'),
  };
  Object? data;
  if (table != null) {
    final t = db[table] ?? _notThis('no such table: $table');
    data = {
      'table': table,
      'columns': t.cols,
      'rows': [for (var i = offset; i < t.rows && i < offset + 200; i++) t.row(i)],
      'offset': offset,
    };
  }
  final names = db.keys.toList()..sort();
  return {
    'path': n.path,
    'tables': [
      for (final name in names) {'name': name, 'kind': db[name]!.kind, 'sql': db[name]!.sql, 'rows': db[name]!.rows},
    ],
    'data': data,
  };
}

// ---- Taxes 2019.xlsx

final Map<String, List<List<Object?>>> _taxes = {
  'Summary': [
    ['Tax year 2019–20', null],
    [null, null],
    ['Income', 41250.0],
    ['Expenses', 6312.45],
    ['Taxable profit', 34937.55],
    ['Personal allowance', 12500.0],
    ['Tax due', 4487.51],
    ['Paid on account', 4100.0],
    ['Still to pay', 387.51],
    [null, null],
    ['Prepared', '2020-03-30'],
    ['Sent to Helen', true],
  ],
  'Income': [
    ['Date', 'Client', 'Invoice', 'Amount', 'Paid'],
    for (var m = 1; m <= 12; m++)
      [
        '2019-${_two(m)}-28',
        ['Northwind Studio', 'Blue Harbour Cafe', 'Hillside School', 'Riverside Cycling Club'][m % 4],
        'invoice-2019-${_two(m)}',
        [3450.0, 2860.0, 4120.0, 3010.0][m % 4] + m * 11,
        m < 12,
      ],
    [null, 'Total', null, 41250.0, null],
  ],
  'Expenses': [
    ['Date', 'Category', 'Description', 'Amount', 'Receipt'],
    for (var i = 0; i < 38; i++)
      [
        '2019-${_two(1 + i % 12)}-${_two(3 + i * 5 % 25)}',
        ['Equipment', 'Travel', 'Software', 'Phone', 'Insurance', 'Printing'][i % 6],
        [
          'Lens (35mm f/1.8)',
          'Train to Leeds',
          'Photo editing subscription',
          'Phone contract',
          'Equipment insurance',
          'Album prints',
        ][i % 6],
        [389.0, 42.6, 11.99, 24.0, 18.5, 96.4][i % 6] + (i % 3),
        i % 7 != 4,
      ],
  ],
  'Mileage': [
    ['Date', 'From', 'To', 'Miles', 'Purpose'],
    for (var i = 0; i < 24; i++)
      [
        '2019-${_two(1 + i % 12)}-${_two(2 + i * 3 % 26)}',
        'Lancaster',
        ['Kendal', 'Preston', 'Windermere', 'Morecambe'][i % 4],
        [22.0, 24.5, 31.0, 4.2][i % 4] * 2,
        ['Wedding shoot', 'Client meeting', 'Portrait session', 'School photos'][i % 4],
      ],
  ],
  'Rates': [
    ['Mileage (first 10,000)', 0.45],
    ['Personal allowance', 12500.0],
    ['Basic rate', 0.2],
  ],
};

Map<String, Object?> _sheet(DemoNode n, String? sheet, int offset, int? limit) {
  if (n.name != 'Taxes 2019.xlsx') _notThis('not a spreadsheet BackDown can read');
  final name = sheet != null && _taxes.containsKey(sheet) ? sheet : 'Summary';
  final rows = _taxes[name]!;
  final width = rows.fold<int>(0, (a, r) => r.length > a ? r.length : a);
  final take = limit ?? 500;
  return {
    'sheets': [
      for (final s in _taxes.keys) {'name': s, 'hidden': s == 'Rates'},
    ],
    'sheet': name,
    'offset': offset,
    'rows': rows.skip(offset).take(take).toList(),
    'width': width,
    'total_rows': rows.length,
    'total_cols': width,
    'more': offset + take < rows.length,
    'cut': false,
    'early': false,
  };
}

// ---- shop-2018.sql

Map<String, Object?> _col(
  String name,
  String type, {
  bool nullable = false,
  String? dflt,
  bool pk = false,
  bool auto = false,
  List<String> extra = const [],
  String? comment,
}) => {
  'name': name,
  'type': type,
  'null': nullable && !pk,
  'default': dflt,
  'pk': pk,
  'auto': auto,
  'extra': extra,
  'comment': comment,
};

String _create(String name, List<Map<String, Object?>> columns) {
  final cols = [
    for (final c in columns)
      '  `${c['name']}` ${c['type']}${c['null'] == true ? '' : ' NOT NULL'}'
          '${c['auto'] == true ? ' AUTO_INCREMENT' : ''}${c['default'] != null ? ' DEFAULT ${c['default']}' : ''}',
  ];
  final pk = [
    for (final c in columns)
      if (c['pk'] == true) '`${c['name']}`',
  ];
  return 'CREATE TABLE `$name` (\n${cols.join(',\n')},\n  PRIMARY KEY (${pk.join(', ')})\n'
      ') ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;';
}

Map<String, Object?> _table(
  String name, {
  required List<Map<String, Object?>> columns,
  required int rows,
  required int bytes,
  List<Map<String, Object?>> indexes = const [],
  List<Map<String, Object?>> fks = const [],
  String? comment,
}) => {
  'key': 'shop.$name',
  'schema': 'shop',
  'name': name,
  'defined': true,
  'columns': columns,
  'primary_key': [
    for (final c in columns)
      if (c['pk'] == true) c['name'],
  ],
  'indexes': indexes,
  'foreign_keys': fks,
  'checks': 0,
  'rows': rows,
  'uncounted': false,
  'bytes': bytes,
  'statements': (rows / 900).ceil(),
  'copy': false,
  'engine': 'InnoDB',
  'charset': 'utf8mb4',
  'comment': comment,
  'note': null,
  'sql': _create(name, columns),
};

Map<String, Object?> _fk(String col, String ref, {String onDelete = 'CASCADE'}) => {
  'name': 'fk_${col}_$ref',
  'columns': [col],
  'ref': 'shop.$ref',
  'ref_columns': ['id'],
  'on_delete': onDelete,
  'on_update': null,
};

Map<String, Object?> _idx(String name, List<String> cols, [String kind = 'index']) => {
  'name': name,
  'columns': cols,
  'kind': kind,
};

List<Map<String, Object?>> _shopTables({required bool all}) => [
  _table(
    'admin_users',
    rows: 3,
    bytes: 612,
    columns: [
      _col('id', 'int(11)', pk: true, auto: true),
      _col('email', 'varchar(190)'),
      _col('password_hash', 'char(60)'),
      _col('last_login', 'datetime', nullable: true),
    ],
    indexes: [
      _idx('email', ['email'], 'unique'),
    ],
  ),
  _table(
    'categories',
    rows: 24,
    bytes: 2210,
    columns: [
      _col('id', 'int(11)', pk: true, auto: true),
      _col('parent_id', 'int(11)', nullable: true),
      _col('name', 'varchar(120)'),
      _col('slug', 'varchar(120)'),
    ],
    indexes: [
      _idx('slug', ['slug'], 'unique'),
    ],
  ),
  _table(
    'coupons',
    rows: 57,
    bytes: 4980,
    columns: [
      _col('code', 'varchar(32)', pk: true),
      _col('percent_off', 'tinyint(3) unsigned', dflt: '0'),
      _col('valid_until', 'date', nullable: true),
      _col('uses', 'int(11)', dflt: '0'),
    ],
  ),
  _table(
    'customers',
    rows: 1842,
    bytes: 421888,
    columns: [
      _col('id', 'int(11)', pk: true, auto: true),
      _col('email', 'varchar(190)'),
      _col('name', 'varchar(160)'),
      _col('phone', 'varchar(32)', nullable: true),
      _col('created_at', 'timestamp', dflt: 'CURRENT_TIMESTAMP'),
      _col('newsletter', 'tinyint(1)', dflt: '0', comment: 'opted in at checkout'),
    ],
    indexes: [
      _idx('email', ['email'], 'unique'),
      _idx('created', ['created_at']),
    ],
  ),
  _table(
    'order_items',
    rows: 18904,
    bytes: 2710528,
    columns: [
      _col('id', 'int(11)', pk: true, auto: true),
      _col('order_id', 'int(11)'),
      _col('product_id', 'int(11)'),
      _col('quantity', 'smallint(6)', dflt: '1'),
      _col('price', 'decimal(10,2)'),
    ],
    fks: [
      _fk('order_id', 'orders'),
      _fk('product_id', 'products', onDelete: 'RESTRICT'),
    ],
    indexes: [
      _idx('order_id', ['order_id']),
      _idx('product_id', ['product_id']),
    ],
  ),
  _table(
    'orders',
    rows: 6315,
    bytes: 1992704,
    columns: [
      _col('id', 'int(11)', pk: true, auto: true),
      _col('customer_id', 'int(11)'),
      _col('status', "enum('new','paid','shipped','refunded')", dflt: "'new'"),
      _col('total', 'decimal(10,2)'),
      _col('coupon', 'varchar(32)', nullable: true),
      _col('placed_at', 'datetime'),
      _col('updated_at', 'datetime', nullable: true, extra: ['ON UPDATE CURRENT_TIMESTAMP']),
    ],
    fks: [_fk('customer_id', 'customers', onDelete: 'RESTRICT')],
    indexes: [
      _idx('customer_id', ['customer_id']),
      _idx('placed', ['placed_at']),
    ],
  ),
  _table(
    'product_images',
    rows: 1104,
    bytes: 163840,
    columns: [
      _col('id', 'int(11)', pk: true, auto: true),
      _col('product_id', 'int(11)'),
      _col('path', 'varchar(255)'),
      _col('position', 'tinyint(4)', dflt: '0'),
    ],
    fks: [_fk('product_id', 'products')],
  ),
  _table(
    'products',
    rows: 312,
    bytes: 389120,
    columns: [
      _col('id', 'int(11)', pk: true, auto: true),
      _col('category_id', 'int(11)', nullable: true),
      _col('sku', 'varchar(40)'),
      _col('title', 'varchar(200)'),
      _col('description', 'text', nullable: true),
      _col('price', 'decimal(10,2)'),
      _col('stock', 'int(11)', dflt: '0'),
    ],
    fks: [_fk('category_id', 'categories', onDelete: 'SET NULL')],
    indexes: [
      _idx('sku', ['sku'], 'unique'),
      _idx('title', ['title', 'description'], 'fulltext'),
    ],
  ),
  _table(
    'sessions',
    rows: all ? 88120 : 31540,
    bytes: all ? 43106304 : 15204352,
    columns: [
      _col('id', 'char(64)', pk: true),
      _col('customer_id', 'int(11)', nullable: true),
      _col('data', 'mediumtext'),
      _col('expires', 'int(10) unsigned'),
    ],
    comment: 'Never cleaned up: most of these expired years ago.',
  ),
  if (all)
    _table(
      'settings',
      rows: 41,
      bytes: 3870,
      columns: [_col('name', 'varchar(64)', pk: true), _col('value', 'text', nullable: true)],
    ),
];

Map<String, Object?> _sqldump(DemoNode n, String? more) {
  if (n.name != 'shop-2018.sql') return {'format': 'binary', 'size': n.size};
  final all = more == 'demo-shop-1';
  final read = all ? n.size : (n.size * 0.45).round();
  return {
    'format': 'sql',
    'dialect': 'mysql',
    'tool': 'mysqldump',
    'compression': null,
    'facts': [
      ['Made by', 'mysqldump 10.13 (with MySQL 5.7.24)'],
      ['Server', '5.7.24-0ubuntu0.18.04.1'],
      ['Host', 'localhost'],
      ['Database', 'shop'],
      ['Dumped', '2018-12-31 03:00:01'],
      ['Character set', 'utf8mb4'],
    ],
    'databases': ['shop'],
    'schemas': ['shop'],
    'tables': _shopTables(all: all),
    'views': [
      if (all) {'schema': 'shop', 'name': 'v_order_totals', 'kind': 'view', 'on': null},
    ],
    'triggers': [
      if (all) {'schema': 'shop', 'name': 'trg_orders_updated', 'kind': 'trigger', 'on': 'shop.orders'},
    ],
    'routines': <Object?>[],
    'events': <Object?>[],
    'sequences': <Object?>[],
    'other': <Object?>[],
    'statements': all ? 4127 : 1503,
    'size': n.size,
    'read': read,
    'pos': read,
    'complete': all,
    'truncated': false,
    'cut': null,
    'notes': <String>[],
    'elapsed_ms': all ? 4630 : 2510,
    'more': all ? null : 'demo-shop-1',
  };
}
