import 'dart:async';
import 'dart:io';

import 'package:clock/clock.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:local_auth/local_auth.dart';

import '../../theme.dart';
import 'preferences.dart';

/// Whether this platform has App Lock (local_auth: Android and iOS).
bool get appLockSupported => Platform.isAndroid || Platform.isIOS;

/// Asks for the phone's fingerprint, face or screen lock. True when the person passed.
Future<bool> unlockWithDevice(String reason) async {
  if (!appLockSupported) return true;
  try {
    return await LocalAuthentication().authenticate(localizedReason: reason, persistAcrossBackgrounding: true);
  } on Object {
    return false;
  }
}

/// Covers the app until the person unlocks it, when App Lock is on: at start, and when it comes
/// back after more than a minute away.
class LockGate extends ConsumerStatefulWidget {
  const LockGate({super.key, required this.child});
  final Widget child;

  @override
  ConsumerState<LockGate> createState() => _LockGateState();
}

class _LockGateState extends ConsumerState<LockGate> with WidgetsBindingObserver {
  static const _grace = Duration(minutes: 1);
  late bool _locked = ref.read(appLockProvider) && appLockSupported;
  DateTime? _away;
  bool _asking = false;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    if (_locked) WidgetsBinding.instance.addPostFrameCallback((_) => _unlock());
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (!ref.read(appLockProvider) || !appLockSupported) return;
    if (state == AppLifecycleState.paused || state == AppLifecycleState.hidden) {
      _away ??= clock.now();
    } else if (state == AppLifecycleState.resumed) {
      final away = _away;
      _away = null;
      if (_asking) return;
      if (away != null && clock.now().difference(away) > _grace) {
        setState(() => _locked = true);
        unawaited(_unlock());
      }
    }
  }

  Future<void> _unlock() async {
    if (_asking) return;
    _asking = true;
    final ok = await unlockWithDevice('Unlock BackDown');
    _asking = false;
    if (ok && mounted) setState(() => _locked = false);
  }

  @override
  Widget build(BuildContext context) {
    if (!_locked) return widget.child;
    return Stack(
      children: [
        // keep the app's state underneath, out of sight
        Offstage(child: widget.child),
        Positioned.fill(
          child: Material(
            color: context.colors.surface,
            child: SafeArea(
              child: Center(
                child: Column(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    const Wordmark(size: 30),
                    const SizedBox(height: 28),
                    Text('BackDown is locked', style: context.text.titleMedium),
                    const SizedBox(height: 20),
                    FilledButton.icon(
                      onPressed: _unlock,
                      icon: const Icon(Icons.lock_open),
                      label: const Text('Unlock'),
                    ),
                  ],
                ),
              ),
            ),
          ),
        ),
      ],
    );
  }
}
