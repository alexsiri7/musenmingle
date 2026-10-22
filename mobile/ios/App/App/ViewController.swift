import Capacitor
import UIKit

class ViewController: CAPBridgeViewController {

    override func capacitorDidLoad() {
        super.capacitorDidLoad()
        // Capacitor has no setting for it: the edge swipe goes back (and
        // forward) through the site's history, like Safari's.
        webView?.allowsBackForwardNavigationGestures = true
        excludeWebsiteDataFromBackup()
    }

    /// Saved and hidden events live in the site's localStorage, which
    /// WebKit keeps under Library/WebKit; like Android's
    /// `allowBackup="false"`, they stay out of iCloud and computer backups.
    private func excludeWebsiteDataFromBackup() {
        let files = FileManager.default
        guard var dir = files.urls(for: .libraryDirectory, in: .userDomainMask).first?
            .appendingPathComponent("WebKit", isDirectory: true) else { return }
        try? files.createDirectory(at: dir, withIntermediateDirectories: true)
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try? dir.setResourceValues(values)
    }
}
