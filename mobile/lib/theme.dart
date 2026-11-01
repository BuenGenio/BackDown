import 'package:flutter/material.dart';

/// BackDown's colours, as the web UI and backdown.io use them (ui/app.css): warm ivory and charcoal,
/// the orange mark, and green, amber and blue for good, watch and info.
@immutable
class BdColors extends ThemeExtension<BdColors> {
  const BdColors({
    required this.good,
    required this.warn,
    required this.info,
    required this.crit,
    required this.folder,
    required this.muted,
    required this.selection,
  });

  final Color good, warn, info, crit, folder, muted, selection;

  static const light = BdColors(
    good: Color(0xff2e8b57),
    warn: Color(0xffb8800e),
    info: Color(0xff4f6fb5),
    crit: Color(0xffd8432b),
    folder: Color(0xff6f93d6),
    muted: Color(0xff85818f),
    selection: Color(0xfffbe3dc),
  );

  static const dark = BdColors(
    good: Color(0xff4fb37a),
    warn: Color(0xffe0a93a),
    info: Color(0xff7c97d6),
    crit: Color(0xfff0603f),
    folder: Color(0xff6f93d6),
    muted: Color(0xff85818f),
    selection: Color(0xff4a241c),
  );

  /// A verdict's colour: "ok"/"good"/"done" green, "watch"/"warn" amber, "fail"/"crit"/"failed" red.
  Color forVerdict(String v) => switch (v) {
    'ok' || 'good' || 'done' || 'healthy' => good,
    'watch' || 'warn' || 'warning' || 'stopped' || 'due' => warn,
    'fail' || 'failing' || 'crit' || 'critical' || 'failed' || 'bad' => crit,
    'running' || 'scanning' || 'queued' || 'info' => info,
    _ => muted,
  };

  @override
  BdColors copyWith({
    Color? good,
    Color? warn,
    Color? info,
    Color? crit,
    Color? folder,
    Color? muted,
    Color? selection,
  }) => BdColors(
    good: good ?? this.good,
    warn: warn ?? this.warn,
    info: info ?? this.info,
    crit: crit ?? this.crit,
    folder: folder ?? this.folder,
    muted: muted ?? this.muted,
    selection: selection ?? this.selection,
  );

  @override
  BdColors lerp(BdColors? other, double t) {
    if (other == null) return this;
    return BdColors(
      good: Color.lerp(good, other.good, t)!,
      warn: Color.lerp(warn, other.warn, t)!,
      info: Color.lerp(info, other.info, t)!,
      crit: Color.lerp(crit, other.crit, t)!,
      folder: Color.lerp(folder, other.folder, t)!,
      muted: Color.lerp(muted, other.muted, t)!,
      selection: Color.lerp(selection, other.selection, t)!,
    );
  }
}

extension BdTheme on BuildContext {
  BdColors get bd => Theme.of(this).extension<BdColors>()!;
  ColorScheme get colors => Theme.of(this).colorScheme;
  TextTheme get text => Theme.of(this).textTheme;
}

const _lightScheme = ColorScheme(
  brightness: Brightness.light,
  primary: Color(0xffd8432b),
  onPrimary: Color(0xffffffff),
  primaryContainer: Color(0xfffbe3dc),
  onPrimaryContainer: Color(0xff5c1a0e),
  secondary: Color(0xff4f6fb5),
  onSecondary: Color(0xffffffff),
  secondaryContainer: Color(0xffe3e9f6),
  onSecondaryContainer: Color(0xff1c2c50),
  tertiary: Color(0xff2e8b57),
  onTertiary: Color(0xffffffff),
  error: Color(0xffb3261e),
  onError: Color(0xffffffff),
  errorContainer: Color(0xfff9dedc),
  onErrorContainer: Color(0xff410e0b),
  surface: Color(0xfff7f6f2),
  onSurface: Color(0xff17161b),
  onSurfaceVariant: Color(0xff4e4b57),
  surfaceContainerLowest: Color(0xffffffff),
  surfaceContainerLow: Color(0xfffbfaf7),
  surfaceContainer: Color(0xfff1efe9),
  surfaceContainerHigh: Color(0xffebe9e2),
  surfaceContainerHighest: Color(0xffe4e1d9),
  outline: Color(0xffcfcabd),
  outlineVariant: Color(0xffe2dfd6),
  inverseSurface: Color(0xff2b2a30),
  onInverseSurface: Color(0xfff1efe9),
  inversePrimary: Color(0xfff0603f),
  shadow: Color(0xff000000),
  scrim: Color(0xff000000),
);

