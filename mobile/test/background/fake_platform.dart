import 'package:backdown_mobile/background/platform.dart';

/// A phone that can run background checks, recording what the app asks of it.
class FakeBackground implements BackgroundPlatform {
  FakeBackground({this.grant = true, this.ios = false, this.notificationsAllowed = true});

  /// What the person answers when asked for the permission.
  bool grant;
  final bool ios;
  bool notificationsAllowed;

  int asked = 0;
  int tests = 0;
  final List<int> schedules = [];

  @override
  bool get supported => true;
  @override
  bool get isIos => ios;
  @override
  Future<bool> allowed() async => notificationsAllowed;
  @override
  Future<bool> requestPermission() async {
    asked++;
    notificationsAllowed = grant;
    return grant;
  }

  @override
  Future<void> openSettings() async {}
  @override
  Future<void> schedule(int everyMinutes) async => schedules.add(everyMinutes);
  @override
  Future<void> showTest() async => tests++;
}
