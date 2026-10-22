# The phone apps (`mobile/`)

Muse & Mingle on Android (and, with #364, iPhone) is the live website in a
[Capacitor](https://capacitorjs.com) shell: `mobile/capacitor.config.json`
points the app's WebView at `https://musenmingle.interstellarai.net`
(`server.url`), so every website change reaches the apps without an app
release. The spec is `openspec/specs/mobile-apps/spec.md`; its acceptance
tests are `tests/js/mobile_apps.test.js`.

## What the shell does

- **Full screen, no browser chrome**: the site fills a plain WebView.
- **Back**: `@capacitor/app` makes the back button/gesture go back in the
  WebView's history (on the first page it does nothing; Home leaves the
  app). Nothing may add a `backButton` listener or a back callback (in
  `mobile/` or in the site's scripts).
- **Offline**: when the site can't be reached (no connection, DNS or TLS
  failure) the app shows `mobile/www/index.html` (`server.errorPath`), a
  self-contained branded page. HTTP errors (a 404, a form's 400) show the
  site's own page: `MainActivity` stops Capacitor from sending those to the
  offline page too. It is the only file in the bundle (`webDir`); Capacitor serves just
  that page, so it inlines its CSS and icon and loads nothing.
- **Venue links leave the app**: Capacitor opens every host other than ours
  in the phone's browser. Keep `server.allowNavigation` unset (or our host
  only).
- **Location**: the manifest declares coarse and fine location; Android
  asks only when the site requests a position, i.e. when someone taps
  "Use my exact coordinates" on `/map`. Never background location.
- **Privacy**: no SDKs beyond Capacitor's own packages (the tests reject
  anything else), and `android:allowBackup="false"`, so saved and hidden
  events (the site's `localStorage`) are not copied to a cloud backup.
- **Icon and splash**: the site's mark (`src/icons/favicon.svg`): an
  adaptive-icon vector (`res/drawable/ic_launcher_foreground.xml`) and
  legacy PNGs in `res/mipmap-*`.

`mobile/android/` is the project `npx cap add android` generated, with
those changes. `npx cap sync android` copies `www/` and the config into it
and rewrites the files marked "DO NOT EDIT"; edit the rest by hand.

## Building

Needs Node 22+, JDK 21 and the Android SDK (`ANDROID_HOME`):

```bash
cd mobile
npm ci
npx cap sync android
cd android && ./gradlew assembleDebug    # app/build/outputs/apk/debug/
```

## Release builds in CI

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

## Checks only a phone can make

Before a store release (#365): the app opens from the home screen with the
icon; filters and saved events behave as on the website; back from an event
returns to the same list and scroll position; in airplane mode the app
shows the offline page; "See it on …" opens the phone's browser; refusing
location leaves everything but "Near me" working.
