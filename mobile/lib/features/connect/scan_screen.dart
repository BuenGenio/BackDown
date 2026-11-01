import 'dart:io';

import 'package:flutter/material.dart';
import 'package:flutter_zxing/flutter_zxing.dart';
import 'package:go_router/go_router.dart';

import '../../core/widgets.dart';
import '../../router.dart';
import '../../servers/server.dart';
import '../../theme.dart';

/// Scans the QR code `backdown token --qr` shows, then hands the link to the link screen to check
/// and name it.
class ScanScreen extends StatefulWidget {
  const ScanScreen({super.key, this.serverId});
  final String? serverId;

  @override
  State<ScanScreen> createState() => _ScanScreenState();
}

class _ScanScreenState extends State<ScanScreen> {
  bool _done = false;
  String? _notOurs;

  void _onScan(Code code) {
    if (_done || !code.isValid || code.text == null) return;
    final link = parseServerLink(code.text!);
    if (link == null) {
      setState(() => _notOurs = 'That code isn\'t a BackDown link.');
      return;
    }
    _done = true;
    final text = '${link.url}/?t=${link.token}';
    context.pushReplacement(
      Uri(path: Routes.connectLink, queryParameters: {'link': text, 'server': ?widget.serverId}).toString(),
    );
  }

  @override
  Widget build(BuildContext context) {
    if (!(Platform.isAndroid || Platform.isIOS)) {
      return Scaffold(
        appBar: AppBar(title: const Text('Scan the code')),
        body: EmptyState(
          icon: Icons.qr_code,
          title: 'No camera here',
          message: 'Paste the link instead.',
          action: FilledButton(
            onPressed: () => context.pushReplacement(Routes.connectLink),
            child: const Text('Paste the link'),
          ),
        ),
      );
    }
    return Scaffold(
      appBar: AppBar(title: const Text('Scan the code')),
      body: Stack(
        children: [
          ReaderWidget(
            onScan: _onScan,
            codeFormat: Format.qrCode,
            showGallery: false,
            showToggleCamera: false,
            cropPercent: 0.7,
            resolution: ResolutionPreset.high,
          ),
          Positioned(
            left: 16,
            right: 16,
            bottom: 24,
            child: Card(
              child: Padding(
                padding: const EdgeInsets.all(14),
                child: Column(
                  mainAxisSize: MainAxisSize.min,
                  children: [
                    Text(
                      _notOurs ?? 'Point the camera at the code `backdown token --qr` shows on the computer.',
                      textAlign: TextAlign.center,
                      style: context.text.bodyMedium,
                    ),
                    TextButton(
                      onPressed: () => context.pushReplacement(Routes.connectLink),
                      child: const Text('Paste the link instead'),
                    ),
                  ],
                ),
              ),
            ),
          ),
        ],
      ),
    );
  }
}
