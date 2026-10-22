import Capacitor
import UIKit
import WebKit
import os

class ViewController: CAPBridgeViewController {

    private static let log = Logger(subsystem: "net.interstellarai.musenmingle", category: "shell")

    // WKWebView holds its navigation delegate weakly.
    private var navigation: CalendarFilesAndConnectionErrors?

    override func capacitorDidLoad() {
        super.capacitorDidLoad()
        // Capacitor has no setting for it: the edge swipe goes back (and
        // forward) through the site's history, like Safari's.
        webView?.allowsBackForwardNavigationGestures = true
        if let webView = webView, let capacitor = webView.navigationDelegate {
            navigation = CalendarFilesAndConnectionErrors(capacitor: capacitor)
            webView.navigationDelegate = navigation
        }
        excludeWebsiteDataFromBackup()
    }

    /// Saved and hidden events live in the site's localStorage, which
    /// WebKit keeps under Library/WebKit; like Android's
    /// `allowBackup="false"`, they stay out of iCloud and computer backups.
    private func excludeWebsiteDataFromBackup() {
        let files = FileManager.default
        guard var dir = files.urls(for: .libraryDirectory, in: .userDomainMask).first?
            .appendingPathComponent("WebKit", isDirectory: true) else { return }
        do {
            try files.createDirectory(at: dir, withIntermediateDirectories: true)
            var values = URLResourceValues()
            values.isExcludedFromBackup = true
            try dir.setResourceValues(values)
        } catch {
            Self.log.error("Could not exclude website data from backups: \(error.localizedDescription, privacy: .public)")
        }
    }
}

/// Sits in front of Capacitor's navigation delegate (a
/// CAPBridgeViewController subclass can't replace it) and forwards it
/// everything else. Capacitor shows `server.errorPath` (the offline page)
/// for every failed load; WebKit can't show the site's `.ics` files
/// ("Add to calendar", "Download .ics") and fails those loads, so they go
/// to Safari, which offers to add them to Calendar, and only real
/// connection failures get the offline page.
private final class CalendarFilesAndConnectionErrors: NSObject, WKNavigationDelegate {

    private let capacitor: WKNavigationDelegate

    init(capacitor: WKNavigationDelegate) {
        self.capacitor = capacitor
    }

    override func responds(to aSelector: Selector!) -> Bool {
        super.responds(to: aSelector) || capacitor.responds(to: aSelector)
    }

    override func forwardingTarget(for aSelector: Selector!) -> Any? {
        capacitor.responds(to: aSelector) ? capacitor : nil
    }

    func webView(_ webView: WKWebView, decidePolicyFor navigationResponse: WKNavigationResponse,
                 decisionHandler: @escaping (WKNavigationResponsePolicy) -> Void) {
        guard navigationResponse.isForMainFrame, !navigationResponse.canShowMIMEType,
              let url = navigationResponse.response.url else {
            decisionHandler(.allow)
            return
        }
        UIApplication.shared.open(url)
        decisionHandler(.cancel)
    }

    // The force unwraps are part of the protocol declaration.
    // swiftlint:disable implicitly_unwrapped_optional
    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        if !Self.isInterruption(error) {
            capacitor.webView?(webView, didFail: navigation, withError: error)
        }
    }

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        if !Self.isInterruption(error) {
            capacitor.webView?(webView, didFailProvisionalNavigation: navigation, withError: error)
        }
    }
    // swiftlint:enable implicitly_unwrapped_optional

    /// A load stopped by the app, not by the network: another tap started a
    /// new one (cancelled), or `decidePolicyFor navigationResponse` above
    /// handed it to Safari (WebKit's "frame load interrupted", 102).
    private static func isInterruption(_ error: Error) -> Bool {
        let error = error as NSError
        return (error.domain == NSURLErrorDomain && error.code == NSURLErrorCancelled)
            || (error.domain == "WebKitErrorDomain" && error.code == 102)
    }
}