const _darkScheme = ColorScheme(
  brightness: Brightness.dark,
  primary: Color(0xfff0603f),
  onPrimary: Color(0xff17161b),
  primaryContainer: Color(0xff4a241c),
  onPrimaryContainer: Color(0xffffdad2),
  secondary: Color(0xff7c97d6),
  onSecondary: Color(0xff17161b),
  secondaryContainer: Color(0xff2b3654),
  onSecondaryContainer: Color(0xffdbe3f8),
  tertiary: Color(0xff4fb37a),
  onTertiary: Color(0xff17161b),
  error: Color(0xffffb4ab),
  onError: Color(0xff690005),
  errorContainer: Color(0xff93000a),
  onErrorContainer: Color(0xffffdad6),
  surface: Color(0xff1b1a1f),
  onSurface: Color(0xfff1efe9),
  onSurfaceVariant: Color(0xffbbb7c4),
  surfaceContainerLowest: Color(0xff16151a),
  surfaceContainerLow: Color(0xff1f1e24),
  surfaceContainer: Color(0xff232228),
  surfaceContainerHigh: Color(0xff2a2930),
  surfaceContainerHighest: Color(0xff323138),
  outline: Color(0xff3d3a45),
  outlineVariant: Color(0xff2e2c34),
  inverseSurface: Color(0xfff1efe9),
  onInverseSurface: Color(0xff1b1a1f),
  inversePrimary: Color(0xffd8432b),
  shadow: Color(0xff000000),
  scrim: Color(0xff000000),
);

ThemeData buildTheme(Brightness brightness) {
  final scheme = brightness == Brightness.light ? _lightScheme : _darkScheme;
  final base = ThemeData(colorScheme: scheme, useMaterial3: true, brightness: brightness);
  return base.copyWith(
    scaffoldBackgroundColor: scheme.surface,
    extensions: [brightness == Brightness.light ? BdColors.light : BdColors.dark],
    appBarTheme: AppBarTheme(
      backgroundColor: scheme.surface,
      surfaceTintColor: Colors.transparent,
      scrolledUnderElevation: 0.5,
      titleTextStyle: base.textTheme.titleLarge?.copyWith(fontWeight: FontWeight.w700, color: scheme.onSurface),
    ),
    cardTheme: CardThemeData(
      color: scheme.surfaceContainerLowest,
      surfaceTintColor: Colors.transparent,
      elevation: 0,
      margin: EdgeInsets.zero,
      shape: RoundedRectangleBorder(
        borderRadius: BorderRadius.circular(14),
        side: BorderSide(color: scheme.outlineVariant),
      ),
    ),
    navigationBarTheme: NavigationBarThemeData(
      backgroundColor: scheme.surfaceContainer,
      indicatorColor: scheme.primaryContainer,
      surfaceTintColor: Colors.transparent,
      labelTextStyle: WidgetStatePropertyAll(base.textTheme.labelSmall?.copyWith(fontWeight: FontWeight.w600)),
    ),
    listTileTheme: ListTileThemeData(iconColor: scheme.onSurfaceVariant),
    dividerTheme: DividerThemeData(color: scheme.outlineVariant, space: 1, thickness: 1),
    chipTheme: base.chipTheme.copyWith(side: BorderSide(color: scheme.outlineVariant)),
    snackBarTheme: const SnackBarThemeData(behavior: SnackBarBehavior.floating),
    inputDecorationTheme: InputDecorationTheme(
      filled: true,
      fillColor: scheme.surfaceContainerLowest,
      border: OutlineInputBorder(
        borderRadius: BorderRadius.circular(12),
        borderSide: BorderSide(color: scheme.outline),
      ),
      enabledBorder: OutlineInputBorder(
        borderRadius: BorderRadius.circular(12),
        borderSide: BorderSide(color: scheme.outline),
      ),
    ),
    progressIndicatorTheme: ProgressIndicatorThemeData(
      color: scheme.primary,
      linearTrackColor: scheme.surfaceContainerHighest,
    ),
  );
}

/// The lowercase wordmark, as on backdown.io (Bricolage Grotesque ExtraBold).
class Wordmark extends StatelessWidget {
  const Wordmark({super.key, this.size = 22, this.withMark = true});
  final double size;
  final bool withMark;

  @override
  Widget build(BuildContext context) {
    final text = Text(
      'backdown',
      style: TextStyle(
        fontFamily: 'Bricolage',
        fontWeight: FontWeight.w800,
        fontSize: size,
        letterSpacing: -0.045 * size,
        color: context.colors.onSurface,
        height: 1,
      ),
    );
    if (!withMark) return text;
    return Row(
      mainAxisSize: MainAxisSize.min,
      children: [
        Image.asset('assets/icon/mark.png', height: size * 1.05, color: context.colors.primary),
        SizedBox(width: size * 0.4),
        text,
      ],
    );
  }
}
