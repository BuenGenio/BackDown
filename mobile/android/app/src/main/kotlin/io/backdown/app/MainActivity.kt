package io.backdown.app

import io.flutter.embedding.android.FlutterFragmentActivity

/**
 * A FlutterFragmentActivity, not a FlutterActivity: App Lock's prompt (local_auth, on androidx.biometric's
 * BiometricPrompt) needs a FragmentActivity. It hosts the same engine in a FlutterFragment, so plugins see no
 * difference.
 */
class MainActivity : FlutterFragmentActivity()
