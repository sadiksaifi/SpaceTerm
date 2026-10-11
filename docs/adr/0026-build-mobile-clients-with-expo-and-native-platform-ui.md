# Build mobile clients with Expo and native platform UI

The deferred iOS and Android SpaceTerm Clients use Expo and React Native. Their native UI uses `@expo/ui/swift-ui` on iOS and `@expo/ui/jetpack-compose` on Android, as provided by [Expo UI](https://docs.expo.dev/versions/latest/sdk/ui/). Expo and React Native share mobile application code while Expo UI exposes each platform's native UI primitives.

Mobile clients use the same [production server architecture](0025-support-desktop-clients-and-headless-servers-on-macos-and-linux.md). Terminal rendering and mobile distribution policy require decisions before mobile delivery; [ADR 0009](0009-distribute-tagged-releases-with-signed-updates.md) currently avoids Apple membership fees.
