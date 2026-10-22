# The phone apps (`mobile/`)

Muse & Mingle on Android and iPhone is the live website in a
[Capacitor](https://capacitorjs.com) shell: `mobile/capacitor.config.json`
points the app's WebView at `https://musenmingle.interstellarai.net`
(`server.url`), so every website change reaches the apps without an app
release. The spec is `openspec/specs/mobile-apps/spec.md`; its acceptance
tests are `tests/js/mobile_apps.test.js`.

## What the shell does

- **Full screen, no browser chrome**: the site fills a plain WebView. On
  iPhone it is inset below the status bar and notch (`ios.contentInset`:
  `automatic`), since the site doesn't lay itself out around them.
- **Back**: `@capacitor/app` makes the back button/gesture go back in the
  WebView's history (on the first page it does nothing; Home leaves the
  app). Nothing may add a `backButton` listener or a back callback (in
  `mobile/` or in the site's scripts). On iPhone, `ViewController.swift`
  turns on WebKit's edge swipe back (and forward) through that history.
- **Offline**: when the site can't be reached (no connection, DNS or TLS
  failure) the app shows `mobile/www/index.html` (`server.errorPath`), a
  self-contained branded page. HTTP errors (a 404, a form's 400) show the
  site's own page: on Android `MainActivity` stops Capacitor from sending
  those to the offline page too (iOS only does so for failed loads). It is the only file in the bundle (`webDir`); Capacitor serves just
  that page, so it inlines its CSS and icon and loads nothing.
- **Venue links leave the app**: Capacitor opens every host other than ours
  in the phone's browser. Keep `server.allowNavigation` unset (or our host
  only).
- **Location**: the Android manifest declares coarse and fine location,
  and the iOS `Info.plist` has the "when in use" usage string
  (`NSLocationWhenInUseUsageDescription`); each phone asks only when the
  site requests a position, i.e. when someone taps "Use my exact
  coordinates" on `/map`. Never background location.
- **Privacy**: no SDKs beyond Capacitor's own packages (the tests reject
  anything else). Saved and hidden events (the site's `localStorage`) are
  not copied to a backup: `android:allowBackup="false"` on Android, and on
  iPhone `ViewController.swift` excludes WebKit's data (`Library/WebKit`)
  from iCloud and computer backups. The iOS privacy manifest
  (`PrivacyInfo.xcprivacy`) declares no tracking and no collected data.
- **Icon and splash**: the site's mark (`src/icons/favicon.svg`): an
  adaptive-icon vector (`res/drawable/ic_launcher_foreground.xml`) and
  legacy PNGs in `res/mipmap-*` on Android; on iOS the 1024 px
  `AppIcon.appiconset` PNG and the `Splash.imageset` launch images, drawn
  from the same SVG geometry.

`mobile/android/` and `mobile/ios/` are the projects `npx cap add android`
and `npx cap add ios` generated, with those changes (iOS uses Swift Package
Manager, no CocoaPods). `npx cap sync` copies `www/` and the config into
them and rewrites the files marked "DO NOT MODIFY"/"DO NOT EDIT"; edit the
rest by hand.

## Building

Needs Node 22+, JDK 21 and the Android SDK (`ANDROID_HOME`):

```bash
cd mobile
npm ci
npx cap sync android
cd android && ./gradlew assembleDebug    # app/build/outputs/apk/debug/
```

The iPhone app needs a Mac with Xcode 26+:

```bash
cd mobile
npm ci
npx cap sync ios
npx cap open ios                         # then run the App scheme
```

## Release builds in CI

### Android

`.github/workflows/android.yml` runs on changes under `mobile/` and by hand
(*Run workflow*). It builds `bundleRelease` with `versionCode` = the run
number and uploads the AAB as an artifact. Pull requests build it unsigned
when the Doppler token isn't available; on `main` and manual runs a missing
token fails the job.

The upload key lives in Doppler. The workflow reads it with the repository
secret `DOPPLER_TOKEN`, a Doppler service token for the config holding:

| Doppler secret | Value |
|---|---|
| `ANDROID_KEYSTORE_BASE64` | the upload keystore, `base64 -w0 upload.jks` |
| `ANDROID_KEYSTORE_PASSWORD` | the keystore's password |
| `ANDROID_KEY_ALIAS` | the key's alias |
| `ANDROID_KEY_PASSWORD` | the key's password |

Make the keystore once, keep it out of the repository, and enrol it as the
upload key in Google Play (Play App Signing):

```bash
keytool -genkeypair -keystore upload.jks -alias upload -keyalg RSA -keysize 4096 -validity 10000
```

### iPhone

`.github/workflows/ios.yml` runs on the same triggers on a macOS runner. It
archives the app with the build number = the run number and uploads the
IPA as an artifact. Pull requests build it unsigned when the signing
credentials aren't available; on `main` and manual runs their absence fails
the job.

It reads the credentials from Doppler through the same `DOPPLER_TOKEN`
repository secret (its config then holds both platforms' secrets):

| Doppler secret | Value |
|---|---|
| `IOS_CERTIFICATE_BASE64` | the Apple Distribution certificate with its private key, `base64 -i distribution.p12` |
| `IOS_CERTIFICATE_PASSWORD` | the `.p12`'s export password |
| `IOS_PROVISIONING_PROFILE_BASE64` | an App Store provisioning profile for `net.interstellarai.musenmingle`, `base64 -i profile.mobileprovision` |

Both come from the Apple Developer account (Certificates, Identifiers &
Profiles): create the App ID `net.interstellarai.musenmingle`, an Apple
Distribution certificate (export it from Keychain Access as `.p12`), and an
App Store Connect provisioning profile for that App ID and certificate. The
workflow takes the team ID and profile name from the profile. Renew the
profile and certificate before they expire (yearly).

## Checks only a phone can make

Before a store release (#365), on both phones: the app opens from the home
screen with the icon; filters and saved events behave as on the website;
back from an event (the back gesture; on iPhone the swipe from the left
edge) returns to the same list and scroll position; in airplane mode the
app shows the offline page; "See it on …" opens the phone's browser
(Safari on iPhone); refusing location leaves everything but "Near me"
working; on iPhone the page starts below the status bar, and a device
backup leaves `Library/WebKit` out.
