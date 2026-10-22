# mobile-apps

## Purpose

Many people look for something to do while out and about, on their phone. Muse & Mingle apps for Android and iPhone let them install it from their phone's store and browse London's creative events like any other app, with the same content, filters and privacy as the website. Whether the apps are native or the website packaged as an app is an implementation choice.

## Requirements

### Requirement: Installable from the phone's store
Muse & Mingle SHALL be available as an Android app on Google Play and as an iPhone app on the Apple App Store, each under the Muse & Mingle name and icon.

#### Scenario: Android install
- **WHEN** a person searches Google Play for Muse & Mingle on an Android phone
- **THEN** they can install the app and open it from their home screen

#### Scenario: iPhone install
- **WHEN** a person searches the App Store for Muse & Mingle on an iPhone
- **THEN** they can install the app and open it from their home screen

### Requirement: Same events and features as the website
The apps SHALL show the same events as the website and let a person browse, filter, search, open an event, save events and use "Near me, right now" as they can on the website.

#### Scenario: Browsing in the app
- **WHEN** a person opens the app and filters by today and free
- **THEN** they see the same events the website shows for today and free

#### Scenario: New website feature
- **WHEN** a feature ships on the website
- **THEN** it is available in the apps without waiting for a separate app redesign

### Requirement: Feels like an app
The apps MUST open full-screen without browser controls, follow the phone's back gesture or button, and show a clear message rather than a browser error page when there is no connection.

#### Scenario: No connection
- **WHEN** a person opens the app with no internet connection
- **THEN** the app shows a Muse & Mingle message saying it is offline, not a browser error page

#### Scenario: Back gesture
- **WHEN** a person opens an event and uses the back gesture
- **THEN** they return to the list they came from

### Requirement: Venue links leave the app
Links to a venue's or source's own page SHALL open in the phone's browser, so traffic goes to the venue as on the website.

#### Scenario: Going to the venue
- **WHEN** a person taps the link to an event's source page
- **THEN** the venue's page opens in the phone's browser

### Requirement: Same privacy as the website
The apps MUST NOT require an account, include third-party analytics, advertising or tracking, or send saved events off the device; saved and hidden events stay on the phone.

#### Scenario: Saving in the app
- **WHEN** a person saves an event in the app
- **THEN** the save is kept on the phone and nothing about it is sent to Muse & Mingle or anyone else

#### Scenario: Store privacy label
- **WHEN** a person reads the app's privacy details in either store
- **THEN** they state that no data is collected for tracking

### Requirement: Location only when asked
The apps SHALL ask for location permission only when the person uses a feature that needs it, such as "Near me, right now", and MUST keep working fully, apart from that feature, if permission is refused.

#### Scenario: Location refused
- **WHEN** a person refuses location permission
- **THEN** they can still browse, filter and save events, and "Near me" explains that it needs location
