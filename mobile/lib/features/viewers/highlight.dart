import 'dart:typed_data';

import 'package:flutter/painting.dart';
import 'package:re_highlight/languages/bash.dart';
import 'package:re_highlight/languages/c.dart';
import 'package:re_highlight/languages/cpp.dart';
import 'package:re_highlight/languages/csharp.dart';
import 'package:re_highlight/languages/css.dart';
import 'package:re_highlight/languages/dart.dart';
import 'package:re_highlight/languages/diff.dart';
import 'package:re_highlight/languages/dockerfile.dart';
import 'package:re_highlight/languages/go.dart';
import 'package:re_highlight/languages/gradle.dart';
import 'package:re_highlight/languages/ini.dart';
import 'package:re_highlight/languages/java.dart';
import 'package:re_highlight/languages/javascript.dart';
import 'package:re_highlight/languages/json.dart';
import 'package:re_highlight/languages/kotlin.dart';
import 'package:re_highlight/languages/less.dart';
import 'package:re_highlight/languages/lua.dart';
import 'package:re_highlight/languages/makefile.dart';
import 'package:re_highlight/languages/markdown.dart';
import 'package:re_highlight/languages/objectivec.dart';
import 'package:re_highlight/languages/perl.dart';
import 'package:re_highlight/languages/php.dart';
import 'package:re_highlight/languages/powershell.dart';
import 'package:re_highlight/languages/python.dart';
import 'package:re_highlight/languages/r.dart';
import 'package:re_highlight/languages/ruby.dart';
import 'package:re_highlight/languages/rust.dart';
import 'package:re_highlight/languages/scala.dart';
import 'package:re_highlight/languages/scss.dart';
import 'package:re_highlight/languages/sql.dart';
import 'package:re_highlight/languages/swift.dart';
import 'package:re_highlight/languages/typescript.dart';
import 'package:re_highlight/languages/vbnet.dart';
import 'package:re_highlight/languages/x86asm.dart';
import 'package:re_highlight/languages/xml.dart';
import 'package:re_highlight/languages/yaml.dart';
import 'package:re_highlight/re_highlight.dart';
import 'package:re_highlight/styles/github-dark.dart';
import 'package:re_highlight/styles/github.dart';

import '../../core/kinds.dart';

/// highlight.js's language for a file, by extension (as the web UI's HLJS_LANG), and its name.
const _langOf = {
  'js': 'javascript', 'mjs': 'javascript', 'cjs': 'javascript', 'jsx': 'javascript', 'ts': 'typescript', //
  'tsx': 'typescript', 'py': 'python', 'rs': 'rust', 'go': 'go', 'c': 'c', 'h': 'c', 'cc': 'cpp', 'cpp': 'cpp',
  'hpp': 'cpp', 'java': 'java', 'kt': 'kotlin', 'swift': 'swift', 'rb': 'ruby', 'php': 'php', 'pl': 'perl',
  'lua': 'lua', 'r': 'r', 'sh': 'bash', 'bash': 'bash', 'zsh': 'bash', 'ps1': 'powershell', 'css': 'css',
  'scss': 'scss', 'less': 'less', 'html': 'xml', 'htm': 'xml', 'xml': 'xml', 'svg': 'xml', 'vue': 'xml',
  'svelte': 'xml', 'json': 'json', 'yaml': 'yaml', 'yml': 'yaml', 'toml': 'ini', 'ini': 'ini', 'cfg': 'ini',
  'conf': 'ini', 'sql': 'sql', 'md': 'markdown', 'markdown': 'markdown', 'diff': 'diff', 'patch': 'diff',
  'makefile': 'makefile', 'dockerfile': 'dockerfile', 'cs': 'csharp', 'dart': 'dart', 'scala': 'scala',
  'vb': 'vbnet', 'm': 'objectivec', 'mm': 'objectivec', 'asm': 'x86asm', 'gradle': 'gradle',
};

const _names = {
  'javascript': 'JavaScript', 'typescript': 'TypeScript', 'python': 'Python', 'rust': 'Rust', 'go': 'Go', //
  'c': 'C', 'cpp': 'C++', 'java': 'Java', 'kotlin': 'Kotlin', 'swift': 'Swift', 'ruby': 'Ruby', 'php': 'PHP',
  'perl': 'Perl', 'lua': 'Lua', 'r': 'R', 'bash': 'Shell', 'powershell': 'PowerShell', 'css': 'CSS',
  'scss': 'SCSS', 'less': 'Less', 'xml': 'HTML/XML', 'json': 'JSON', 'yaml': 'YAML', 'ini': 'INI/TOML',
  'sql': 'SQL', 'markdown': 'Markdown', 'diff': 'Diff', 'makefile': 'Makefile', 'dockerfile': 'Dockerfile',
  'csharp': 'C#', 'dart': 'Dart', 'scala': 'Scala', 'vbnet': 'Visual Basic', 'objectivec': 'Objective-C',
  'x86asm': 'Assembly', 'gradle': 'Gradle',
};

/// The highlight.js language for [name], if there's one.
String? languageOf(String name) => _langOf[extensionOf(name)];

String? languageName(String? lang) => lang == null ? null : _names[lang];

