import Flutter
import UIKit
import UserNotifications
import flutter_local_notifications
import workmanager_apple

@main
@objc class AppDelegate: FlutterAppDelegate, FlutterImplicitEngineDelegate {
  /// The background check's BGAppRefreshTask: the same identifier is in Info.plist
  /// (BGTaskSchedulerPermittedIdentifiers) and in Dart (lib/background/background.dart).
  static let checkTask = "io.backdown.app.check"

  override func application(
    _ application: UIApplication,
    didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
  ) -> Bool {
    // iOS asks the app delegate about notification taps; FlutterAppDelegate passes them on to
    // flutter_local_notifications. It has to be set before launch finishes.
    UNUserNotificationCenter.current().delegate = self as? UNUserNotificationCenterDelegate

    // The background check runs in an engine of its own, which needs the plugins too. Set here
    // rather than in didInitializeImplicitFlutterEngine: when iOS launches the app only in the
    // background, no scene connects and that callback never comes.
    FlutterLocalNotificationsPlugin.setPluginRegistrantCallback { registry in
      GeneratedPluginRegistrant.register(with: registry)
    }
    WorkmanagerPlugin.setPluginRegistrantCallback { registry in
      GeneratedPluginRegistrant.register(with: registry)
    }

    // BGTaskScheduler only accepts launch handlers until launch finishes. iOS decides when the
    // check really runs; the Dart side skips runs that come sooner than the chosen interval.
    WorkmanagerPlugin.registerPeriodicTask(
      withIdentifier: AppDelegate.checkTask,
      earliestBeginInSeconds: NSNumber(value: 15 * 60)
    )
    WorkmanagerPlugin.registerLaunchHandlers()

    return super.application(application, didFinishLaunchingWithOptions: launchOptions)
  }

  func didInitializeImplicitFlutterEngine(_ engineBridge: FlutterImplicitEngineBridge) {
    GeneratedPluginRegistrant.register(with: engineBridge.pluginRegistry)
  }
}