final _modes = <String, Mode>{
  'bash': langBash, 'c': langC, 'cpp': langCpp, 'csharp': langCsharp, 'css': langCss, 'dart': langDart, //
  'diff': langDiff, 'dockerfile': langDockerfile, 'go': langGo, 'gradle': langGradle, 'ini': langIni,
  'java': langJava, 'javascript': langJavascript, 'json': langJson, 'kotlin': langKotlin, 'less': langLess,
  'lua': langLua, 'makefile': langMakefile, 'markdown': langMarkdown, 'objectivec': langObjectivec,
  'perl': langPerl, 'php': langPhp, 'powershell': langPowershell, 'python': langPython, 'r': langR,
  'ruby': langRuby, 'rust': langRust, 'scala': langScala, 'scss': langScss, 'sql': langSql,
  'swift': langSwift, 'typescript': langTypescript, 'vbnet': langVbnet, 'x86asm': langX86Asm, 'xml': langXml,
  'yaml': langYaml,
};

Highlight? _hl;
Highlight get _highlighter => _hl ??= (Highlight()..registerLanguages(_modes));

/// Text cut into rows for a lazy list, with its colours as runs: [runStarts] are offsets where a
/// run begins, [runScopes] each run's colour (an index into [scopes], 0 for none). Lines longer
/// than [rowMax] are cut into several rows (a minified file is one line), numbered on the first.
class Highlighted {
  Highlighted._(this.text, this.runStarts, this.runScopes, this.scopes, this.rowStart, this.rowEnd, this.rowLine);

  final String text;
  final Int32List runStarts;
  final Uint8List runScopes;
  final List<String> scopes;
  final Int32List rowStart, rowEnd;

  /// The line number a row starts (1-based), or 0 when it continues the line above.
  final Int32List rowLine;

  int get rows => rowStart.length;
  int get lines => rowLine.isEmpty ? 0 : rowLine.reduce((a, b) => a > b ? a : b);

  static const rowMax = 1000;

  /// Colours [text] as [lang] (null: plain) and cuts it into rows. Runs on another isolate for
  /// big files.
  static Highlighted of(String text, String? lang) {
    var starts = <int>[0];
    var runs = <int>[0];
    var scopes = <String>[''];
    if (lang != null && _modes.containsKey(lang) && text.length <= 1000000) {
      try {
        final r = _highlighter.highlight(code: text, language: lang);
        final c = _Collect();
        r.render(c);
        if (c.pos == text.length && c.scopes.length < 255) {
          starts = c.starts;
          runs = c.runs;
          scopes = c.scopes;
        }
      } on Object {
        // a grammar that trips on this text: shown plain
      }
    }
    final rs = <int>[], re = <int>[], rl = <int>[];
    var line = 0, a = 0;
    while (a <= text.length) {
      var b = text.indexOf('\n', a);
      final last = b < 0;
      if (last) b = text.length;
      if (last && a == b && line > 0) break; // the newline at the end doesn't start a line
      line++;
      final end = b > a && text.codeUnitAt(b - 1) == 13 ? b - 1 : b; // CRLF
      if (end - a <= rowMax) {
        rs.add(a);
        re.add(end);
        rl.add(line);
      } else {
        for (var s = a; s < end; s += rowMax) {
          rs.add(s);
          re.add(s + rowMax < end ? s + rowMax : end);
          rl.add(s == a ? line : 0);
        }
      }
      if (last) break;
      a = b + 1;
    }
    return Highlighted._(
      text,
      Int32List.fromList(starts),
      Uint8List.fromList(runs),
      scopes,
      Int32List.fromList(rs),
      Int32List.fromList(re),
      Int32List.fromList(rl),
    );
  }

  /// Row [r]'s text as spans in [dark] or light colours.
  TextSpan row(int r, {required bool dark}) {
    final theme = dark ? githubDarkTheme : githubTheme;
    var a = rowStart[r];
    final b = rowEnd[r];
    if (a >= b) return const TextSpan(text: '');
    // the run that covers a: the last start at or before it
    var lo = 0, hi = runStarts.length - 1;
    while (lo < hi) {
      final mid = (lo + hi + 1) >> 1;
      if (runStarts[mid] <= a) {
        lo = mid;
      } else {
        hi = mid - 1;
      }
    }
    final out = <TextSpan>[];
    for (var i = lo; a < b && i < runStarts.length; i++) {
      final runEnd = i + 1 < runStarts.length ? runStarts[i + 1] : text.length;
      final e = runEnd < b ? runEnd : b;
      if (e <= a) continue;
      final scope = scopes[runScopes[i]];
      final s = scope.isEmpty ? null : theme[scope];
      out.add(TextSpan(text: text.substring(a, e).replaceAll('\t', '    '), style: s?.copyWith(backgroundColor: null)));
      a = e;
    }
    return out.length == 1 ? out.first : TextSpan(children: out);
  }

  /// The whole of row [r] as plain text.
  String rowText(int r) => text.substring(rowStart[r], rowEnd[r]);
}

/// Records the runs a highlight makes: each piece of text with the innermost scope that has a
/// colour in the theme.
class _Collect implements HighlightRenderer {
  final starts = <int>[];
  final runs = <int>[];
  final scopes = <String>[''];
  final _index = <String, int>{'': 0};
  final _stack = <String?>[];
  int pos = 0;

  @override
  void addText(String text) {
    if (text.isEmpty) return;
    var scope = '';
    for (var i = _stack.length - 1; i >= 0; i--) {
      final s = _stack[i];
      if (s != null && githubTheme.containsKey(s)) {
        scope = s;
        break;
      }
    }
    final k = _index.putIfAbsent(scope, () {
      scopes.add(scope);
      return scopes.length - 1;
    });
    if (runs.isEmpty || runs.last != k) {
      starts.add(pos);
      runs.add(k);
    }
    pos += text.length;
  }

  @override
  void openNode(DataNode node) => _stack.add(node.scope);

  @override
  void closeNode(DataNode node) {
    if (_stack.isNotEmpty) _stack.removeLast();
  }
}
